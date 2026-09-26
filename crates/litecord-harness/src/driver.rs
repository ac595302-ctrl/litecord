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

/// Finds an executable on `PATH` (or returns `explicit` if it exists).
pub fn find_executable(name: &str, explicit: Option<&std::path::Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    let exts: &[&str] = if cfg!(windows) {
        &["exe", "cmd", "bat"]
    } else {
        &[""]
    };
    for dir in std::env::split_paths(&path) {
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
