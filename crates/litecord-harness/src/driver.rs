//! The one trait every harness implements, plus the launcher that starts
//! its sidecar on demand.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::types::*;

/// A running harness sidecar. All methods are cheap to call concurrently;
/// drivers serialize what the harness requires internally.
#[async_trait]
pub trait HarnessDriver: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> HarnessKind;

    /// Events from the sidecar. Subscribe before calling `send`.
    fn subscribe(&self) -> broadcast::Receiver<HarnessEvent>;

    async fn login_state(&self) -> HarnessResult<LoginState>;
    /// Starts the harness's own sign-in; returns `SigningIn { url }`.
    /// Completion arrives as `HarnessEvent::Login`.
    async fn begin_login(&self) -> HarnessResult<LoginState>;
    async fn logout(&self) -> HarnessResult<()>;

    /// Sign-in methods the harness offers (browser OAuth, API key, ...).
    /// The default is one browser method that maps to [`Self::begin_login`].
    async fn login_options(&self) -> HarnessResult<Vec<LoginOption>> {
        Ok(vec![LoginOption {
            id: "default".into(),
            label: "Sign in with browser".into(),
            kind: LoginKind::Browser,
        }])
    }

    /// Start a specific browser sign-in method from [`Self::login_options`].
    async fn begin_login_with(&self, option: &str) -> HarnessResult<LoginState> {
        let _ = option;
        self.begin_login().await
    }

    /// Finish a sign-in that needs a code pasted from the browser
    /// (`SigningIn { needs_code: true }`).
    async fn submit_login_code(&self, code: &str) -> HarnessResult<()> {
        let _ = code;
        Err(HarnessError::Unsupported("pasted sign-in codes"))
    }

    /// Sign in with an API key. The key goes straight to the harness, which
    /// stores it in its own credential store; Litecord keeps no copy.
    async fn login_api_key(&self, option: &str, key: &str) -> HarnessResult<()> {
        let _ = (option, key);
        Err(HarnessError::Unsupported("API key sign-in"))
    }

    /// Models the harness can use, as it names them (e.g. `gpt-5-codex`,
    /// `anthropic/claude-sonnet-4`). Empty when the harness can't list them.
    async fn models(&self) -> HarnessResult<Vec<String>> {
        Ok(Vec::new())
    }

    async fn start_session(&self, cfg: &SessionConfig) -> HarnessResult<ExternalId>;
    /// Reopen a session after a sidecar restart. Drivers that cannot resume
    /// return `Unsupported`; the service then starts a fresh session.
    async fn resume_session(&self, id: &str, cfg: &SessionConfig) -> HarnessResult<ExternalId>;
    async fn send(&self, session: &str, text: &str) -> HarnessResult<()>;
    async fn interrupt(&self, session: &str) -> HarnessResult<()>;
    async fn compact(&self, session: &str) -> HarnessResult<()>;
    async fn answer(&self, request_id: &str, decision: Decision) -> HarnessResult<()>;

    /// Stop the sidecar. Idempotent.
    async fn shutdown(&self);
}

/// Paths and launch details the app gives a launcher.
#[derive(Debug, Clone)]
pub struct LaunchContext {
    /// The Omni workspace directory (cwd for the harness).
    pub workspace: PathBuf,
    /// Litecord's data directory; must not be exposed to the harness.
    pub protected_dir: PathBuf,
    pub mcp: McpLaunch,
}

/// Detects and starts one harness.
#[async_trait]
pub trait HarnessLauncher: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> HarnessKind;
    /// Whether the harness binary is available (no process is started).
    fn installed(&self) -> bool;
    async fn launch(&self, ctx: &LaunchContext) -> HarnessResult<Arc<dyn HarnessDriver>>;
}

/// `PATH` plus the usual per-user install folders.
///
/// An app opened from Finder (or a desktop shortcut) gets a minimal `PATH`
/// such as `/usr/bin:/bin:/usr/sbin:/sbin`, without Homebrew, npm or
/// installer folders, so Codex and OpenCode looked "not installed" unless
/// Litecord was started from a terminal. Codex is also a Node script, so the
/// harness itself needs these folders to find `node`.
pub fn search_path() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let mut extra: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            extra.push(PathBuf::from(appdata).join("npm"));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            extra.push(PathBuf::from(local).join("Programs").join("opencode"));
        }
    } else {
        extra.extend(["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"].map(PathBuf::from));
    }
    if let Some(home) = &home {
        for rel in [
            ".local/bin",
            ".npm-global/bin",
            ".opencode/bin",
            ".bun/bin",
            ".volta/bin",
            ".cargo/bin",
            "bin",
        ] {
            extra.push(home.join(rel));
        }
        // nvm installs one folder per Node version; prefer the newest.
        if let Ok(entries) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            let mut versions: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
            versions.sort();
            extra.extend(versions.into_iter().rev().map(|v| v.join("bin")));
        }
    }
    for dir in extra {
        if dir.is_dir() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// Finds an executable on [`search_path`] (or returns `explicit` if it exists).
pub fn find_executable(name: &str, explicit: Option<&std::path::Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return p.is_file().then(|| p.to_path_buf());
    }
    let exts: &[&str] = if cfg!(windows) {
        &["exe", "cmd", "bat"]
    } else {
        &[""]
    };
    for dir in search_path() {
        for ext in exts {
            let mut candidate = dir.join(name);
            if !ext.is_empty() {
                candidate.set_extension(ext);
            }
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
