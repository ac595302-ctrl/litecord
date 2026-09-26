//! Adds a "Bookmark" action to a message's context menu.

use crate::feature::{
    Feature, FeatureContext, FeatureMetadata, MessageAction, MessageActionContext,
};
use crate::intent::AppIntent;

#[derive(Debug, Default)]
pub struct LocalBookmarks;

impl Feature for LocalBookmarks {
    fn metadata(&self) -> FeatureMetadata {
        FeatureMetadata {
            id: "local_bookmarks",
            name: "Local Bookmarks",
            description: "Bookmark a message for later, without leaving a trace in Discord.",
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
            id: "local_bookmarks.bookmark".to_string(),
            label: "Bookmark".to_string(),
            group: "bookmarks".to_string(),
            intents: vec![AppIntent::BookmarkMessage {
                message_id: msg.message_id,
            }],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litecord_types::ids::{ConversationId, MessageId, UserId};

    #[test]
    fn produces_bookmark_intent() {
        let feature = LocalBookmarks;
        let msg_ctx = MessageActionContext {
            message_id: MessageId(9),
            conversation_id: ConversationId(1),
            author_id: UserId(1),
            content: String::new(),
            guild_id: None,
            authored_by_me: false,
        };
        let mut out = Vec::new();
        feature.message_actions(&msg_ctx, &FeatureContext::default(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].intents,
            vec![AppIntent::BookmarkMessage {
                message_id: MessageId(9)
            }]
        );
    }
}
