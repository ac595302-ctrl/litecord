//! `litecord` — headless shell over the Litecord foundation.
//!
//! * `litecord demo`   — start the app on the synthetic demo backend, hydrate,
//!   print view-model summaries and a compiled context pack.
//! * `litecord mcp`    — MCP server on stdio over the local database (point
//!   Codex/OpenCode at this). It can read memory, write local items and
//!   *propose* Discord actions; the app executes them after user approval.
//! * `litecord status` — revision and counts of the local database.
//!
//! Configuration precedence: defaults < `--config` file < `LITECORD_*` env <
//! flags. Logs go to stderr (stdout is reserved for MCP / JSON output).

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};

use litecord_app::{standalone_gateway, LitecordApp};
use litecord_core::config::{BackendKind, ConfigLoader, ConfigOverrides, LitecordConfig};
use litecord_mcp::McpServer;
use litecord_store::{repos, Database};

#[derive(Debug, Parser)]
#[command(
    name = "litecord",
    version,
    about = "Lightweight Discord client foundation with unified memory"
)]
struct Cli {
    /// TOML configuration file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Data directory (database, caches).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
    /// Database file (overrides data_dir/litecord.db).
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    /// Backend selection.
    #[arg(long, global = true, value_enum)]
    backend: Option<BackendArg>,
    /// tracing filter, e.g. `info,litecord_store=debug`.
    #[arg(long, global = true)]
    log: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BackendArg {
    Demo,
    SocialSdk,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Open the native workspace (demo backend by default).
    #[cfg(feature = "gui")]
    Gui {
        #[cfg(feature = "screenshots")]
        #[arg(long, hide = true)]
        screenshot: Option<PathBuf>,
        #[cfg(feature = "screenshots")]
        #[arg(long, hide = true, default_value = "Messages")]
        screen: String,
        #[cfg(feature = "screenshots")]
        #[arg(long, hide = true, default_value_t = 1586.0)]
        width: f32,
    },
    /// Run the demo backend end to end and print summaries.
    Demo {
        /// Use a throwaway in-memory database.
        #[arg(long)]
        in_memory: bool,
        /// Question to compile a context pack for.
        #[arg(long, default_value = "What did I miss? Who should I reply to?")]
        ask: String,
    },
    /// Serve MCP over stdio.
    Mcp,
    /// Print database status.
    Status,
}

fn load_config(cli: &Cli) -> litecord_core::Result<LitecordConfig> {
    let mut loader = ConfigLoader::new()
        .env(std::env::vars())
        .overrides(ConfigOverrides {
            data_dir: cli.data_dir.clone(),
            database_path: cli.db.clone(),
            backend: cli.backend.map(|b| match b {
                BackendArg::Demo => BackendKind::Demo,
                BackendArg::SocialSdk => BackendKind::SocialSdk,
            }),
            log_filter: cli.log.clone(),
        });
    if let Some(path) = &cli.config {
        loader = loader.file(path);
    }
    loader.load()
}

fn init_tracing(filter: &str) {
    let filter = tracing_subscriber::EnvFilter::try_new(filter)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn print_json(v: &impl serde::Serialize) {
    match serde_json::to_string_pretty(v) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("serialization failed: {e}"),
    }
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let cfg = match load_config(&cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("litecord: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    init_tracing(&cfg.logging.filter);
    let result = match cli.command {
        #[cfg(feature = "gui")]
        Command::Gui {
            #[cfg(feature = "screenshots")]
            screenshot,
            #[cfg(feature = "screenshots")]
            screen,
            #[cfg(feature = "screenshots")]
            width,
        } => {
            #[cfg(not(feature = "screenshots"))]
            let options = litecord_ui::WindowOptions::default();
            #[cfg(feature = "screenshots")]
            let options = litecord_ui::WindowOptions {
                screenshot,
                destination: litecord_layout_destination(&screen),
                size: Some([width, 992.0]),
            };
            gui(cfg, options).await
        }
        Command::Demo { in_memory, ask } => demo(cfg, in_memory, &ask).await,
        Command::Mcp => mcp(cfg).await,
        Command::Status => status(&cfg),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("litecord: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(feature = "gui")]
async fn gui(
    cfg: LitecordConfig,
    options: litecord_ui::WindowOptions,
) -> litecord_core::Result<()> {
    let app = LitecordApp::builder(cfg).start().await?;
    let result =
        litecord_ui::run_with_options(app.clone(), tokio::runtime::Handle::current(), options);
    let report = app.shutdown().await;
    tracing::info!(?report, "GUI shutdown complete");
    result.map_err(|e| litecord_core::Error::internal(format!("native window: {e}")))
}

#[cfg(feature = "screenshots")]
fn litecord_layout_destination(name: &str) -> Option<litecord_ui::Destination> {
    litecord_ui::Destination::ALL
        .into_iter()
        .find(|d| d.label().eq_ignore_ascii_case(name))
}

async fn demo(cfg: LitecordConfig, in_memory: bool, ask: &str) -> litecord_core::Result<()> {
    let mut builder = LitecordApp::builder(cfg);
    if in_memory {
        builder = builder.in_memory();
    }
    let app = builder.start().await?;
    // Wait (bounded) for initial hydration to settle.
    for _ in 0..100 {
        let d = app.diagnostics_view()?;
        if d.hydration_pending == 0 && d.hydration_active == 0 && d.counts.users > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    if let Some(c) = app.conversations_view(1)?.conversations.first() {
        // Opening a conversation hydrates its recent history.
        app.conversation_view(c.conversation_id, 20, None)?;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let friends = app.friends_view()?;
    println!("== DEMO DATA IS SYNTHETIC ==");
    println!(
        "friends: {} online, {} offline, {} pending, {} blocked",
        friends.online.len(),
        friends.offline.len(),
        friends.pending_incoming.len() + friends.pending_outgoing.len(),
        friends.blocked.len()
    );
    for c in app.conversations_view(10)?.conversations {
        println!(
            "  {:<24} {}{}",
            c.title,
            if c.awaiting_reply {
                "[needs reply] "
            } else {
                ""
            },
            c.last_message_preview.unwrap_or_default()
        );
    }
    let inbox = app.agent_inbox_view()?;
    println!(
        "agent inbox: {} items needing attention",
        inbox.needs_attention.len()
    );
    let gateway = app.agent_gateway();
    let pack = gateway
        .call_tool(
            "compile_context",
            serde_json::json!({ "instruction": ask, "max_tokens": 1500 }),
            &litecord_agent::Caller::new("cli-demo"),
        )
        .await?;
    println!("== context pack for: {ask} ==");
    print_json(&pack["stats"]);
    let d = app.diagnostics_view()?;
    println!("== diagnostics ==");
    print_json(&serde_json::json!({
        "revision": d.revision,
        "session": d.session,
        "counts": d.counts,
        "rss_bytes": d.metrics.rss_bytes,
        "events_ingested": d.metrics.events_ingested,
        "db_write": d.metrics.db_write,
        "context_compile": d.metrics.context_compile,
    }));
    let report = app.shutdown().await;
    tracing::info!(?report, "shutdown complete");
    Ok(())
}

async fn mcp(cfg: LitecordConfig) -> litecord_core::Result<()> {
    let db = Database::open(cfg.database_path(), &cfg.database)?;
    let gateway = standalone_gateway(db, &cfg)?;
    let mut server = McpServer::new(gateway, "mcp");
    tracing::info!(db = %cfg.database_path().display(), "MCP server on stdio");
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    litecord_mcp::serve(&mut server, stdin, tokio::io::stdout())
        .await
        .map_err(|e| litecord_core::Error::internal(format!("stdio: {e}")))
}

fn status(cfg: &LitecordConfig) -> litecord_core::Result<()> {
    let db = Database::open(cfg.database_path(), &cfg.database)?;
    let v = db.read(|r| -> litecord_core::Result<serde_json::Value> {
        Ok(serde_json::json!({
            "database": cfg.database_path(),
            "revision": r.revision(),
            "users": repos::users::count(r)?,
            "messages": repos::messages::count(r, None)?,
            "memories_by_status": repos::memory::count_by_status(r)?,
        }))
    })?;
    print_json(&v);
    Ok(())
}
