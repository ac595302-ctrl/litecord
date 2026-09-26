//! Pure translation from Discord API v10 JSON (gateway dispatches and REST
//! responses) into Litecord's normalized [`litecord_core::events::DiscordEvent`]
//! and [`litecord_types::social`] types.
//!
//! Every function here is a total, side-effect-free mapping from
//! [`serde_json::Value`] to a domain type (or a [`TranslateError`]). Nothing
//! in this module performs I/O, retries, or holds a token — the bot token
//! lives only in the transport layer that calls these functions.

use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use litecord_core::events::{DiscordEvent, HydrationKey};
use litecord_types::ids::{ChannelId, ConversationId, GuildId, MessageId, UserId};
use litecord_types::social::{
    Activity, Channel, ChannelAccess, ChannelCapabilities, ChannelKind, Conversation,
    ConversationKind, Guild, Message, MessageExtra, Presence, PresenceStatus, SessionState, User,
};

use super::time::parse_iso8601;

/// A JSON payload did not have the shape a translator expected.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TranslateError {
    #[error("missing field {0}")]
    Missing(&'static str),
    #[error("invalid field {0}")]
    Invalid(&'static str),
}

fn field<'a>(v: &'a Value, name: &'static str) -> Result<&'a Value, TranslateError> {
    match v.get(name) {
        Some(Value::Null) | None => Err(TranslateError::Missing(name)),
        Some(value) => Ok(value),
    }
}

fn str_field<'a>(v: &'a Value, name: &'static str) -> Result<&'a str, TranslateError> {
    field(v, name)?
        .as_str()
        .ok_or(TranslateError::Invalid(name))
}

fn opt_str_field<'a>(v: &'a Value, name: &'static str) -> Option<&'a str> {
    v.get(name).and_then(Value::as_str)
}

fn id_field<T: FromStr>(v: &Value, name: &'static str) -> Result<T, TranslateError> {
    str_field(v, name)?
        .parse()
        .map_err(|_| TranslateError::Invalid(name))
}

fn opt_id_field<T: FromStr>(v: &Value, name: &'static str) -> Result<Option<T>, TranslateError> {
    match v.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => s
            .parse()
            .map(Some)
            .map_err(|_| TranslateError::Invalid(name)),
        Some(_) => Err(TranslateError::Invalid(name)),
    }
}

/// Translates a Discord user object into a [`User`].
///
/// The CDN avatar URL is derived from the `avatar` hash:
/// `https://cdn.discordapp.com/avatars/{id}/{hash}.png`.
pub fn user(v: &Value) -> Result<User, TranslateError> {
    let id: UserId = id_field(v, "id")?;
    let username = str_field(v, "username")?;
    let global_name = opt_str_field(v, "global_name").filter(|s| !s.is_empty());
    let avatar_url = opt_str_field(v, "avatar")
        .filter(|s| !s.is_empty())
        .map(|hash| {
            Arc::from(format!(
                "https://cdn.discordapp.com/avatars/{id}/{hash}.png"
            ))
        });
    let is_bot = v.get("bot").and_then(Value::as_bool).unwrap_or(false);

    Ok(User {
        id,
        username: Arc::from(username),
        global_name: global_name.map(Arc::from),
        avatar_url,
        is_bot,
        is_provisional: false,
    })
}

/// Translates a Discord guild object into a [`Guild`].
///
/// The CDN icon URL is derived from the `icon` hash:
/// `https://cdn.discordapp.com/icons/{id}/{hash}.png`.
pub(crate) fn guild(v: &Value) -> Result<Guild, TranslateError> {
    let id: GuildId = id_field(v, "id")?;
    let name = str_field(v, "name")?;
    let icon_url = opt_str_field(v, "icon")
        .filter(|s| !s.is_empty())
        .map(|hash| Arc::from(format!("https://cdn.discordapp.com/icons/{id}/{hash}.png")));

    Ok(Guild {
        id,
        name: Arc::from(name),
        icon_url,
    })
}

/// Maps a Discord channel `type` integer to a [`ChannelKind`], or `None` for
/// DM (1) and group DM (3) types, which are not guild channels.
fn channel_kind(type_num: u64) -> Option<ChannelKind> {
    Some(match type_num {
        0 => ChannelKind::Text,
        2 => ChannelKind::Voice,
        4 => ChannelKind::Category,
        5 => ChannelKind::Announcement,
        10..=12 => ChannelKind::Thread,
        13 => ChannelKind::Stage,
        15 => ChannelKind::Forum,
        1 | 3 => return None,
        _ => ChannelKind::Other,
    })
}

