#![cfg(all(feature = "codex", feature = "opencode"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Sign-in against the real Codex and OpenCode binaries. Ignored unless
//! asked for, so CI stays hermetic:
//!
//! ```text
//! LITECORD_REAL_HARNESS=1 PATH=<dir with codex and opencode>:$PATH \
//!   cargo test -p litecord-harness --features codex,opencode \
//!   --test real_harness -- --ignored --test-threads=1
//! ```
//!
//! Each test points the harness at fresh temporary homes (`CODEX_HOME`,
//! `XDG_*`, `HOME`), so the user's real configuration is never read or
//! changed. Only a dummy API key is used; nothing talks to a model.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use litecord_harness::codex::CodexLauncher;
use litecord_harness::opencode::OpenCodeLauncher;
use litecord_harness::*;
use serde_json::Value;
use tokio::sync::broadcast;

const DUMMY_KEY: &str = "sk-test-not-real";

fn enabled() -> bool {
    std::env::var("LITECORD_REAL_HARNESS").as_deref() == Ok("1")
}

/// Fresh harness homes under a temp dir; sets the variables the harness
/// children inherit (the allowlist passes `HOME`, `CODEX_*`, `XDG_*`).
fn isolate(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "litecord-real-harness-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["home", "codex", "data", "config", "state", "cache", "work"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    // Tests in this file run one at a time (`--test-threads=1`).
    let set = |k: &str, v: &Path| std::env::set_var(k, v);
    set("HOME", &root.join("home"));
    set("CODEX_HOME", &root.join("codex"));
    set("XDG_DATA_HOME", &root.join("data"));
    set("XDG_CONFIG_HOME", &root.join("config"));
    set("XDG_STATE_HOME", &root.join("state"));
    set("XDG_CACHE_HOME", &root.join("cache"));
    root
}

fn ctx(root: &Path) -> LaunchContext {
    LaunchContext {
        workspace: root.join("work"),
        protected_dir: root.join("litecord-data"),
        mcp: McpLaunch {
            command: PathBuf::from("/bin/true"),
            args: vec!["mcp".into()],
        },
    }
}

/// `(host, callback port)` of an OAuth URL with a localhost `redirect_uri`.
fn auth_url_parts(url: &str) -> (String, Option<u16>) {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?']).next().unwrap_or("").to_owned();
    let port = url
        .split("redirect_uri=http%3A%2F%2Flocalhost%3A")
        .nth(1)
        .and_then(|r| r.split('%').next())
        .and_then(|p| p.parse().ok());
    (host, port)
}

fn listening(port: u16) -> bool {
    TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(1)).is_ok()
}

