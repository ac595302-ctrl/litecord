//! Harness-neutral vocabulary shared by every driver and by the app.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which external harness powers Omni.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessKind {
    Codex,
    #[serde(rename = "opencode")]
    OpenCode,
    /// Scripted driver for tests and the demo build.
    Fake,
}

impl HarnessKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Fake => "fake",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Fake => "Demo harness",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::OpenCode),
            "fake" => Some(Self::Fake),
            _ => None,
        }
    }
}

/// Sign-in state reported by the harness. Credentials themselves never
/// leave the harness; this carries display data only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LoginState {
    /// The harness binary was not found.
    NotInstalled,
    /// Installed; the sidecar is not running, so the state is unknown.
    Stopped,
    SignedOut,
    /// A browser sign-in is in progress; open `url` if present.
    SigningIn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        /// Extra instructions from the harness (e.g. a device code).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    Ready {
        /// Display label such as "ChatGPT Plus" or a provider name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account: Option<String>,
    },
    Error {
        message: String,
    },
}

impl LoginState {
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

/// What Omni may touch in a session (see docs/AGENT_HARNESS.md §5).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmniMode {
    /// Litecord tools only. Harness shell/file requests are auto-declined.
    #[default]
    Assistant,
    /// Sandboxed shell and files inside the Omni workspace directory.
    Workspace,
    /// Whatever computer-use tools the user configured in the harness.
    ComputerUse,
}

impl OmniMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assistant => "assistant",
            Self::Workspace => "workspace",
            Self::ComputerUse => "computer_use",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "assistant" => Some(Self::Assistant),
            "workspace" => Some(Self::Workspace),
            "computer_use" => Some(Self::ComputerUse),
            _ => None,
        }
    }

    /// Whether harness-level command/file requests may reach the user at
    /// all. In Assistant mode they are declined without asking.
    pub fn allows_local_tools(self) -> bool {
        !matches!(self, Self::Assistant)
    }
}

/// How the harness launches Litecord's MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpLaunch {
    pub command: PathBuf,
    pub args: Vec<String>,
}

/// Everything a driver needs to start or resume a session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Developer instructions (Omni prompt + profile).
    pub instructions: String,
    pub mode: OmniMode,
    pub model: Option<String>,
    /// Working directory: the Omni workspace, never the data directory.
    pub cwd: PathBuf,
    /// Litecord's data directory, which the harness must not read.
    pub protected_dir: PathBuf,
    pub mcp: McpLaunch,
}

/// Opaque id of a session/thread inside the harness.
pub type ExternalId = String;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// Omni's reply text.
    Message,
    Reasoning,
    /// A tool call (Litecord MCP tools and others).
    ToolCall,
    Command,
    FileChange,
    Subagent,
    Plan,
    Compaction,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Reasoning => "reasoning",
            Self::ToolCall => "tool_call",
            Self::Command => "command",
            Self::FileChange => "file_change",
            Self::Subagent => "subagent",
            Self::Plan => "plan",
            Self::Compaction => "compaction",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "message" => Self::Message,
            "reasoning" => Self::Reasoning,
            "tool_call" => Self::ToolCall,
            "command" => Self::Command,
            "file_change" => Self::FileChange,
            "subagent" => Self::Subagent,
            "plan" => Self::Plan,
            "compaction" => Self::Compaction,
            _ => return None,
        })
    }
}

/// A completed transcript item. `text` is the full content for messages;
/// for tool calls and commands it is a short, content-light label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptItem {
    pub kind: ItemKind,
    pub text: String,
}

/// Harness-level approval request (not a Discord action: those always go
/// through the Action Engine via MCP proposals).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RequestKind {
    Command {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    FileChange {
        summary: String,
    },
    Permission {
        title: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessRequest {
    /// Driver-scoped id used to answer.
    pub id: String,
    pub session: ExternalId,
    pub request: RequestKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Accept,
    AcceptForSession,
    Decline,
}

/// Events streamed from a driver.
#[derive(Debug, Clone, PartialEq)]
pub enum HarnessEvent {
    /// A turn started.
    TurnStarted {
        session: ExternalId,
    },
    /// Streaming reply text. Ephemeral: never persisted as-is.
    Delta {
        session: ExternalId,
        text: String,
    },
    ItemCompleted {
        session: ExternalId,
        item: TranscriptItem,
    },
    TurnCompleted {
        session: ExternalId,
        usage: Option<TokenUsage>,
    },
    TurnFailed {
        session: ExternalId,
        message: String,
    },
    Request(HarnessRequest),
    /// A request was resolved elsewhere (timeout, the harness cancelled).
    RequestResolved {
        id: String,
    },
    Compacted {
        session: ExternalId,
    },
    Login(LoginState),
    /// The sidecar exited; drivers are unusable afterwards.
    Exited {
        message: Option<String>,
    },
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum HarnessError {
    #[error("{0} is not installed")]
    NotInstalled(&'static str),
    #[error("not signed in to the harness")]
    SignedOut,
    #[error("harness sidecar is not running")]
    NotRunning,
    #[error("harness protocol error: {0}")]
    Protocol(String),
    #[error("harness error: {0}")]
    Harness(String),
    #[error("harness did not answer in time")]
    Timeout,
    #[error("unsupported by this harness: {0}")]
    Unsupported(&'static str),
}

pub type HarnessResult<T> = Result<T, HarnessError>;