fn channel_capabilities(kind: &ChannelKind) -> ChannelCapabilities {
    match kind {
        ChannelKind::Text | ChannelKind::Announcement => {
            ChannelCapabilities::DISCOVERABLE
                | ChannelCapabilities::READABLE
                | ChannelCapabilities::WRITABLE
                | ChannelCapabilities::OPEN_EXTERNAL
        }
        ChannelKind::Voice | ChannelKind::Stage => {
            ChannelCapabilities::VOICE
                | ChannelCapabilities::DISCOVERABLE
                | ChannelCapabilities::OPEN_EXTERNAL
        }
        _ => ChannelCapabilities::DISCOVERABLE | ChannelCapabilities::OPEN_EXTERNAL,
    }
}

/// Translates a Discord channel object into a [`Channel`], or `Ok(None)` for
/// DM/group-DM channel types (which have no guild and are not represented as
/// guild channels).
pub fn channel(v: &Value, guild_id: GuildId) -> Result<Option<Channel>, TranslateError> {
    let type_num = field(v, "type")?
        .as_u64()
        .ok_or(TranslateError::Invalid("type"))?;
    let Some(kind) = channel_kind(type_num) else {
        return Ok(None);
    };

    let id: ChannelId = id_field(v, "id")?;
    let name = str_field(v, "name")?;
    let position = v.get("position").and_then(Value::as_i64).unwrap_or(0) as i32;
    let parent_id = opt_id_field(v, "parent_id")?;
    let capabilities = channel_capabilities(&kind);

    Ok(Some(Channel {
        id,
        guild_id,
        name: Arc::from(name),
        kind,
        position,
        parent_id,
        access: ChannelAccess::Native,
        capabilities,
    }))
}

/// Derives the [`Conversation`] a channel represents, for the channel kinds
/// that carry messages (text, announcement, thread, forum). Voice, stage and
/// category channels have no associated conversation.
pub fn channel_conversation(c: &Channel) -> Option<Conversation> {
    match c.kind {
        ChannelKind::Text
        | ChannelKind::Announcement
        | ChannelKind::Thread
        | ChannelKind::Forum => Some(Conversation {
            id: ConversationId(c.id.get()),
            kind: ConversationKind::GuildChannel,
            recipient_id: None,
            guild_id: Some(c.guild_id),
            lobby_id: None,
            title: Some(Arc::from(format!("#{}", c.name))),
            last_message_id: None,
            last_activity_at: None,
        }),
        _ => None,
    }
}

fn message_extras(v: &Value) -> Vec<MessageExtra> {
    let mut extras = Vec::new();

    if let Some(attachments) = v.get("attachments").and_then(Value::as_array) {
        for a in attachments {
            let filename = a
                .get("filename")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let content_type = a
                .get("content_type")
                .and_then(Value::as_str)
                .map(str::to_string);
            let size_bytes = a.get("size").and_then(Value::as_u64).unwrap_or(0);
            extras.push(MessageExtra::Attachment {
                filename,
                content_type,
                size_bytes,
            });
        }
    }

    if let Some(embeds) = v.get("embeds").and_then(Value::as_array) {
        for e in embeds {
            let title = e.get("title").and_then(Value::as_str).map(str::to_string);
            let url = e.get("url").and_then(Value::as_str).map(str::to_string);
            extras.push(MessageExtra::Embed { title, url });
        }
    }

    if let Some(stickers) = v.get("sticker_items").and_then(Value::as_array) {
        for s in stickers {
            let name = s
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            extras.push(MessageExtra::Sticker { name });
        }
    }

    if v.get("poll").is_some_and(|p| !p.is_null()) {
        extras.push(MessageExtra::Poll);
    }

    extras
}

