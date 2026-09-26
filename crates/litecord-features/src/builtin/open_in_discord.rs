//! Adds an "Open in Discord" action for content Litecord cannot fully render.

use litecord_types::capability::DiscordTarget;
use litecord_types::ids::ChannelId;

use crate::feature::{
    Feature, FeatureContext, FeatureMetadata, MessageAction, MessageActionContext,
};
use crate::intent::AppIntent;

#[derive(Debug, Default)]
pub struct OpenInDiscord;

impl Feature for OpenInDiscord {
    fn metadata(&self) -> FeatureMetadata {
        FeatureMetadata {
            id: "open_in_discord",
            name: "Open in Discord",
            description: "Deep-link a message into the official Discord client.",
            default_enabled: true,
        }
    }

    fn message_actions(
        &self,
        msg: &MessageActionContext,
        _ctx: &FeatureContext,
        out: &mut Vec<MessageAction>,
    ) {
        let target = DiscordTarget::Message {
            guild_id: msg.guild_id,
            // A conversation's id *is* the Discord channel id (see
            // `litecord_types::social::Conversation`).
            channel_id: ChannelId::new(msg.conversation_id.get()),
            message_id: msg.message_id,
        };
        out.push(MessageAction {
            id: "open_in_discord.open".to_string(),
            label: "Open in Discord".to_string(),
            group: "external".to_string(),
            intents: vec![AppIntent::OpenExternal { target }],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litecord_types::ids::{ConversationId, GuildId, MessageId, UserId};

    #[test]
    fn produces_open_external_intent_with_message_target() {
        let feature = OpenInDiscord;
        let msg_ctx = MessageActionContext {
            message_id: MessageId(5),
            conversation_id: ConversationId(3),
            author_id: UserId(1),
            content: String::new(),
            guild_id: Some(GuildId(2)),
            authored_by_me: false,
        };
        let mut out = Vec::new();
        feature.message_actions(&msg_ctx, &FeatureContext::default(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].intents,
            vec![AppIntent::OpenExternal {
                target: DiscordTarget::Message {
                    guild_id: Some(GuildId(2)),
                    channel_id: ChannelId::new(3),
                    message_id: MessageId(5),
                }
            }]
        );
    }
}
