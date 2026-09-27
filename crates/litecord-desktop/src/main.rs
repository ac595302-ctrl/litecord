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
    Status {
        /// Do not start the harness to check the sign-in state.
        #[arg(long)]
        cached: bool,
    },
    /// Choose the harness: `codex` or `opencode`.
    Harness { kind: String },
    /// Sign in. Without `--method`, lists the methods the harness offers.
    Login {
        /// A method id from the list (browser or API key).
        #[arg(long)]
        method: Option<String>,
        /// Read an API key from stdin (pipe it; it is never echoed or stored
        /// by Litecord).
        #[arg(long)]
        api_key_stdin: bool,
        /// A value the method asks for, as `key=value` (repeatable), e.g.
        /// `--input deploymentType=enterprise`.
        #[arg(long = "input", value_name = "KEY=VALUE")]
        inputs: Vec<String>,
        /// List every provider, not only the common and connected ones.
        #[arg(long)]
        all: bool,
    },
    /// Sign out (OpenCode: every stored provider credential, or one with
    /// `--provider`).
    Logout {
        #[arg(long)]
        provider: Option<String>,
    },
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
    Run { id: i64 },
    /// Check installed harnesses against what Litecord expects.
    Doctor,
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
                BackendArg::UserSession => BackendKind::UserSession,
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
            omni,
            #[cfg(feature = "screenshots")]
            omni_ask,
        } => {
            #[cfg(not(feature = "screenshots"))]
            let options = litecord_ui::WindowOptions {
                omni_open: omni,
                ..Default::default()
            };
            #[cfg(feature = "screenshots")]
            let options = litecord_ui::WindowOptions {
                screenshot,
                destination: litecord_layout_destination(&screen),
                size: Some([width, 992.0]),
                omni_open: omni,
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
    options: litecord_ui::WindowOptions,
    omni_ask: Option<String>,
) -> litecord_core::Result<()> {
    let app = with_omni(
        with_account(with_bot(LitecordApp::builder(cfg.clone()))?, &cfg)?,
        &cfg,
    )
    .start()
    .await?;
    if let Some(text) = omni_ask {
        let a = app.clone();
        tokio::spawn(async move {
            if let Err(e) = a.omni().send(None, &text).await {
                tracing::warn!(error = %e, "omni-ask failed");
            }
        });
    }
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
            OmniCmd::Status { cached } => {
                omni.refresh_installed();
                if !cached && omni.status().selected.is_some() {
                    // Starts the harness; a failure shows up as `last_error`.
                    let _ = omni.refresh_login().await;
                }
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
                inputs,
                all,
            } => {
                let options = omni.login_options().await?;
                let Some(method) = method else {
                    print_login_options(&options, all);
                    return Ok(());
                };
                let option = options
                    .iter()
                    .find(|o| o.id == method)
                    .ok_or_else(|| Error::validation("unknown sign-in method"))?;
                let mut values = litecord_app::harness::LoginInputs::new();
                for kv in &inputs {
                    let (k, v) = kv
                        .split_once('=')
                        .ok_or_else(|| Error::validation("--input takes key=value"))?;
                    values.insert(k.trim().to_owned(), v.trim().to_owned());
                }
                if option.kind == LoginKind::ApiKey || api_key_stdin {
                    let mut key = String::new();
                    std::io::stdin()
                        .read_line(&mut key)
                        .map_err(|e| Error::internal(format!("stdin: {e}")))?;
                    omni.sign_in_api_key_with(
                        &option.id,
                        &litecord_core::secrets::Secret::new(key),
                        &values,
                    )
                    .await?;
                } else {
                    match omni.sign_in_with_inputs(&option.id, &values).await? {
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
                            } else {
                                println!("Waiting for the sign-in to finish (Ctrl-C to cancel)...");
                            }
                        }
                        other => print_json(&other),
                    }
                }
                // Wait (bounded) for the harness to confirm.
                for _ in 0..600 {
                    match omni.status().login {
                        LoginState::Ready { account } => {
                            match account {
                                Some(a) => println!("Signed in: {a}"),
                                None => println!("Signed in."),
                            }
                            return Ok(());
                        }
                        LoginState::Error { message } => {
                            return Err(Error::new(ErrorKind::Authentication, message));
                        }
                        _ => tokio::time::sleep(Duration::from_millis(500)).await,
                    }
                }
                return Err(Error::new(
                    ErrorKind::Authentication,
                    "sign-in did not complete",
                ));
            }
            OmniCmd::Logout { provider } => {
                match provider {
                    Some(p) => omni.sign_out_provider(&p).await?,
                    None => omni.sign_out().await?,
                }
                match omni.status().login {
                    LoginState::Ready { account } => println!(
                        "Signed out. Still usable through: {}",
                        account
                            .as_deref()
                            .unwrap_or("the harness's own configuration")
                    ),
                    _ => println!("Signed out."),
                }
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

/// Prints sign-in methods grouped by provider: common providers and any
/// already connected first; the rest only with `all`.
fn print_login_options(options: &[litecord_app::harness::LoginOption], all: bool) {
    use litecord_app::harness::LoginKind;
    let groups = litecord_app::omni::login_groups(options);
    let mut hidden = 0;
    for g in &groups {
        if !(all || g.featured || g.connected) {
            hidden += 1;
            continue;
        }
        let mark = if g.connected { " (connected)" } else { "" };
        let label = if g.label.is_empty() {
            "Sign in"
        } else {
            &g.label
        };
        println!("{label}{mark}");
        for o in &g.options {
            let kind = match o.kind {
                LoginKind::Browser => "browser",
                LoginKind::ApiKey => "api key",
            };
            println!("  {:<28} {} ({kind})", o.id, o.method_label);
            for p in &o.prompts {
                let choices: Vec<&str> = p.choices.iter().map(|c| c.value.as_str()).collect();
                let hint = if choices.is_empty() {
                    p.placeholder.clone().unwrap_or_default()
                } else {
                    choices.join("|")
                };
                println!("  {:<28}   --input {}=<{hint}>  {}", "", p.key, p.message);
            }
        }
    }
    if hidden > 0 {
        println!("\n{hidden} more providers: litecord omni login --all");
    }
    println!("\nSign in with: litecord omni login --method <id>");
}

/// Checks what Omni depends on and prints a plain report.
async fn doctor(
    cfg: &LitecordConfig,
    omni: &litecord_app::OmniService,
) -> litecord_core::Result<()> {
    let ok = |b: bool| if b { "ok  " } else { "FAIL" };
    omni.refresh_installed();
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
            match (&h.path, h.installed) {
                (Some(p), _) => format!("installed ({p})"),
                (None, true) => "installed".to_owned(),
                (None, false) => format!("not installed ({})", h.install_hint),
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
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hash);
    let secrets = Arc::new(litecord_core::secrets::OsSecretStore::new(&format!(
        "account.{:016x}",
        hash.finish()
    )));
    let transport = discord_adapter::user_session::HttpTransport::user_session()?;
    Ok(builder.backend(Arc::new(
        discord_adapter::user_session::UserSessionBackend::new(
            Arc::new(transport),
            secrets,
            Arc::new(litecord_core::clock::SystemClock),
        ),
    )))
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
