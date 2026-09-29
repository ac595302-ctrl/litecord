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
    command: Option<Command>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BackendArg {
    Demo,
    SocialSdk,
    UserSession,
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
        /// Start with the Omni panel open.
        #[arg(long)]
        omni: bool,
        /// Screenshot aid: send this to Omni at startup.
        #[cfg(feature = "screenshots")]
        #[arg(long, hide = true)]
        omni_ask: Option<String>,
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
    Mcp {
        /// Label recorded as the acting harness in the audit log.
        #[arg(long, default_value = "mcp")]
        harness: String,
    },
    /// Print database status.
    Status,
    /// Omni (your Codex/OpenCode harness): sign-in, models, automations.
    Omni {
        #[command(subcommand)]
        cmd: OmniCmd,
    },
}

#[derive(Debug, Subcommand)]
enum OmniCmd {
    /// Harness, sign-in state, model and automations.
    Status,
    /// Choose the harness: `codex` or `opencode`.
    Harness {
        kind: String,
    },
    /// Sign in. Without `--method`, lists the methods the harness offers.
    Login {
        /// A method id from the list (browser or API key).
        #[arg(long)]
        method: Option<String>,
        /// Read an API key from stdin (pipe it; it is never echoed or stored
        /// by Litecord).
        #[arg(long)]
        api_key_stdin: bool,
    },
    Logout,
    /// List models; `--set <model>` or `--default` to choose.
    Models {
        #[arg(long)]
        set: Option<String>,
        #[arg(long)]
        default: bool,
    },
    /// List automations.
    Automations,
    /// Run an automation now.
    Run {
        id: i64,
    },
    /// Check installed harnesses against what Litecord expects.
    Doctor,
}

fn load_config(cli: &Cli) -> litecord_core::Result<LitecordConfig> {
    load_config_from(cli, std::env::vars().collect())
}

fn load_config_from(
    cli: &Cli,
    env: Vec<(String, String)>,
) -> litecord_core::Result<LitecordConfig> {
    let has_env = |key: &str| env.iter().any(|(k, _)| k == key);
    // An account build opens the account for every command except `demo`,
    // so the window, `status`, `omni` and `mcp` all see the same database.
    let account_build = cfg!(feature = "discord-user-session");
    let demo_command = matches!(cli.command, Some(Command::Demo { .. }));
    let backend = cli
        .backend
        .map(|b| match b {
            BackendArg::Demo => BackendKind::Demo,
            BackendArg::SocialSdk => BackendKind::SocialSdk,
            BackendArg::UserSession => BackendKind::UserSession,
        })
        .or_else(|| {
            (account_build && !demo_command && cli.config.is_none() && !has_env("LITECORD_BACKEND"))
                .then_some(BackendKind::UserSession)
        });
    let explicit_dir = cli.data_dir.is_some() || has_env("LITECORD_DATA_DIR");
    let base = app_data_base(&env);
    let mut loader = ConfigLoader::new().env(env).overrides(ConfigOverrides {
        data_dir: cli.data_dir.clone(),
        database_path: cli.db.clone(),
        backend,
        log_filter: cli.log.clone(),
    });
    if let Some(path) = &cli.config {
        loader = loader.file(path);
    }
    let mut cfg = loader.load()?;
    cfg.data_dir = anchor_data_dir(&cfg, cli.config.as_deref(), explicit_dir, base);
    if default_account_folder(cli, &cfg, explicit_dir) {
        // The default account folder may hold one sub-folder per account.
        cfg.data_dir = litecord_app::account_slots::resolve(&cfg.data_dir);
    }
    if let Some(db) = &cli.db {
        cfg.database.path = Some(absolute_or_same(db));
    }
    Ok(cfg)
}

/// Whether the account data folder is the built-in one, where several
/// accounts can be kept and switched between (see `account_slots`).
fn default_account_folder(cli: &Cli, cfg: &LitecordConfig, explicit_dir: bool) -> bool {
    cfg.backend.kind == BackendKind::UserSession
        && !explicit_dir
        && cli.config.is_none()
        && cli.db.is_none()
}

