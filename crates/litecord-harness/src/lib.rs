//! Omni harness drivers (design: `docs/AGENT_HARNESS.md`).
//!
//! Litecord does not run inference. Omni is powered by the user's own
//! harness (Codex or OpenCode), signed in with that harness's own login and
//! started as a sidecar only while Omni is in use. The harness reaches
//! Litecord through the `litecord mcp` server, so it gets exactly the tool
//! surface, visibility rules and proposal-only Discord writes that exist
//! for any MCP client. This crate only speaks the harness protocols.

#[cfg(feature = "codex")]
pub mod codex;
pub mod driver;
pub mod env;
pub mod fake;
#[cfg(feature = "opencode")]
pub mod opencode;
pub mod types;

pub use driver::{
    find_executable, HarnessDriver, HarnessLauncher, LaunchContext, ResolvedExecutable,
};
pub use fake::{FakeDriver, FakeLauncher, FakeTurn};
pub use types::*;