/// Translates a Discord message object into a [`Message`].
///
/// `content`, `timestamp` and `author.id` are required; everything else
/// (edits, replies, attachments, embeds, stickers, polls) is optional.
pub fn message(v: &Value) -> Result<Message, TranslateError> {
    let id: MessageId = id_field(v, "id")?;
    let conversation_id: ConversationId = id_field(v, "channel_id")?;
    let author = field(v, "author")?;
    let author_id: UserId = id_field(author, "id")?;
    let content = str_field(v, "content")?;

    let sent_at =
        parse_iso8601(str_field(v, "timestamp")?).ok_or(TranslateError::Invalid("timestamp"))?;
    let edited_at = match v.get("edited_timestamp") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            Some(parse_iso8601(s).ok_or(TranslateError::Invalid("edited_timestamp"))?)
        }
        Some(_) => return Err(TranslateError::Invalid("edited_timestamp")),
    };

    let reply_to = v
        .get("message_reference")
        .and_then(|r| r.get("message_id"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<MessageId>().ok());

    Ok(Message {
        id,
        conversation_id,
        author_id,
        content: Arc::from(content),
        sent_at,
        edited_at,
        reply_to,
        extras: message_extras(v),
    })
}

fn presence_status(s: &str) -> PresenceStatus {
    match s {
        "online" => PresenceStatus::Online,
        "idle" => PresenceStatus::Idle,
        "dnd" => PresenceStatus::DoNotDisturb,
        "offline" | "invisible" => PresenceStatus::Offline,
        _ => PresenceStatus::Unknown,
    }
}

fn first_activity_name(v: &Value) -> Option<Activity> {
    v.get("activities")?
        .as_array()?
        .first()?
        .get("name")?
        .as_str()
        .map(|name| Activity {
            name: name.to_string(),
            details: None,
            state: None,
        })
}

/// Translates a `GUILD_CREATE` payload's `channels` array (skipping DM-type
/// entries) plus the guild-upsert and member-upsert events it implies.
fn guild_create(data: &Value) -> Result<Vec<DiscordEvent>, TranslateError> {
    if data.get("unavailable").and_then(Value::as_bool) == Some(true) {
        // An outage placeholder, not real guild data: nothing to observe.
        return Ok(vec![]);
    }

    let g = guild(data)?;
    let guild_id = g.id;
    let mut events = vec![DiscordEvent::GuildUpserted { guild: g }];

    let mut channels = Vec::new();
    if let Some(raw_channels) = data.get("channels").and_then(Value::as_array) {
        for raw in raw_channels {
            if let Some(c) = channel(raw, guild_id)? {
                channels.push(c);
            }
        }
    }
    events.push(DiscordEvent::GuildChannelsSnapshot {
        guild_id,
        channels: channels.clone(),
    });
    for c in &channels {
        if let Some(conversation) = channel_conversation(c) {
            events.push(DiscordEvent::ConversationUpserted { conversation });
        }
    }

    if let Some(members) = data.get("members").and_then(Value::as_array) {
        for member in members.iter().take(1000) {
            if let Some(user_v) = member.get("user") {
                events.push(DiscordEvent::UserUpserted {
                    user: user(user_v)?,
                });
            }
        }
    }

    Ok(events)
}

fn channel_upsert(data: &Value) -> Result<Vec<DiscordEvent>, TranslateError> {
    let guild_id: GuildId = id_field(data, "guild_id")?;
    match channel(data, guild_id)? {
        Some(c) => {
            let mut events = vec![DiscordEvent::ChannelUpserted { channel: c.clone() }];
            if let Some(conversation) = channel_conversation(&c) {
                events.push(DiscordEvent::ConversationUpserted { conversation });
            }
            Ok(events)
        }
        None => Ok(vec![]),
    }
}

fn message_delete_bulk(data: &Value) -> Result<Vec<DiscordEvent>, TranslateError> {
    let conversation_id: ConversationId = id_field(data, "channel_id")?;
    let ids = field(data, "ids")?
        .as_array()
        .ok_or(TranslateError::Invalid("ids"))?;

    let mut events = Vec::with_capacity(ids.len());
    for id_v in ids {
        let message_id: MessageId = id_v
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(TranslateError::Invalid("ids"))?;
        events.push(DiscordEvent::MessageDeleted {
            message_id,
            conversation_id,
        });
    }
    Ok(events)
}

fn presence_update(data: &Value) -> Result<Vec<DiscordEvent>, TranslateError> {
    let user_id: UserId = id_field(field(data, "user")?, "id")?;
    let status = presence_status(str_field(data, "status")?);
    let activity = first_activity_name(data);
    Ok(vec![DiscordEvent::PresenceChanged {
        user_id,
        presence: Presence { status, activity },
    }])
}

