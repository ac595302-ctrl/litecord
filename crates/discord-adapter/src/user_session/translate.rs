//! Account payload normalization; no I/O, secrets or database access.
use crate::bot::translate::{self as common, TranslateError};
use litecord_core::events::{DiscordEvent, HydrationKey};
use litecord_types::ids::*;
use litecord_types::social::*;
use serde_json::Value;
use std::sync::Arc;

fn id<T: std::str::FromStr>(v: &Value, key: &'static str) -> Result<T, TranslateError> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or(TranslateError::Missing(key))?
        .parse()
        .map_err(|_| TranslateError::Invalid(key))
}

pub fn conversation(raw: &Value) -> Result<Conversation, TranslateError> {
    let typ = raw
        .get("type")
        .and_then(Value::as_u64)
        .ok_or(TranslateError::Missing("type"))?;
    if typ != 1 && typ != 3 {
        return Err(TranslateError::Invalid("type"));
    }
    let recipients = raw.get("recipients").and_then(Value::as_array);
    let recipient_id = if typ == 1 {
        recipients
            .and_then(|r| r.first())
            .map(|r| id(r, "id"))
            .transpose()?
            .or_else(|| {
                raw.get("recipient_ids")
                    .and_then(Value::as_array)
                    .and_then(|r| r.first())
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse().ok())
            })
    } else {
        None
    };
    let title = raw.get("name").and_then(Value::as_str).map(Arc::from);
    Ok(Conversation {
        id: id(raw, "id")?,
        kind: if typ == 1 {
            ConversationKind::DirectMessage
        } else {
            ConversationKind::GroupDm
        },
        recipient_id,
        guild_id: None,
        lobby_id: None,
        title,
        last_message_id: raw
            .get("last_message_id")
            .and_then(Value::as_str)
            .and_then(|v| v.parse().ok()),
        last_activity_at: None,
    })
}

pub fn relationship(raw: &Value) -> Result<(Relationship, Option<User>), TranslateError> {
    let user = raw.get("user").and_then(|u| common::user(u).ok());
    let user_id = match &user {
        Some(u) => u.id,
        None => {
            if let Some(u) = raw.get("user") {
                id(u, "id")?
            } else {
                id(raw, "id")?
            }
        }
    };
    let discord = match raw.get("type").and_then(Value::as_u64) {
        Some(1) => RelationshipKind::Friend,
        Some(2) => RelationshipKind::Blocked,
        Some(3) => RelationshipKind::PendingIncoming,
        Some(4) => RelationshipKind::PendingOutgoing,
        _ => RelationshipKind::Implicit,
    };
    Ok((
        Relationship {
            user_id,
            discord,
            game: RelationshipKind::None,
            since: None,
        },
        user,
    ))
}

fn private_channel(raw: &Value) -> Result<Vec<DiscordEvent>, TranslateError> {
    let mut out = Vec::new();
    if let Some(users) = raw.get("recipients").and_then(Value::as_array) {
        for u in users {
            if let Ok(user) = common::user(u) {
                out.push(DiscordEvent::UserUpserted { user });
            }
        }
    }
    out.push(DiscordEvent::ConversationUpserted {
        conversation: conversation(raw)?,
    });
    Ok(out)
}

