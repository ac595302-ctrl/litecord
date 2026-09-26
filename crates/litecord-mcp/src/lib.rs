//! MCP server for Litecord.
//!
//! A deliberately small, dependency-free implementation of the parts of the
//! Model Context Protocol that Litecord needs: lifecycle (`initialize`,
//! `notifications/initialized`, `ping`), tools (`tools/list`, `tools/call`)
//! and resources (`resources/list`, `resources/templates/list`,
//! `resources/read`). Transport is newline-delimited JSON-RPC 2.0 over stdio.
//!
//! The server is a thin adapter over [`litecord_agent::AgentGateway`]; it adds
//! no capabilities of its own. In particular it has no way to approve
//! actions, read SQLite directly, or see credentials. Discord writes surface
//! as proposals that the user approves in the Litecord app.
//!
//! Logs go to stderr; stdout carries protocol messages only.

mod server;

pub use server::{serve, McpServer, LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS};