/// Translates one gateway DISPATCH (op 0) event into zero or more normalized
/// [`DiscordEvent`]s. Unknown event names translate to `Ok(vec![])` rather
/// than an error, so the caller can safely ignore gateway events this
/// adapter does not yet model.
pub fn dispatch(event: &str, data: &Value) -> Result<Vec<DiscordEvent>, TranslateError> {
    match event {
        "READY" => Ok(vec![
            DiscordEvent::CurrentUser {
                user: user(field(data, "user")?)?,
            },
            DiscordEvent::SessionChanged {
                state: SessionState::Ready,
            },
        ]),
        "RESUMED" => Ok(vec![DiscordEvent::SessionChanged {
            state: SessionState::Ready,
        }]),
        "GUILD_CREATE" => guild_create(data),
        "GUILD_UPDATE" => Ok(vec![DiscordEvent::GuildUpserted {
            guild: guild(data)?,
        }]),
        "GUILD_DELETE" => {
            if data.get("unavailable").and_then(Value::as_bool) == Some(true) {
                // Outage, not a real departure: nothing to do.
                Ok(vec![])
            } else {
                Ok(vec![DiscordEvent::GuildRemoved {
                    guild_id: id_field(data, "id")?,
                }])
            }
        }
        "CHANNEL_CREATE" | "CHANNEL_UPDATE" | "THREAD_CREATE" | "THREAD_UPDATE" => {
            channel_upsert(data)
        }
        "CHANNEL_DELETE" => Ok(vec![DiscordEvent::Invalidated {
            key: HydrationKey::GuildChannels {
                guild_id: id_field(data, "guild_id")?,
            },
        }]),
        "MESSAGE_CREATE" => Ok(vec![
            DiscordEvent::UserUpserted {
                user: user(field(data, "author")?)?,
            },
            DiscordEvent::MessageCreated {
                message: message(data)?,
            },
        ]),
        "MESSAGE_UPDATE" => {
            if data.get("content").and_then(Value::as_str).is_some() {
                Ok(vec![DiscordEvent::MessageUpdated {
                    message: message(data)?,
                }])
            } else {
                // Partial update (e.g. embeds-only): we cannot reconstruct
                // the full message, so ask hydration to refetch it.
                Ok(vec![DiscordEvent::Invalidated {
                    key: HydrationKey::DmConversation {
                        conversation_id: id_field(data, "channel_id")?,
                    },
                }])
            }
        }
        "MESSAGE_DELETE" => Ok(vec![DiscordEvent::MessageDeleted {
            message_id: id_field(data, "id")?,
            conversation_id: id_field(data, "channel_id")?,
        }]),
        "MESSAGE_DELETE_BULK" => message_delete_bulk(data),
        "PRESENCE_UPDATE" => presence_update(data),
        "GUILD_MEMBER_ADD" | "GUILD_MEMBER_UPDATE" => Ok(vec![DiscordEvent::UserUpserted {
            user: user(field(data, "user")?)?,
        }]),
        _ => Ok(vec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ready_yields_current_user_and_session_ready() {
        let data = json!({
            "user": { "id": "1", "username": "bot", "bot": true },
            "session_id": "abc",
        });
        let events = dispatch("READY", &data).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            DiscordEvent::CurrentUser {
                user: User {
                    id: UserId(1),
                    username: Arc::from("bot"),
                    global_name: None,
                    avatar_url: None,
                    is_bot: true,
                    is_provisional: false,
                }
            }
        );
        assert_eq!(
            events[1],
            DiscordEvent::SessionChanged {
                state: SessionState::Ready
            }
        );
    }

    #[test]
    fn resumed_yields_session_ready() {
        let events = dispatch("RESUMED", &json!({})).unwrap();
        assert_eq!(
            events,
            vec![DiscordEvent::SessionChanged {
                state: SessionState::Ready
            }]
        );
    }

    fn sample_channels() -> Value {
        json!([
            { "id": "10", "type": 0, "name": "general", "position": 0 },
            { "id": "11", "type": 2, "name": "voice-chat", "position": 1 },
            { "id": "12", "type": 4, "name": "category", "position": 2 },
            { "id": "13", "type": 5, "name": "announcements", "position": 3 },
            { "id": "14", "type": 15, "name": "forum", "position": 4 },
            { "id": "15", "type": 1, "name": "should-be-skipped" },
        ])
    }

    #[test]
    fn guild_create_produces_guild_channels_conversations_and_members() {
        let data = json!({
            "id": "100",
            "name": "My Guild",
            "icon": "abcd",
            "channels": sample_channels(),
            "members": [
                { "user": { "id": "200", "username": "alice" } },
                { "user": { "id": "201", "username": "bob" } },
            ],
        });

        let events = dispatch("GUILD_CREATE", &data).unwrap();

        let DiscordEvent::GuildUpserted { guild } = &events[0] else {
            panic!("expected GuildUpserted");
        };
        assert_eq!(guild.id, GuildId(100));
        assert_eq!(
            guild.icon_url.as_deref(),
            Some("https://cdn.discordapp.com/icons/100/abcd.png")
        );

        let DiscordEvent::GuildChannelsSnapshot { guild_id, channels } = &events[1] else {
            panic!("expected GuildChannelsSnapshot");
        };
        assert_eq!(*guild_id, GuildId(100));
        // 5 non-DM channels: the type=1 entry is skipped entirely.
        assert_eq!(channels.len(), 5);
        assert!(channels.iter().all(|c| c.id != ChannelId(15)));

        // Conversations only for text/announcement/thread/forum channels:
        // general (text), announcements, and forum — not voice or category.
        let conversation_ids: Vec<ConversationId> = events[2..]
            .iter()
            .take_while(|e| matches!(e, DiscordEvent::ConversationUpserted { .. }))
            .map(|e| match e {
                DiscordEvent::ConversationUpserted { conversation } => conversation.id,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            conversation_ids,
            vec![ConversationId(10), ConversationId(13), ConversationId(14)]
        );

        let user_events: Vec<&DiscordEvent> = events
            .iter()
            .filter(|e| matches!(e, DiscordEvent::UserUpserted { .. }))
            .collect();
        assert_eq!(user_events.len(), 2);
    }

    #[test]
    fn unavailable_guild_create_yields_no_events() {
        let data = json!({ "id": "100", "unavailable": true });
        assert_eq!(dispatch("GUILD_CREATE", &data).unwrap(), vec![]);
    }

    #[test]
    fn guild_delete_outage_yields_no_events() {
        let data = json!({ "id": "100", "unavailable": true });
        assert_eq!(dispatch("GUILD_DELETE", &data).unwrap(), vec![]);
    }

    #[test]
    fn guild_delete_real_departure_yields_guild_removed() {
        let data = json!({ "id": "100" });
        assert_eq!(
            dispatch("GUILD_DELETE", &data).unwrap(),
            vec![DiscordEvent::GuildRemoved {
                guild_id: GuildId(100)
            }]
        );
    }

    #[test]
    fn message_create_with_attachment_embed_and_reply() {
        let data = json!({
            "id": "500",
            "channel_id": "10",
            "author": { "id": "200", "username": "alice" },
            "content": "look at this",
            "timestamp": "2026-09-24T15:00:00.000000+00:00",
            "message_reference": { "message_id": "499" },
            "attachments": [
                { "filename": "a.png", "content_type": "image/png", "size": 1024 }
            ],
            "embeds": [ { "title": "a link", "url": "https://example.invalid" } ],
        });

        let events = dispatch("MESSAGE_CREATE", &data).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], DiscordEvent::UserUpserted { .. }));
        let DiscordEvent::MessageCreated { message } = &events[1] else {
            panic!("expected MessageCreated");
        };
        assert_eq!(message.id, MessageId(500));
        assert_eq!(message.reply_to, Some(MessageId(499)));
        assert_eq!(message.extras.len(), 2);
        assert!(matches!(
            message.extras[0],
            MessageExtra::Attachment {
                size_bytes: 1024,
                ..
            }
        ));
        assert!(matches!(message.extras[1], MessageExtra::Embed { .. }));
    }

    #[test]
    fn message_update_with_content_yields_message_updated() {
        let data = json!({
            "id": "500",
            "channel_id": "10",
            "author": { "id": "200", "username": "alice" },
            "content": "edited",
            "timestamp": "2026-09-24T15:00:00Z",
            "edited_timestamp": "2026-09-24T15:05:00Z",
        });
        let events = dispatch("MESSAGE_UPDATE", &data).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], DiscordEvent::MessageUpdated { .. }));
    }

    #[test]
    fn partial_message_update_without_content_invalidates_conversation() {
        let data = json!({ "id": "500", "channel_id": "10" });
        let events = dispatch("MESSAGE_UPDATE", &data).unwrap();
        assert_eq!(
            events,
            vec![DiscordEvent::Invalidated {
                key: HydrationKey::DmConversation {
                    conversation_id: ConversationId(10)
                }
            }]
        );
    }

    #[test]
    fn message_delete_bulk_yields_one_event_per_id() {
        let data = json!({ "channel_id": "10", "ids": ["1", "2", "3"] });
        let events = dispatch("MESSAGE_DELETE_BULK", &data).unwrap();
        assert_eq!(
            events,
            vec![
                DiscordEvent::MessageDeleted {
                    message_id: MessageId(1),
                    conversation_id: ConversationId(10)
                },
                DiscordEvent::MessageDeleted {
                    message_id: MessageId(2),
                    conversation_id: ConversationId(10)
                },
                DiscordEvent::MessageDeleted {
                    message_id: MessageId(3),
                    conversation_id: ConversationId(10)
                },
            ]
        );
    }

    #[test]
    fn presence_update_maps_status_and_first_activity() {
        let data = json!({
            "user": { "id": "200" },
            "status": "dnd",
            "activities": [ { "name": "Debugging" }, { "name": "ignored" } ],
        });
        let events = dispatch("PRESENCE_UPDATE", &data).unwrap();
        assert_eq!(
            events,
            vec![DiscordEvent::PresenceChanged {
                user_id: UserId(200),
                presence: Presence {
                    status: PresenceStatus::DoNotDisturb,
                    activity: Some(Activity {
                        name: "Debugging".to_string(),
                        details: None,
                        state: None,
                    }),
                }
            }]
        );
    }

    #[test]
    fn unknown_event_yields_no_events() {
        assert_eq!(dispatch("SOME_FUTURE_EVENT", &json!({})).unwrap(), vec![]);
    }

    #[test]
    fn channel_create_upserts_channel_and_conversation() {
        let data = json!({ "id": "10", "guild_id": "100", "type": 0, "name": "general" });
        let events = dispatch("CHANNEL_CREATE", &data).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], DiscordEvent::ChannelUpserted { .. }));
        assert!(matches!(
            events[1],
            DiscordEvent::ConversationUpserted { .. }
        ));
    }

    #[test]
    fn channel_create_dm_type_yields_no_events() {
        let data = json!({ "id": "10", "guild_id": "100", "type": 1 });
        assert_eq!(dispatch("CHANNEL_CREATE", &data).unwrap(), vec![]);
    }

    #[test]
    fn channel_delete_invalidates_guild_channels() {
        let data = json!({ "id": "10", "guild_id": "100", "type": 0, "name": "general" });
        assert_eq!(
            dispatch("CHANNEL_DELETE", &data).unwrap(),
            vec![DiscordEvent::Invalidated {
                key: HydrationKey::GuildChannels {
                    guild_id: GuildId(100)
                }
            }]
        );
    }

    #[test]
    fn missing_required_field_is_a_translate_error() {
        let err = user(&json!({ "id": "1" })).unwrap_err();
        assert_eq!(err, TranslateError::Missing("username"));
    }

    #[test]
    fn non_string_id_is_invalid() {
        let err = user(&json!({ "id": 1, "username": "a" })).unwrap_err();
        assert_eq!(err, TranslateError::Invalid("id"));
    }

    #[test]
    fn channel_conversation_only_for_text_like_kinds() {
        let text = Channel {
            id: ChannelId(1),
            guild_id: GuildId(1),
            name: Arc::from("general"),
            kind: ChannelKind::Text,
            position: 0,
            parent_id: None,
            access: ChannelAccess::Native,
            capabilities: ChannelCapabilities::empty(),
        };
        let conv = channel_conversation(&text).unwrap();
        assert_eq!(conv.title.as_deref(), Some("#general"));

        let voice = Channel {
            kind: ChannelKind::Voice,
            ..text
        };
        assert!(channel_conversation(&voice).is_none());
    }
}