/// Where the data directory lives. Relative paths used to resolve against
/// the working directory, which differs between Finder, Explorer, a
/// terminal and a shortcut, so each launch could open a different database
/// (and, with it, a different saved sign-in). Now:
/// * the built-in default is a per-OS app-data folder, split by mode;
/// * a relative path from `--data-dir` or `LITECORD_DATA_DIR` is relative
///   to the working directory the user typed it in;
/// * a relative path from a config file is relative to that file.
fn anchor_data_dir(
    cfg: &LitecordConfig,
    config_file: Option<&std::path::Path>,
    explicit: bool,
    base: Option<PathBuf>,
) -> PathBuf {
    let dir = &cfg.data_dir;
    if dir.is_absolute() {
        return dir.clone();
    }
    if explicit {
        return absolute_or_same(dir);
    }
    if let Some(file) = config_file {
        if *dir != LitecordConfig::default().data_dir {
            let parent = absolute_or_same(file)
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default();
            return parent.join(dir);
        }
    }
    let mode = match cfg.backend.kind {
        BackendKind::UserSession => "account",
        BackendKind::Demo | BackendKind::SocialSdk => "demo",
    };
    match base {
        Some(base) => base.join(mode),
        None => absolute_or_same(dir),
    }
}

fn absolute_or_same(path: &std::path::Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Per-OS folder that holds Litecord's data directories.
fn app_data_base(env: &[(String, String)]) -> Option<PathBuf> {
    let value = |key: &str| {
        env.iter()
            .find(|(k, v)| k == key && !v.is_empty())
            .map(|(_, v)| PathBuf::from(v))
    };
    if cfg!(target_os = "windows") {
        value("LOCALAPPDATA")
            .or_else(|| value("HOME"))
            .map(|p| p.join("Litecord"))
    } else if cfg!(target_os = "macos") {
        value("HOME").map(|p| p.join("Library/Application Support/Litecord"))
    } else {
        value("XDG_DATA_HOME")
            .or_else(|| value("HOME").map(|p| p.join(".local/share")))
            .map(|p| p.join("litecord"))
    }
}

fn init_tracing(filter: &str, log_file: Option<&std::path::Path>) {
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    let filter = tracing_subscriber::EnvFilter::try_new(filter)
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let file = log_file.and_then(open_log_file);
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    let _ = match file {
        Some(file) => builder
            .with_ansi(false)
            .with_writer(std::io::stderr.and(std::sync::Mutex::new(file)))
            .try_init(),
        None => builder.with_writer(std::io::stderr).try_init(),
    };
    // Panics otherwise vanish when there is no terminal (Finder, Explorer).
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "Litecord hit an internal error");
        default_hook(info);
    }));
}

