//! Adds "copy" actions to a message's context menu.

use crate::feature::{
    Feature, FeatureContext, FeatureMetadata, MessageAction, MessageActionContext,
};
use crate::intent::AppIntent;

const GROUP: &str = "copy";

#[derive(Debug, Default)]
pub struct QuickCopy;

impl Feature for QuickCopy {
    fn metadata(&self) -> FeatureMetadata {
        FeatureMetadata {
            id: "quick_copy",
            name: "Quick Copy",
            description: "Copy a message's text, message id or author id to the clipboard.",
            default_enabled: true,
        }
    }

    fn message_actions(
        &self,
        msg: &MessageActionContext,
        _ctx: &FeatureContext,
        out: &mut Vec<MessageAction>,
    ) {
        out.push(MessageAction {
            id: "quick_copy.copy_text".to_string(),
            label: "Copy text".to_string(),
            group: GROUP.to_string(),
            intents: vec![AppIntent::CopyToClipboard {
                text: msg.content.clone(),
            }],
        });
        out.push(MessageAction {
            id: "quick_copy.copy_message_id".to_string(),
            label: "Copy message ID".to_string(),
            group: GROUP.to_string(),
            intents: vec![AppIntent::CopyToClipboard {
                text: msg.message_id.to_string(),
            }],
        });
        out.push(MessageAction {
            id: "quick_copy.copy_user_id".to_string(),
            label: "Copy user ID".to_string(),
            group: GROUP.to_string(),
            intents: vec![AppIntent::CopyToClipboard {
                text: msg.author_id.to_string(),
            }],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litecord_types::ids::{ConversationId, MessageId, UserId};

    #[test]
    fn produces_three_copy_actions() {
        let feature = QuickCopy;
        let msg_ctx = MessageActionContext {
            message_id: MessageId(42),
            conversation_id: ConversationId(1),
            author_id: UserId(7),
            content: "hello world".to_string(),
            guild_id: None,
            authored_by_me: false,
        };
        let mut out = Vec::new();
        feature.message_actions(&msg_ctx, &FeatureContext::default(), &mut out);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|a| a.group == "copy"));
        assert_eq!(
            out[0].intents,
            vec![AppIntent::CopyToClipboard {
                text: "hello world".to_string()
            }]
        );
        assert_eq!(
            out[1].intents,
            vec![AppIntent::CopyToClipboard {
                text: "42".to_string()
            }]
        );
        assert_eq!(
            out[2].intents,
            vec![AppIntent::CopyToClipboard {
                text: "7".to_string()
            }]
        );
    }
}
