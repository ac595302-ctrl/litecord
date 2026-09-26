use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use litecord_agent::{AgentGateway, Caller, ToolError};

pub const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const RESOURCE_NOT_FOUND: i64 = -32002;

/// Protocol state machine. Transport-agnostic: feed it parsed JSON values.
#[derive(Debug)]
pub struct McpServer {
    gateway: AgentGateway,
    caller: Caller,
    initialized: bool,
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

impl McpServer {
    /// `client_label` identifies the harness in audit logs (e.g. "mcp").
    pub fn new(gateway: AgentGateway, client_label: &str) -> Self {
        Self {
            gateway,
            caller: Caller::new(client_label),
            initialized: false,
        }
    }

    /// Handle one JSON-RPC message (or batch). Returns the response to send,
    /// or `None` for notifications.
    pub async fn handle(&mut self, message: Value) -> Option<Value> {
        if let Value::Array(batch) = message {
            if batch.is_empty() {
                return Some(error_response(Value::Null, INVALID_REQUEST, "empty batch"));
            }
            let mut out = Vec::new();
            for m in batch {
                if let Some(r) = Box::pin(self.handle(m)).await {
                    out.push(r);
                }
            }
            return (!out.is_empty()).then_some(Value::Array(out));
        }
        let Some(obj) = message.as_object() else {
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "expected an object",
            ));
        };
        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error_response(
                obj.get("id").cloned().unwrap_or(Value::Null),
                INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
            ));
        }
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // A response from the client (we never send requests) — ignore.
            return None;
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = obj.get("id").cloned() else {
            self.handle_notification(method);
            return None;
        };
        tracing::debug!(method, "mcp request");
        Some(match self.dispatch(method, params).await {
            Ok(result) => ok_response(id, result),
            Err((code, msg)) => error_response(id, code, &msg),
        })
    }

    fn handle_notification(&mut self, method: &str) {
        match method {
            "notifications/initialized" => self.initialized = true,
            "notifications/cancelled" => {} // requests complete synchronously
            other => tracing::debug!(method = other, "ignoring notification"),
        }
    }

    async fn dispatch(&mut self, method: &str, params: Value) -> Result<Value, (i64, String)> {
        match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.tools_list()),
            "tools/call" => self.tools_call(params).await,
            "resources/list" => Ok(self.resources_list()),
            "resources/templates/list" => Ok(self.resource_templates_list()),
            "resources/read" => self.resources_read(&params),
            _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
        }
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let version = requested
            .filter(|v| SUPPORTED_PROTOCOL_VERSIONS.contains(v))
            .unwrap_or(LATEST_PROTOCOL_VERSION);
        json!({
            "protocolVersion": version,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false }
            },
            "serverInfo": {
                "name": "litecord",
                "title": "Litecord unified memory",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": litecord_agent::prompts::MCP_INSTRUCTIONS.trim()
        })
    }

    fn tools_list(&self) -> Value {
        let tools: Vec<Value> = self
            .gateway
            .tools()
            .into_iter()
            .map(|t| {
                let class = t.class.as_str();
                json!({
                    "name": t.name,
                    "title": t.title,
                    "description": t.description,
                    "inputSchema": t.input_schema,
                    "annotations": {
                        "readOnlyHint": class == "read",
                        "destructiveHint": false,
                        "openWorldHint": class == "discord_write",
                    }
                })
            })
            .collect();
        json!({ "tools": tools })
    }

    async fn tools_call(&mut self, params: Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((INVALID_PARAMS, "`name` is required".to_owned()))?
            .to_owned();
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        match self.gateway.call_tool(&name, args, &self.caller).await {
            Ok(value) => Ok(json!({
                "content": [{ "type": "text", "text": value.to_string() }],
                "structuredContent": value,
                "isError": false
            })),
            Err(ToolError::UnknownTool(n)) => Err((INVALID_PARAMS, format!("unknown tool: {n}"))),
            // Tool-level failures are reported in-band so the model can react.
            Err(e) => {
                if !e.is_client_error() {
                    tracing::warn!(tool = %name, error = %e, "tool failed");
                }
                Ok(json!({
                    "content": [{ "type": "text", "text": e.to_string() }],
                    "isError": true
                }))
            }
        }
    }

    fn resources_list(&self) -> Value {
        let resources: Vec<Value> = self
            .gateway
            .resources()
            .into_iter()
            .map(|r| json!({ "uri": r.uri, "name": r.name, "description": r.description, "mimeType": "application/json" }))
            .collect();
        json!({ "resources": resources })
    }

    fn resource_templates_list(&self) -> Value {
        let templates: Vec<Value> = self
            .gateway
            .resource_templates()
            .into_iter()
            .map(|r| json!({ "uriTemplate": r.uri_template, "name": r.name, "description": r.description, "mimeType": "application/json" }))
            .collect();
        json!({ "resourceTemplates": templates })
    }

    fn resources_read(&self, params: &Value) -> Result<Value, (i64, String)> {
        let uri = params
            .get("uri")
            .and_then(Value::as_str)
            .ok_or((INVALID_PARAMS, "`uri` is required".to_owned()))?;
        match self.gateway.read_resource(uri) {
            Ok(v) => Ok(json!({
                "contents": [{ "uri": uri, "mimeType": "application/json", "text": v.to_string() }]
            })),
            Err(ToolError::NotFound(what)) => {
                Err((RESOURCE_NOT_FOUND, format!("{what} not found")))
            }
            Err(e) if e.is_client_error() => Err((INVALID_PARAMS, e.to_string())),
            Err(e) => {
                tracing::warn!(%uri, error = %e, "resource read failed");
                Err((INTERNAL_ERROR, "internal error".to_owned()))
            }
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }
}

/// Serve MCP over a line-delimited stream until EOF.
pub async fn serve<R, W>(server: &mut McpServer, reader: R, mut writer: W) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => server.handle(msg).await,
            Err(_) => Some(error_response(Value::Null, PARSE_ERROR, "parse error")),
        };
        if let Some(resp) = response {
            let mut bytes = serde_json::to_vec(&resp).unwrap_or_default();
            bytes.push(b'\n');
            writer.write_all(&bytes).await?;
            writer.flush().await?;
        }
    }
    Ok(())
}