/// `<data dir>/litecord.log`, started fresh once it passes 5 MB.
fn open_log_file(path: &std::path::Path) -> Option<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let too_big = std::fs::metadata(path).is_ok_and(|m| m.len() > 5 * 1024 * 1024);
    std::fs::OpenOptions::new()
        .create(true)
        .append(!too_big)
        .write(true)
        .truncate(too_big)
        .open(path)
        .ok()
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
    let log_file = cfg.data_dir.join("litecord.log");
    // `mcp` runs beside the window as Omni's tool server; keep its logs on
    // stderr so the two processes don't interleave one file.
    let file_logging = !matches!(cli.command, Some(Command::Mcp { .. }));
    init_tracing(
        &cfg.logging.filter,
        file_logging.then_some(log_file.as_path()),
    );
    #[cfg(feature = "gui")]
    let account_slots = {
        let explicit = cli.data_dir.is_some() || std::env::var_os("LITECORD_DATA_DIR").is_some();
        let env: Vec<(String, String)> = std::env::vars().collect();
        default_account_folder(&cli, &cfg, explicit)
            .then(|| app_data_base(&env).map(|base| base.join("account")))
            .flatten()
            .map(|root| litecord_app::account_slots::AccountSlots::new(root, cfg.data_dir.clone()))
    };
    let command = match cli.command {
        Some(command) => command,
        #[cfg(feature = "gui")]
        None => Command::Gui {
            #[cfg(feature = "screenshots")]
            screenshot: None,
            #[cfg(feature = "screenshots")]
            screen: "Settings".into(),
            #[cfg(feature = "screenshots")]
            width: 1586.0,
            omni: false,
            #[cfg(feature = "screenshots")]
            omni_ask: None,
        },
        #[cfg(not(feature = "gui"))]
        None => {
            use clap::CommandFactory;
            let _ = Cli::command().print_help();
            return std::process::ExitCode::SUCCESS;
        }
    };
    let result = match command {
        #[cfg(feature = "gui")]
        Command::Gui {
            #[cfg(feature = "screenshots")]
            screenshot,
            #[cfg(feature = "screenshots")]
            screen,
            #[cfg(feature = "screenshots")]
            width,
            omni,
            #[cfg(feature = "screenshots")]
            omni_ask,
        } => {
            #[cfg(not(feature = "screenshots"))]
            let options = litecord_ui::WindowOptions {
                omni_open: omni,
                account_slots,
                ..Default::default()
            };
            #[cfg(feature = "screenshots")]
            let options = litecord_ui::WindowOptions {
                screenshot,
                destination: litecord_layout_destination(&screen),
                size: Some([width, 992.0]),
                omni_open: omni,
                account_slots,
            };
            #[cfg(not(feature = "screenshots"))]
            let omni_ask: Option<String> = None;
            gui(cfg, options, omni_ask).await
        }
        Command::Demo { in_memory, ask } => demo(cfg, in_memory, &ask).await,
        Command::Mcp { harness } => mcp(cfg, &harness).await,
        Command::Status => status(&cfg),
        Command::Omni { cmd } => omni_cli(cfg, cmd).await,
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
    mut options: litecord_ui::WindowOptions,
    omni_ask: Option<String>,
) -> litecord_core::Result<()> {
    let started = async {
        with_omni(
            with_account(with_bot(LitecordApp::builder(cfg.clone()))?, &cfg)?,
            &cfg,
        )
        .start()
        .await
    }
    .await;
    let app = match started {
        Ok(app) => app,
        Err(e) => {
            tracing::error!(error = %e, "startup failed");
            let log = cfg.data_dir.join("litecord.log");
            let _ = litecord_ui::show_startup_error(&e.to_string(), Some(&log));
            return Err(e);
        }
    };
    // Open Settings when there is no account to show yet. The session is
    // still connecting at this point on every launch, so its state alone
    // would send a signed-in user to Settings each time.
    if options.destination.is_none()
        && cfg.backend.kind == BackendKind::UserSession
        && app.account_view()?.user_id.is_none()
    {
        options.destination = Some(litecord_ui::Destination::Settings);
    }
    if let Some(text) = omni_ask {
        let a = app.clone();
        tokio::spawn(async move {
            if let Err(e) = a.omni().send(None, &text).await {
                tracing::warn!(error = %e, "omni-ask failed");
            }
        });
    }
    let slots = options.account_slots.clone();
    let result =
        litecord_ui::run_with_options(app.clone(), tokio::runtime::Handle::current(), options);
    let report = app.shutdown().await;
    tracing::info!(?report, "GUI shutdown complete");
    // An account switch closes the window; start again on the new folder
    // (only now: this process held the data folder's lock until shutdown).
    if slots.is_some_and(|s| s.restart_requested()) {
        match std::env::current_exe() {
            Ok(exe) => {
                if let Err(e) = std::process::Command::new(exe)
                    .args(std::env::args_os().skip(1))
                    .spawn()
                {
                    tracing::error!(error = %e, "could not restart after switching accounts");
                }
            }
            Err(e) => tracing::error!(error = %e, "could not find Litecord to restart"),
        }
    }
    result.map_err(|e| litecord_core::Error::internal(format!("native window: {e}")))
}

