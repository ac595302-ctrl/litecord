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

    /// How to install the harness, for a "not installed" hint.
    pub fn install_hint(self) -> &'static str {
        match self {
            Self::Codex => "npm install -g @openai/codex",
            Self::OpenCode => "npm install -g opencode-ai (or see opencode.ai)",
            Self::Fake => "",
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
        /// The browser shows a code the user must paste back
        /// ([`crate::HarnessDriver::submit_login_code`]).
        #[serde(default)]
        needs_code: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginKind {
    /// Opens a browser; may ask for a pasted code.
    Browser,
    /// Paste an API key.
    ApiKey,
}

/// One way to sign in to the harness.
///
/// UIs group options by [`provider`](Self::provider) (showing
/// [`provider_label`](Self::provider_label) as the heading and
/// [`method_label`](Self::method_label) per entry), list `featured` groups
/// first and put the rest behind "More providers". [`label`](Self::label)
/// is the combined one-line text for simple lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginOption {
    /// Driver-scoped id passed back to `begin_login_with` / `login_api_key`.
    pub id: String,
    /// One-line label, e.g. "Sign in with ChatGPT" or
    /// "Anthropic · API key".
    pub label: String,
    pub kind: LoginKind,
    /// Grouping key: the model provider this signs in to (`openai`,
    /// `anthropic`, `github-copilot`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Display name of `provider` ("OpenAI", "GitHub Copilot").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_label: Option<String>,
    /// The method alone, for use under a provider heading
    /// ("ChatGPT Pro/Plus (browser)", "API key").
    #[serde(default)]
    pub method_label: String,
    /// Worth showing without "More providers".
    #[serde(default)]
    pub featured: bool,
    /// The provider already has working credentials.
    #[serde(default)]
    pub connected: bool,
    /// Extra fields the method asks for (e.g. a GitHub Enterprise URL).
    /// Values go in the `inputs` map of `begin_login_with_inputs` /
    /// `login_api_key_with`; the harness applies its own defaults when a
    /// field is left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<LoginPrompt>,
}

impl LoginOption {
    /// An option with no grouping or prompts; `method_label` = `label`.
    pub fn new(id: impl Into<String>, label: impl Into<String>, kind: LoginKind) -> Self {
        let label = label.into();
        Self {
            id: id.into(),
            method_label: label.clone(),
            label,
            kind,
            provider: None,
            provider_label: None,
            featured: false,
            connected: false,
            prompts: Vec::new(),
        }
    }
}

/// An extra input a sign-in method asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginPrompt {
    /// Key in the `inputs` map.
    pub key: String,
    /// Question shown to the user.
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Non-empty for a choice; the value sent is `LoginChoice::value`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<LoginChoice>,
    /// Only ask when another input has (or lacks) a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<LoginPromptCondition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginChoice {
    pub label: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Show a prompt only if `inputs[key] == value` (or `!=` when `equals` is
/// false).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginPromptCondition {
    pub key: String,
    pub equals: bool,
    pub value: String,
}

impl LoginPromptCondition {
    pub fn applies(&self, inputs: &LoginInputs) -> bool {
        let matches = inputs.get(&self.key).map(String::as_str) == Some(self.value.as_str());
        matches == self.equals
    }
}

/// Values for [`LoginOption::prompts`], keyed by [`LoginPrompt::key`].
pub type LoginInputs = std::collections::BTreeMap<String, String>;

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

/// Removes every occurrence of `secret` from `text` (used on harness error
/// messages that could echo an API key back).
pub fn redact(text: &str, secret: &str) -> String {
    let secret = secret.trim();
    if secret.is_empty() {
        text.to_owned()
    } else {
        text.replace(secret, "[redacted]")
    }
}

pub type HarnessResult<T> = Result<T, HarnessError>;
