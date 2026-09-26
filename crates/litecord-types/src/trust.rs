//! Trust labels and agent visibility — the prompt-injection boundary.
//!
//! Discord messages are **external content**. They are serialized into agent
//! context with `trusted_as_instruction = false` and can never authorize an
//! action. Only [`TrustLevel::SystemPolicy`], [`TrustLevel::UserInstruction`]
//! and explicit UI approval (handled by the Action Engine) authorize.

use serde::{Deserialize, Serialize};

use crate::provenance::Origin;
use crate::ValidationError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    SystemPolicy,
    UserInstruction,
    UserConfirmedMemory,
    AgentDerived,
    ExternalDiscordContent,
}

impl TrustLevel {
    /// Only these levels may authorize actions.
    pub const fn can_authorize_actions(self) -> bool {
        matches!(self, TrustLevel::SystemPolicy | TrustLevel::UserInstruction)
    }

    /// Whether content at this level may be treated as an instruction by an
    /// agent runtime.
    pub const fn trusted_as_instruction(self) -> bool {
        self.can_authorize_actions()
    }

    /// Default trust label for content with the given origin.
    pub const fn for_origin(origin: Origin) -> TrustLevel {
        match origin {
            Origin::DiscordSocialSdk
            | Origin::DiscordUserSession
            | Origin::DiscordBotGateway
            | Origin::Synthetic => TrustLevel::ExternalDiscordContent,
            Origin::UserProvided => TrustLevel::UserConfirmedMemory,
            Origin::AgentDerived | Origin::LocalApplication | Origin::Imported => {
                TrustLevel::AgentDerived
            }
        }
    }
}

/// Per-conversation agent visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentVisibility {
    /// Agents may read metadata and message content.
    #[default]
    Allowed,
    /// Agents may see that the conversation exists (participants, activity
    /// times) but never message content.
    MetadataOnly,
    /// Agents may not see the conversation at all.
    Hidden,
}

impl AgentVisibility {
    pub const fn as_str(self) -> &'static str {
        match self {
            AgentVisibility::Allowed => "allowed",
            AgentVisibility::MetadataOnly => "metadata_only",
            AgentVisibility::Hidden => "hidden",
        }
    }

    pub fn parse(s: &str) -> Result<Self, ValidationError> {
        match s {
            "allowed" => Ok(Self::Allowed),
            "metadata_only" => Ok(Self::MetadataOnly),
            "hidden" => Ok(Self::Hidden),
            _ => Err(ValidationError::Parse {
                what: "AgentVisibility",
                input: s.to_owned(),
            }),
        }
    }

    pub const fn allows_content(self) -> bool {
        matches!(self, AgentVisibility::Allowed)
    }

    pub const fn allows_metadata(self) -> bool {
        !matches!(self, AgentVisibility::Hidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_content_never_authorizes() {
        assert!(!TrustLevel::ExternalDiscordContent.can_authorize_actions());
        assert!(!TrustLevel::AgentDerived.can_authorize_actions());
        assert!(!TrustLevel::UserConfirmedMemory.can_authorize_actions());
        assert!(TrustLevel::UserInstruction.can_authorize_actions());
        assert_eq!(
            TrustLevel::for_origin(Origin::DiscordSocialSdk),
            TrustLevel::ExternalDiscordContent
        );
    }

    #[test]
    fn visibility_semantics() {
        assert!(AgentVisibility::MetadataOnly.allows_metadata());
        assert!(!AgentVisibility::MetadataOnly.allows_content());
        assert!(!AgentVisibility::Hidden.allows_metadata());
    }
}