#[cfg(feature = "screenshots")]
fn litecord_layout_destination(name: &str) -> Option<litecord_ui::Destination> {
    if name.eq_ignore_ascii_case("memory") {
        return Some(litecord_ui::Destination::Omni);
    }
    litecord_ui::Destination::ALL
        .into_iter()
        .find(|d| d.label().eq_ignore_ascii_case(name))
}

async fn demo(cfg: LitecordConfig, in_memory: bool, ask: &str) -> litecord_core::Result<()> {
    let mut builder = with_account(with_bot(LitecordApp::builder(cfg.clone()))?, &cfg)?;
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

async fn omni_cli(cfg: LitecordConfig, cmd: OmniCmd) -> litecord_core::Result<()> {
    use litecord_app::harness::{HarnessKind, LoginKind, LoginState};
    use litecord_core::error::{Error, ErrorKind};
    let app = with_omni(
        with_account(with_bot(LitecordApp::builder(cfg.clone()))?, &cfg)?,
        &cfg,
    )
    .start()
    .await?;
    let omni = app.omni().clone();
    let result = async {
        match cmd {
            OmniCmd::Status => {
                let v = omni.view(None)?;
                print_json(&serde_json::json!({
                    "status": v.status,
                    "heartbeats": v.heartbeat_enabled,
                    "automations": v.automations,
                }));
            }
            OmniCmd::Harness { kind } => {
                let k = HarnessKind::parse(&kind)
                    .ok_or_else(|| Error::validation("harness must be codex or opencode"))?;
                omni.select(k).await?;
                println!("Omni will use {}.", k.label());
            }
            OmniCmd::Login {
                method,
                api_key_stdin,
            } => {
                let options = omni.login_options().await?;
                let Some(method) = method else {
                    for o in &options {
                        let kind = match o.kind {
                            LoginKind::Browser => "browser",
                            LoginKind::ApiKey => "api key",
                        };
                        println!("{:<24} {} ({kind})", o.id, o.label);
                    }
                    println!("\nSign in with: litecord omni login --method <id>");
                    return Ok(());
                };
                let option = options
                    .iter()
                    .find(|o| o.id == method)
                    .ok_or_else(|| Error::validation("unknown sign-in method"))?;
                if option.kind == LoginKind::ApiKey || api_key_stdin {
                    let mut key = String::new();
                    std::io::stdin()
                        .read_line(&mut key)
                        .map_err(|e| Error::internal(format!("stdin: {e}")))?;
                    omni.sign_in_api_key(&option.id, &litecord_core::secrets::Secret::new(key))
                        .await?;
                } else {
                    match omni.sign_in_with(&option.id).await? {
                        LoginState::SigningIn {
                            url,
                            instructions,
                            needs_code,
                        } => {
                            if let Some(url) = url {
                                println!("Open this page to sign in:\n  {url}");
                            }
                            if let Some(i) = instructions {
                                println!("{i}");
                            }
                            if needs_code {
                                println!("Paste the code shown in the browser, then press Enter:");
                                let mut code = String::new();
                                std::io::stdin()
                                    .read_line(&mut code)
                                    .map_err(|e| Error::internal(format!("stdin: {e}")))?;
                                omni.submit_login_code(&code).await?;
                            }
                        }
                        other => print_json(&other),
                    }
                }
                // Wait (bounded) for the harness to confirm.
                for _ in 0..600 {
                    if omni.status().login.is_ready() {
                        println!("Signed in.");
                        return Ok(());
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                return Err(Error::new(
                    ErrorKind::Authentication,
                    "sign-in did not complete",
                ));
            }
            OmniCmd::Logout => {
                omni.sign_out().await?;
                println!("Signed out.");
            }
            OmniCmd::Models { set, default } => {
                if default {
                    omni.set_model(None)?;
                } else if let Some(m) = set {
                    omni.set_model(Some(&m))?;
                }
                let models = omni.models().await?;
                let current = omni.model();
                for m in &models {
                    let mark = if Some(m) == current.as_ref() {
                        "*"
                    } else {
                        " "
                    };
                    println!("{mark} {m}");
                }
                if models.is_empty() {
                    println!("The harness did not list models.");
                }
                println!(
                    "current: {}",
                    current.as_deref().unwrap_or("harness default")
                );
            }
            OmniCmd::Automations => print_json(&omni.automations()?),
            OmniCmd::Run { id } => {
                print_json(&omni.run_automation(id).await?);
                // Let the turn finish so results are stored before exit.
                for _ in 0..240 {
                    if omni.view(None)?.sessions.iter().all(|s| !s.running) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            OmniCmd::Doctor => doctor(&cfg, &omni).await?,
        }
        Ok(())
    }
    .await;
    app.shutdown().await;
    result
}

/// Checks what Omni depends on and prints a plain report.
async fn doctor(
    cfg: &LitecordConfig,
    omni: &litecord_app::OmniService,
) -> litecord_core::Result<()> {
    let ok = |b: bool| if b { "ok  " } else { "FAIL" };
    let status = omni.status();
    println!(
        "{} Litecord database on disk: {}",
        ok(status.unavailable.is_none()),
        cfg.database_path().display()
    );
    let secret_blocked =
        !litecord_harness::env::allowed(std::ffi::OsStr::new("LITECORD_BOT_TOKEN"));
    println!(
        "{} Litecord secrets are withheld from the harness",
        ok(secret_blocked)
    );
    for h in &status.harnesses {
        println!(
            "\n{}: {}",
            h.label,
            if h.installed {
                "installed"
            } else {
                "not installed"
            }
        );
        if !h.installed {
            continue;
        }
        match h.kind {
            #[cfg(feature = "omni-codex")]
            litecord_app::harness::HarnessKind::Codex => {
                let path = cfg.omni.codex_path.as_deref();
                println!(
                    "  version: {}",
                    litecord_harness::codex::version(path)
                        .await
                        .as_deref()
                        .unwrap_or("unknown")
                );
                match litecord_harness::codex::schema_check(path).await {
                    Ok(missing) if missing.is_empty() => {
                        println!("  ok   protocol: every method Litecord uses is in this version's schema")
                    }
                    Ok(missing) => println!(
                        "  WARN protocol: not in this version's schema: {}",
                        missing.join(", ")
                    ),
                    Err(e) => println!("  WARN protocol: could not generate the schema ({e})"),
                }
            }
            #[cfg(feature = "omni-opencode")]
            litecord_app::harness::HarnessKind::OpenCode => {
                let path = cfg.omni.opencode_path.as_deref();
                println!(
                    "  version: {}",
                    litecord_harness::opencode::version(path)
                        .await
                        .as_deref()
                        .unwrap_or("unknown")
                );
                let dir =
                    std::env::temp_dir().join(format!("litecord-doctor-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&dir);
                let ctx = litecord_harness::LaunchContext {
                    workspace: dir.clone(),
                    protected_dir: cfg.data_dir.clone(),
                    mcp: litecord_harness::McpLaunch {
                        command: std::env::current_exe().unwrap_or_default(),
                        args: vec!["mcp".into()],
                    },
                };
                let launcher =
                    litecord_harness::opencode::OpenCodeLauncher::new(path.map(Into::into));
                match launcher.spawn(&ctx).await {
                    Ok(driver) => {
                        match driver.doc_check().await {
                            Ok(missing) if missing.is_empty() => {
                                println!("  ok   API: every endpoint Litecord uses is in this version's OpenAPI")
                            }
                            Ok(missing) => println!(
                                "  WARN API: not in this version's OpenAPI: {}",
                                missing.join(", ")
                            ),
                            Err(e) => println!("  WARN API: could not read /doc ({e})"),
                        }
                        use litecord_harness::HarnessDriver as _;
                        driver.shutdown().await;
                    }
                    Err(e) => println!("  FAIL could not start `opencode serve`: {e}"),
                }
                let _ = std::fs::remove_dir_all(&dir);
            }
            _ => {}
        }
    }
    if status.selected.is_some() {
        match omni.refresh_login().await {
            Ok(l) => println!(
                "\nsign-in: {}",
                serde_json::to_string(&l).unwrap_or_default()
            ),
            Err(e) => println!("\nsign-in: could not start the harness ({e})"),
        }
    }
    Ok(())
}

async fn mcp(cfg: LitecordConfig, harness: &str) -> litecord_core::Result<()> {
    let db = Database::open(cfg.database_path(), &cfg.database)?;
    let gateway = standalone_gateway(db, &cfg)?;
    let mut server = McpServer::new(gateway, harness);
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

/// Offer Omni harnesses: Codex and OpenCode when compiled in (used only if
/// installed), plus the clearly labelled demo harness on the demo backend.
/// The harness runs this executable as its MCP server.
fn with_omni(
    mut builder: litecord_app::AppBuilder,
    cfg: &LitecordConfig,
) -> litecord_app::AppBuilder {
    use std::sync::Arc;
    #[cfg(feature = "omni-codex")]
    {
        builder = builder.omni_launcher(Arc::new(litecord_harness::codex::CodexLauncher {
            path: cfg.omni.codex_path.clone(),
        }));
    }
    #[cfg(feature = "omni-opencode")]
    {
        builder = builder.omni_launcher(Arc::new(litecord_harness::opencode::OpenCodeLauncher {
            path: cfg.omni.opencode_path.clone(),
        }));
    }
    if cfg.backend.kind == BackendKind::Demo {
        builder = builder.omni_launcher(Arc::new(litecord_harness::FakeLauncher::new(
            litecord_harness::FakeDriver::demo(),
        )));
    }
    match std::env::current_exe() {
        Ok(exe) => builder.omni_mcp_command(exe),
        Err(e) => {
            tracing::warn!(error = %e, "cannot locate own executable; Omni unavailable");
            builder
        }
    }
}

/// Attach the real application-bot source when built with `discord-bot` and
/// `LITECORD_BOT_TOKEN` is set. The token is read here (the binary is the
/// only place that touches secret environment variables), wrapped in a
/// `Secret`, and handed to the adapter; it never enters configuration.
#[cfg(feature = "discord-bot")]
fn with_bot(builder: litecord_app::AppBuilder) -> litecord_core::Result<litecord_app::AppBuilder> {
    use std::sync::Arc;

    use discord_adapter::bot::{BotBackend, BotConfig, HttpTransport};
    use litecord_core::secrets::Secret;

    let Ok(token) = std::env::var("LITECORD_BOT_TOKEN") else {
        return Ok(builder);
    };
    if token.trim().is_empty() {
        return Ok(builder);
    }
    let transport = HttpTransport::new()?;
    let bot = BotBackend::new(
        Arc::new(transport),
        Secret::new(token.trim().to_owned()),
        BotConfig::default(),
        Arc::new(litecord_core::clock::SystemClock),
    );
    tracing::info!("application-bot source enabled");
    Ok(builder.bot_backend(Arc::new(bot)))
}

#[cfg(not(feature = "discord-bot"))]
fn with_bot(builder: litecord_app::AppBuilder) -> litecord_core::Result<litecord_app::AppBuilder> {
    Ok(builder)
}

#[cfg(feature = "discord-user-session")]
fn with_account(
    builder: litecord_app::AppBuilder,
    cfg: &LitecordConfig,
) -> litecord_core::Result<litecord_app::AppBuilder> {
    use std::hash::{Hash, Hasher};
    use std::sync::Arc;
    if cfg.backend.kind != BackendKind::UserSession {
        return Ok(builder);
    }
    let path = std::path::absolute(cfg.database_path())
        .map_err(|_| litecord_core::Error::config("cannot resolve database path"))?;
    // Earlier builds keyed the credential by `DefaultHasher`, whose output
    // may change between Rust releases, so a rebuilt app could lose the
    // saved sign-in. FNV-1a of the path is stable; old entries are moved.
    let mut legacy = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut legacy);
    let secrets = Arc::new(litecord_core::secrets::OsSecretStore::with_legacy(
        &format!(
            "account.v2.{:016x}",
            fnv1a(path.to_string_lossy().as_bytes())
        ),
        &format!("account.{:016x}", legacy.finish()),
    ));
    let transport =
        discord_adapter::user_session::HttpTransport::user_session_with_access(cfg.backend.access)?;
    Ok(builder.backend(Arc::new(
        discord_adapter::user_session::UserSessionBackend::with_access(
            Arc::new(transport),
            secrets,
            Arc::new(litecord_core::clock::SystemClock),
            cfg.backend.access,
        ),
    )))
}

/// 64-bit FNV-1a: a hash that stays the same across builds and toolchains.
#[cfg(feature = "discord-user-session")]
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(not(feature = "discord-user-session"))]
fn with_account(
    builder: litecord_app::AppBuilder,
    cfg: &LitecordConfig,
) -> litecord_core::Result<litecord_app::AppBuilder> {
    if cfg.backend.kind == BackendKind::UserSession {
        return Err(litecord_core::Error::new(litecord_core::ErrorKind::Unsupported,"build with --features gui,discord-user-session to enable the experimental account source"));
    }
    Ok(builder)
}

#[cfg(test)]
mod config_path_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn home() -> Vec<(String, String)> {
        vec![
            ("HOME".into(), "/owner".into()),
            ("LOCALAPPDATA".into(), "/local".into()),
            ("XDG_DATA_HOME".into(), "/xdg".into()),
        ]
    }

    #[test]
    fn launch_without_arguments_uses_the_app_data_folder() {
        let cli = Cli::try_parse_from(["litecord"]).unwrap();
        let cfg = load_config_from(&cli, home()).unwrap();
        assert!(cfg.data_dir.is_absolute(), "{}", cfg.data_dir.display());
        let mode = if cfg!(feature = "discord-user-session") {
            assert_eq!(cfg.backend.kind, BackendKind::UserSession);
            "account"
        } else {
            "demo"
        };
        assert!(cfg.data_dir.ends_with(mode));
        // The same folder whatever the working directory is.
        let cli = Cli::try_parse_from(["litecord", "status"]).unwrap();
        assert_eq!(
            load_config_from(&cli, home()).unwrap().data_dir,
            cfg.data_dir
        );
    }

    #[test]
    fn demo_and_account_never_share_a_folder() {
        let demo = Cli::try_parse_from(["litecord", "--backend", "demo", "status"]).unwrap();
        let account =
            Cli::try_parse_from(["litecord", "--backend", "user-session", "status"]).unwrap();
        let demo = load_config_from(&demo, home()).unwrap();
        let account = load_config_from(&account, home()).unwrap();
        assert!(demo.data_dir.ends_with("demo"));
        assert!(account.data_dir.ends_with("account"));
        assert_ne!(demo.database_path(), account.database_path());
    }

    #[test]
    fn explicit_relative_paths_resolve_against_the_working_directory() {
        let cli = Cli::try_parse_from(["litecord", "--backend", "demo", "--data-dir", "example"])
            .unwrap();
        let cfg = load_config_from(&cli, home()).unwrap();
        assert_eq!(cfg.backend.kind, BackendKind::Demo);
        assert_eq!(
            cfg.data_dir,
            std::env::current_dir().unwrap().join("example")
        );
        let mut env = home();
        env.push(("LITECORD_BACKEND".into(), "demo".into()));
        env.push(("LITECORD_DATA_DIR".into(), "custom".into()));
        let cfg = load_config_from(&Cli::try_parse_from(["litecord"]).unwrap(), env).unwrap();
        assert_eq!(cfg.backend.kind, BackendKind::Demo);
        assert_eq!(
            cfg.data_dir,
            std::env::current_dir().unwrap().join("custom")
        );
    }

    #[test]
    fn config_file_paths_resolve_against_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("account.toml");
        std::fs::write(
            &file,
            "data_dir = \".litecord-account\"\n[backend]\nkind = \"demo\"\n",
        )
        .unwrap();
        let cli = Cli::try_parse_from(["litecord", "--config", file.to_str().unwrap(), "status"])
            .unwrap();
        let cfg = load_config_from(&cli, home()).unwrap();
        assert_eq!(cfg.data_dir, dir.path().join(".litecord-account"));
    }
}
