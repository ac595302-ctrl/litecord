//! Agent harness abstraction.
//!
//! Litecord is harness-neutral. A harness is whatever drives a model:
//!
//! * **MCP** (`litecord-mcp`): the model runs inside an external tool
//!   (Codex, OpenCode, ...) and calls the gateway through MCP. Litecord does
//!   not run inference itself in this mode.
//! * **In-process harnesses** implement [`AgentHarness`] and call the same
//!   [`AgentGateway`]; e.g. a future `CodexHarness`/`OpenCodeHarness` that
//!   spawns a CLI, or a direct model API harness.
//! * [`ScriptedHarness`] runs a fixed list of tool calls deterministically —
//!   used for tests, demos and replaying agent runs. It performs no
//!   reasoning.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

use crate::error::ToolError;
use crate::gateway::{AgentGateway, Caller};

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallRecord {
    pub tool: String,
    pub ok: bool,
    pub output: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCallRecord>,
}

#[async_trait]
pub trait AgentHarness: Send + Sync + std::fmt::Debug {
    /// Stable identifier recorded as the actor for audit.
    fn id(&self) -> &str;

    async fn execute(
        &self,
        instruction: &str,
        gateway: &AgentGateway,
    ) -> Result<AgentResponse, ToolError>;
}

/// Deterministic harness: executes predefined tool calls in order.
#[derive(Debug, Clone, Default)]
pub struct ScriptedHarness {
    pub steps: Vec<(String, Value)>,
}

impl ScriptedHarness {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn step(mut self, tool: &str, args: Value) -> Self {
        self.steps.push((tool.to_owned(), args));
        self
    }
}

#[async_trait]
impl AgentHarness for ScriptedHarness {
    fn id(&self) -> &str {
        "scripted"
    }

    async fn execute(
        &self,
        instruction: &str,
        gateway: &AgentGateway,
    ) -> Result<AgentResponse, ToolError> {
        let caller = Caller::new(self.id());
        let mut calls = Vec::with_capacity(self.steps.len());
        for (tool, args) in &self.steps {
            match gateway.call_tool(tool, args.clone(), &caller).await {
                Ok(output) => calls.push(ToolCallRecord {
                    tool: tool.clone(),
                    ok: true,
                    output,
                }),
                Err(e) if e.is_client_error() => calls.push(ToolCallRecord {
                    tool: tool.clone(),
                    ok: false,
                    output: Value::String(e.to_string()),
                }),
                Err(e) => return Err(e),
            }
        }
        Ok(AgentResponse {
            text: format!("scripted run for: {instruction}"),
            tool_calls: calls,
        })
    }
}
