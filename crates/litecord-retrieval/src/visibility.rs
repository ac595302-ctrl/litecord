//! Per-conversation agent visibility, resolved once per read snapshot.

use litecord_store::repos::{self, Connection};
use litecord_types::ids::ConversationId;
use litecord_types::trust::AgentVisibility;

use crate::error::RetrievalError;

/// Resolves `AgentVisibility` for any conversation: explicit per-conversation
/// settings first, then the configured default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibilityPolicy {
    pub default: AgentVisibility,
    pub hidden: Vec<ConversationId>,
    pub metadata_only: Vec<ConversationId>,
    pub allowed: Vec<ConversationId>,
}

impl VisibilityPolicy {
    pub fn load(conn: &Connection, default: AgentVisibility) -> Result<Self, RetrievalError> {
        Ok(Self {
            default,
            hidden: repos::conversations::ids_with_visibility(conn, AgentVisibility::Hidden)?,
            metadata_only: repos::conversations::ids_with_visibility(
                conn,
                AgentVisibility::MetadataOnly,
            )?,
            allowed: repos::conversations::ids_with_visibility(conn, AgentVisibility::Allowed)?,
        })
    }

    /// Everything visible: for the user's own searches in the UI.
    pub fn unrestricted() -> Self {
        Self {
            default: AgentVisibility::Allowed,
            hidden: Vec::new(),
            metadata_only: Vec::new(),
            allowed: Vec::new(),
        }
    }

    pub fn visibility_of(&self, c: ConversationId) -> AgentVisibility {
        if self.hidden.contains(&c) {
            AgentVisibility::Hidden
        } else if self.metadata_only.contains(&c) {
            AgentVisibility::MetadataOnly
        } else if self.allowed.contains(&c) {
            AgentVisibility::Allowed
        } else {
            self.default
        }
    }

    pub fn allows_content(&self, c: ConversationId) -> bool {
        self.visibility_of(c).allows_content()
    }

    pub fn allows_metadata(&self, c: ConversationId) -> bool {
        self.visibility_of(c).allows_metadata()
    }

    /// Conversations explicitly excluded from content (SQL pre-filter; the
    /// default is applied afterwards in Rust).
    pub fn content_excluded(&self) -> Vec<ConversationId> {
        self.hidden
            .iter()
            .chain(&self.metadata_only)
            .copied()
            .collect()
    }
}
