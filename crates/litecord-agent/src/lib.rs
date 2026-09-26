//! Harness-neutral agent layer.
//!
//! [`AgentGateway`] exposes *logical* capabilities over unified memory —
//! never SQLite, never the Discord SDK, never credentials. The MCP server is
//! one transport over it; any other harness uses the same gateway.

pub mod error;
pub mod gateway;
pub mod harness;
pub mod spec;

pub use error::ToolError;
pub use gateway::{AgentGateway, Caller};
pub use harness::{AgentHarness, AgentResponse, ScriptedHarness, ToolCallRecord};
pub use spec::{ResourceSpec, ResourceTemplate, ToolSpec};