async fn login_event(rx: &mut broadcast::Receiver<HarnessEvent>) -> LoginState {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("sign-in event in time")
            .expect("channel open");
        if let HarnessEvent::Login(l) = ev {
            return l;
        }
    }
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the real codex binary; set LITECORD_REAL_HARNESS=1"]
async fn codex_sign_in_paths() {
    if !enabled() {
        return;
    }
    let root = isolate("codex");
    let launcher = CodexLauncher::default();
    assert!(launcher.installed(), "codex is not on PATH");
    let d: Arc<dyn HarnessDriver> = launcher.launch(&ctx(&root)).await.unwrap();
    let mut rx = d.subscribe();
    assert_eq!(d.login_state().await.unwrap(), LoginState::SignedOut);

    let options = d.login_options().await.unwrap();
    let ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, ["chatgpt", "api_key", "chatgpt_device_code"]);

    // Browser sign-in: a real auth URL and a localhost callback waiting.
    let LoginState::SigningIn { url: Some(url), .. } = d.begin_login_with("chatgpt").await.unwrap()
    else {
        panic!("expected a sign-in URL");
    };
    let (host, port) = auth_url_parts(&url);
    assert_eq!(host, "auth.openai.com");
    let port = port.expect("localhost redirect_uri");
    assert!(listening(port), "callback server on {port}");

    // Abandon it and start again: the old attempt's "cancelled" report is
    // not surfaced, the new one waits.
    let LoginState::SigningIn {
        url: Some(url2), ..
    } = d.begin_login().await.unwrap()
    else {
        panic!("expected a sign-in URL");
    };
    assert_ne!(url, url2);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    while let Ok(ev) = rx.try_recv() {
        assert!(
            !matches!(ev, HarnessEvent::Login(LoginState::Error { .. })),
            "stale sign-in error surfaced: {ev:?}"
        );
    }
    let port2 = auth_url_parts(&url2).1.unwrap();
    // Cancelling frees the callback port.
    d.cancel_login().await.unwrap();
    let mut freed = false;
    for _ in 0..30 {
        if !listening(port2) {
            freed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(freed, "callback port {port2} still bound after cancel");

    // API key sign-in: stored by Codex in CODEX_HOME, reported as ready.
    d.login_api_key("api_key", DUMMY_KEY).await.unwrap();
    assert_eq!(
        login_event(&mut rx).await,
        LoginState::Ready {
            account: Some("OpenAI API key".into())
        }
    );
    let auth = read_json(&root.join("codex").join("auth.json")).expect("auth.json");
    assert_eq!(auth["OPENAI_API_KEY"], DUMMY_KEY);
    assert!(d.login_state().await.unwrap().is_ready());
    assert!(!d.models().await.unwrap().is_empty());

    d.logout().await.unwrap();
    assert!(!root.join("codex").join("auth.json").exists());
    assert_eq!(d.login_state().await.unwrap(), LoginState::SignedOut);
    d.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the real opencode binary; set LITECORD_REAL_HARNESS=1"]
async fn opencode_sign_in_paths() {
    if !enabled() {
        return;
    }
    let root = isolate("opencode");
    let launcher = OpenCodeLauncher::default();
    assert!(launcher.installed(), "opencode is not on PATH");
    let d = launcher.spawn(&ctx(&root)).await.unwrap();
    let mut rx = d.subscribe();
    // OpenCode's own free provider works without a key (providers set up
    // through the environment, e.g. AWS_*, show up as well).
    let baseline = d.login_state().await.unwrap();
    assert!(
        matches!(&baseline, LoginState::Ready { account: Some(a) } if a.contains("OpenCode Zen (free models)")),
        "{baseline:?}"
    );

    let options = d.login_options().await.unwrap();
    let find = |id: &str| options.iter().find(|o| o.id == id);
    assert_eq!(find("openai:0").unwrap().kind, LoginKind::Browser);
    assert_eq!(find("anthropic:api").unwrap().kind, LoginKind::ApiKey);
    assert!(find("github-copilot:0").is_some_and(|o| !o.prompts.is_empty()));
    assert!(options.len() > 50, "every provider takes an API key");

    // Browser sign-in: a real auth URL and OpenCode's callback waiting.
    let LoginState::SigningIn { url: Some(url), .. } =
        d.begin_login_with("openai:0").await.unwrap()
    else {
        panic!("expected a sign-in URL");
    };
    let (host, port) = auth_url_parts(&url);
    assert_eq!(host, "auth.openai.com");
    assert!(listening(port.expect("localhost redirect_uri")));
    // OpenCode cannot cancel it in place; the app restarts the sidecar.
    assert!(matches!(
        d.cancel_login().await,
        Err(HarnessError::Unsupported(_))
    ));

    // API key for a provider without plugin methods; usable at once.
    d.login_api_key("anthropic:api", DUMMY_KEY).await.unwrap();
    let state = login_event(&mut rx).await;
    assert!(
        matches!(&state, LoginState::Ready { account: Some(a) } if a.starts_with("Anthropic")),
        "{state:?}"
    );
    let auth_file = root.join("data").join("opencode").join("auth.json");
    let auth = read_json(&auth_file).expect("auth.json");
    assert_eq!(auth["anthropic"]["key"], DUMMY_KEY);
    let models = d.models().await.unwrap();
    assert!(
        models.iter().any(|m| m.starts_with("anthropic/")),
        "new provider's models without a restart"
    );

    d.logout_provider("anthropic").await.unwrap();
    let auth = read_json(&auth_file).unwrap_or_default();
    assert!(auth.get("anthropic").is_none());
    assert_eq!(d.login_state().await.unwrap(), baseline);
    d.shutdown().await;
    let _ = std::fs::remove_dir_all(&root);
}
