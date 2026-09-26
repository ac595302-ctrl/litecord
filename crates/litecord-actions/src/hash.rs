//! Canonical payload hashing.
//!
//! Approval is bound to the *exact* action: the hash covers the action kind,
//! every target id and all content. `AgentAction` contains no maps or floats
//! that could serialize non-deterministically, so `serde_json` output is
//! canonical for a given value.

use litecord_types::actions::AgentAction;

use crate::error::ActionError;

/// Canonical JSON for an action (what is stored and hashed).
pub fn canonical_json(action: &AgentAction) -> Result<String, ActionError> {
    serde_json::to_string(action).map_err(|e| ActionError::Internal(e.to_string()))
}

/// Hex blake3 hash of the canonical JSON.
pub fn payload_hash(action: &AgentAction) -> Result<String, ActionError> {
    Ok(hash_json(&canonical_json(action)?))
}

pub fn hash_json(json: &str) -> String {
    blake3::hash(json.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use litecord_types::actions::MessageTarget;
    use litecord_types::ids::{ConversationId, UserId};

    fn send(to: u64, content: &str) -> AgentAction {
        AgentAction::SendMessage {
            target: MessageTarget::User {
                user_id: UserId(to),
            },
            content: content.into(),
        }
    }

    #[test]
    fn hash_changes_with_recipient_content_or_operation() {
        let base = payload_hash(&send(1, "hi")).unwrap();
        assert_eq!(base, payload_hash(&send(1, "hi")).unwrap(), "deterministic");
        assert_ne!(base, payload_hash(&send(2, "hi")).unwrap(), "recipient");
        assert_ne!(base, payload_hash(&send(1, "hi!")).unwrap(), "content");
        let draft = AgentAction::DraftMessage {
            conversation_id: ConversationId(1),
            content: "hi".into(),
        };
        assert_ne!(base, payload_hash(&draft).unwrap(), "operation");
    }
}