fn guild(raw: &Value) -> Result<Vec<DiscordEvent>, TranslateError> {
    if raw.get("unavailable").and_then(Value::as_bool) == Some(true) {
        return Ok(Vec::new());
    }
    let guild_id: GuildId = id(raw, "id")?;
    let mut flattened = raw.clone();
    if let (Some(props), Some(object)) = (
        raw.get("properties").and_then(Value::as_object),
        flattened.as_object_mut(),
    ) {
        for (key, value) in props {
            object.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    let mut out = match common::guild(&flattened) {
        Ok(guild) => vec![DiscordEvent::GuildUpserted { guild }],
        Err(_) => vec![DiscordEvent::Invalidated {
            key: HydrationKey::Guilds,
        }],
    };
    if let Some(channels) = raw.get("channels").and_then(Value::as_array) {
        for raw in channels {
            if let Ok(Some(mut channel)) = common::channel(raw, guild_id) {
                channel.capabilities.0 &= !ChannelCapabilities::WRITABLE.0;
                if let Some(conversation) = common::channel_conversation(&channel) {
                    out.push(DiscordEvent::ConversationUpserted { conversation });
                }
                out.push(DiscordEvent::ChannelUpserted { channel });
            } else {
                out.push(DiscordEvent::Invalidated {
                    key: HydrationKey::GuildChannels { guild_id },
                });
            }
        }
    }
    Ok(out)
}

pub fn dispatch(event: &str, raw: &Value) -> Result<Vec<DiscordEvent>, TranslateError> {
    match event {
        "READY" => {
            let mut out = vec![
                DiscordEvent::CurrentUser {
                    user: common::user(raw.get("user").ok_or(TranslateError::Missing("user"))?)?,
                },
                DiscordEvent::SessionChanged {
                    state: SessionState::Hydrating,
                },
            ];
            if let Some(users) = raw.get("users").and_then(Value::as_array) {
                for raw in users {
                    if let Ok(user) = common::user(raw) {
                        out.push(DiscordEvent::UserUpserted { user });
                    }
                }
            }
            if let Some(guilds) = raw.get("guilds").and_then(Value::as_array) {
                for g in guilds {
                    out.extend(guild(g)?);
                }
            }
            if let Some(channels) = raw.get("private_channels").and_then(Value::as_array) {
                for c in channels {
                    out.extend(private_channel(c)?);
                }
            }
            if let Some(relationships) = raw.get("relationships").and_then(Value::as_array) {
                for r in relationships {
                    let (relationship, user) = relationship(r)?;
                    out.push(DiscordEvent::RelationshipUpserted { relationship, user });
                }
            }
            out.push(DiscordEvent::SessionChanged {
                state: SessionState::Ready,
            });
            Ok(out)
        }
        "GUILD_CREATE" => guild(raw),
        "CHANNEL_CREATE" | "CHANNEL_UPDATE"
            if matches!(raw.get("type").and_then(Value::as_u64), Some(1 | 3)) =>
        {
            private_channel(raw)
        }
        "RELATIONSHIP_ADD" | "RELATIONSHIP_UPDATE" => {
            let (relationship, user) = relationship(raw)?;
            Ok(vec![DiscordEvent::RelationshipUpserted {
                relationship,
                user,
            }])
        }
        "RELATIONSHIP_REMOVE" => Ok(vec![DiscordEvent::RelationshipRemoved {
            user_id: id(raw, "id")?,
        }]),
        // A partial patch is not a full replacement. The backend fetches this
        // exact message asynchronously while socket control remains responsive.
        "MESSAGE_CREATE" => {
            let mut events = common::dispatch(event, raw)?;
            if let Some(nonce) = message_nonce(raw) {
                for event in &mut events {
                    if let DiscordEvent::MessageCreated { message } = event {
                        *event = DiscordEvent::MessageWriteObserved {
                            message: message.clone(),
                            nonce: nonce.clone(),
                            imported: false,
                        };
                    }
                }
            }
            Ok(events)
        }
        "MESSAGE_UPDATE" => Ok(vec![DiscordEvent::Invalidated {
            key: HydrationKey::DmConversation {
                conversation_id: id(raw, "channel_id")?,
            },
        }]),
        "CHANNEL_DELETE" if raw.get("guild_id").is_none() => Ok(vec![DiscordEvent::Invalidated {
            key: HydrationKey::DmSummaries,
        }]),
        _ => {
            let mut events = common::dispatch(event, raw)?;
            for event in &mut events {
                if let DiscordEvent::ChannelUpserted { channel } = event {
                    channel.capabilities.0 &= !ChannelCapabilities::WRITABLE.0;
                }
            }
            Ok(events)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn ready_contains_account_dm_and_relationships_without_bot_identity() {
        let events = dispatch("READY", &json!({"user":{"id":"1","username":"owner"},"guilds":[],"private_channels":[{"id":"2","type":1,"recipients":[{"id":"3","username":"friend"}]}],"relationships":[{"id":"3","type":1}]})).unwrap();
        assert!(events.iter().any(|e|matches!(e,DiscordEvent::ConversationUpserted{conversation} if conversation.id==ConversationId(2) && conversation.recipient_id==Some(UserId(3)))));
        assert!(matches!(
            events.last(),
            Some(DiscordEvent::SessionChanged {
                state: SessionState::Ready
            })
        ));
    }
    #[test]
    fn partial_edit_requests_refresh_without_erasing_content() {
        assert!(matches!(
            &dispatch(
                "MESSAGE_UPDATE",
                &json!({"id":"5","channel_id":"2","embeds":[]})
            )
            .unwrap()[0],
            DiscordEvent::Invalidated { .. }
        ));
    }
}

/// Bounded protocol nonce; it is an operation ID, never a credential.
pub fn message_nonce(raw: &Value) -> Option<String> {
    let nonce = match raw.get("nonce")? {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!nonce.is_empty() && nonce.len() <= 25).then_some(nonce)
}
