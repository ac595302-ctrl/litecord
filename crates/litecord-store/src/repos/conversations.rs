//! Conversations: DMs, group DMs, lobbies and linked/native guild channels.
//!
//! [`upsert`] implements "observe, don't regress" merge semantics: activity
//! only moves forward in time, a known kind is never demoted back to
//! `unknown`, `agent_visibility` is a user preference and is never touched by
//! observation, and a `NULL` title/recipient never clobbers a known one.

use rusqlite::{params, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::Origin;
use litecord_types::social::{Conversation, ConversationKind};
use litecord_types::trust::AgentVisibility;
use litecord_types::{ConversationId, GuildId, LobbyId, MessageId, Revision, Timestamp, UserId};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::sql::{col_err, ts};

/// A stored conversation row plus bookkeeping.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ConversationRecord {
    pub conversation: Conversation,
    pub visibility: Option<AgentVisibility>,
    pub origin: Origin,
    pub observed_at: Timestamp,
    pub revision: Revision,
}

const COLUMNS: &str = "id, kind, recipient_id, guild_id, lobby_id, title, last_message_id, \
     last_activity_at, agent_visibility, origin, observed_at, revision";

fn map_row(row: &Row<'_>) -> rusqlite::Result<ConversationRecord> {
    let id = ConversationId::from_sql(row.get(0)?);
    let kind_s: String = row.get(1)?;
    let kind = ConversationKind::parse(&kind_s).map_err(|e| col_err(1, e))?;
    let recipient_id: Option<i64> = row.get(2)?;
    let guild_id: Option<i64> = row.get(3)?;
    let lobby_id: Option<i64> = row.get(4)?;
    let title: Option<String> = row.get(5)?;
    let last_message_id: Option<i64> = row.get(6)?;
    let last_activity_at: Option<i64> = row.get(7)?;
    let visibility_s: Option<String> = row.get(8)?;
    let visibility = visibility_s
        .map(|s| AgentVisibility::parse(&s).map_err(|e| col_err(8, e)))
        .transpose()?;
    let origin_s: String = row.get(9)?;
    let origin = crate::sql::origin(9, &origin_s)?;
    let observed_at = Timestamp::from_millis(row.get(10)?);
    let revision = crate::sql::rev(row.get(11)?);

    Ok(ConversationRecord {
        conversation: Conversation {
            id,
            kind,
            recipient_id: recipient_id.map(UserId::from_sql),
            guild_id: guild_id.map(GuildId::from_sql),
            lobby_id: lobby_id.map(LobbyId::from_sql),
            title: title.map(Into::into),
            last_message_id: last_message_id.map(MessageId::from_sql),
            last_activity_at: ts(last_activity_at),
        },
        visibility,
        origin,
        observed_at,
        revision,
    })
}

/// Insert or merge-update a conversation. See module docs for the merge
/// rules. Emits [`UnifiedEvent::ConversationObserved`] if anything changed.
pub fn upsert(
    tx: &WriteTx<'_>,
    conv: &Conversation,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let n = tx.execute(
        "INSERT INTO conversations
             (id, kind, recipient_id, guild_id, lobby_id, title, last_message_id, last_activity_at, agent_visibility, origin, observed_at, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11)
         ON CONFLICT(id) DO UPDATE SET
             kind = CASE WHEN excluded.kind != 'unknown' THEN excluded.kind ELSE conversations.kind END,
             recipient_id = COALESCE(excluded.recipient_id, conversations.recipient_id),
             guild_id = COALESCE(excluded.guild_id, conversations.guild_id),
             lobby_id = COALESCE(excluded.lobby_id, conversations.lobby_id),
             title = COALESCE(excluded.title, conversations.title),
             last_message_id = CASE
                 WHEN excluded.last_activity_at IS NOT NULL
                      AND (conversations.last_activity_at IS NULL OR excluded.last_activity_at > conversations.last_activity_at)
                 THEN excluded.last_message_id ELSE conversations.last_message_id END,
             last_activity_at = CASE
                 WHEN excluded.last_activity_at IS NOT NULL
                      AND (conversations.last_activity_at IS NULL OR excluded.last_activity_at > conversations.last_activity_at)
                 THEN excluded.last_activity_at ELSE conversations.last_activity_at END,
             origin = excluded.origin,
             observed_at = excluded.observed_at,
             revision = excluded.revision
         WHERE
             (CASE WHEN excluded.kind != 'unknown' THEN excluded.kind ELSE conversations.kind END) IS NOT conversations.kind
             OR COALESCE(excluded.recipient_id, conversations.recipient_id) IS NOT conversations.recipient_id
             OR COALESCE(excluded.guild_id, conversations.guild_id) IS NOT conversations.guild_id
             OR COALESCE(excluded.lobby_id, conversations.lobby_id) IS NOT conversations.lobby_id
             OR COALESCE(excluded.title, conversations.title) IS NOT conversations.title
             OR (CASE
                     WHEN excluded.last_activity_at IS NOT NULL
                          AND (conversations.last_activity_at IS NULL OR excluded.last_activity_at > conversations.last_activity_at)
                     THEN excluded.last_message_id ELSE conversations.last_message_id END) IS NOT conversations.last_message_id
             OR (CASE
                     WHEN excluded.last_activity_at IS NOT NULL
                          AND (conversations.last_activity_at IS NULL OR excluded.last_activity_at > conversations.last_activity_at)
                     THEN excluded.last_activity_at ELSE conversations.last_activity_at END) IS NOT conversations.last_activity_at",
        params![
            conv.id.to_sql(),
            conv.kind.as_str(),
            conv.recipient_id.map(|u| u.to_sql()),
            conv.guild_id.map(|g| g.to_sql()),
            conv.lobby_id.map(|l| l.to_sql()),
            conv.title.as_deref(),
            conv.last_message_id.map(|m| m.to_sql()),
            conv.last_activity_at.map(Timestamp::as_millis),
            origin.as_str(),
            observed_at.as_millis(),
            tx.revision().get() as i64,
        ],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(
            UnifiedEvent::ConversationObserved {
                conversation_id: conv.id,
            },
            origin,
        )?;
    }
    Ok(changed)
}

/// Insert a placeholder conversation (`kind = unknown`) if `id` is unknown.
/// Emits nothing.
pub fn ensure(
    tx: &WriteTx<'_>,
    id: ConversationId,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<bool> {
    let n = tx.execute(
        "INSERT OR IGNORE INTO conversations (id, kind, origin, observed_at, revision)
         VALUES (?1, 'unknown', ?2, ?3, ?4)",
        params![
            id.to_sql(),
            origin.as_str(),
            observed_at.as_millis(),
            tx.revision().get() as i64,
        ],
    )?;
    Ok(n > 0)
}

/// Advance `last_activity_at`/`last_message_id` only if `at` is newer than
/// what is stored. Emits nothing (callers, e.g. `messages::upsert`, emit
/// their own events).
pub fn touch_activity(
    tx: &WriteTx<'_>,
    id: ConversationId,
    message_id: MessageId,
    at: Timestamp,
) -> StoreResult<bool> {
    let n = tx.execute(
        "UPDATE conversations SET last_message_id = ?1, last_activity_at = ?2, revision = ?3
         WHERE id = ?4 AND (last_activity_at IS NULL OR ?2 > last_activity_at)",
        params![
            message_id.to_sql(),
            at.as_millis(),
            tx.revision().get() as i64,
            id.to_sql(),
        ],
    )?;
    Ok(n > 0)
}

/// Look up one conversation.
pub fn get(conn: &Connection, id: ConversationId) -> StoreResult<Option<ConversationRecord>> {
    let sql = format!("SELECT {COLUMNS} FROM conversations WHERE id = ?1");
    Ok(conn
        .query_row(&sql, params![id.to_sql()], map_row)
        .optional()?)
}

/// Most recently active conversations first (nulls last).
pub fn list_recent(
    conn: &Connection,
    limit: u32,
    offset: u32,
) -> StoreResult<Vec<ConversationRecord>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM conversations
         ORDER BY last_activity_at IS NULL, last_activity_at DESC
         LIMIT ?1 OFFSET ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit, offset], map_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Most recent conversations that are (or are not, with `guild = false`)
/// server channels. Lets the DM list be filled from DMs alone, so busy
/// server channels cannot push every DM past the list limit.
pub fn list_recent_split(
    conn: &Connection,
    guild: bool,
    limit: u32,
) -> StoreResult<Vec<ConversationRecord>> {
    let op = if guild { "=" } else { "<>" };
    let sql = format!(
        "SELECT {COLUMNS} FROM conversations
         WHERE kind {op} 'guild_channel'
         ORDER BY last_activity_at IS NULL, last_activity_at DESC
         LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit], map_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Find the DM conversation with `user_id`, if any.
pub fn find_dm_by_recipient(
    conn: &Connection,
    user_id: UserId,
) -> StoreResult<Option<ConversationRecord>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM conversations WHERE kind = 'dm' AND recipient_id = ?1 LIMIT 1"
    );
    Ok(conn
        .query_row(&sql, params![user_id.to_sql()], map_row)
        .optional()?)
}

/// Set (or clear) the explicit agent-visibility override for a conversation.
/// Always recorded with [`Origin::UserProvided`]. Emits
/// [`UnifiedEvent::VisibilityChanged`] if it changed.
pub fn set_visibility(
    tx: &WriteTx<'_>,
    id: ConversationId,
    visibility: Option<AgentVisibility>,
) -> StoreResult<bool> {
    let v = visibility.map(AgentVisibility::as_str);
    let n = tx.execute(
        "UPDATE conversations SET agent_visibility = ?1, revision = ?2
         WHERE id = ?3 AND agent_visibility IS NOT ?1",
        params![v, tx.revision().get() as i64, id.to_sql()],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(
            UnifiedEvent::VisibilityChanged {
                conversation_id: id,
            },
            Origin::UserProvided,
        )?;
    }
    Ok(changed)
}

/// The visibility that applies to a conversation: its explicit override, or
/// `default` (also `default` for a conversation that does not exist).
pub fn effective_visibility(
    conn: &Connection,
    id: ConversationId,
    default: AgentVisibility,
) -> StoreResult<AgentVisibility> {
    Ok(get(conn, id)?.and_then(|r| r.visibility).unwrap_or(default))
}

/// Conversation ids with an explicit visibility setting equal to `v`.
pub fn ids_with_visibility(
    conn: &Connection,
    v: AgentVisibility,
) -> StoreResult<Vec<ConversationId>> {
    let mut stmt = conn.prepare("SELECT id FROM conversations WHERE agent_visibility = ?1")?;
    let rows = stmt.query_map(params![v.as_str()], |r| {
        Ok(ConversationId::from_sql(r.get(0)?))
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn conv(id: u64, kind: ConversationKind, last_activity: Option<i64>) -> Conversation {
        Conversation {
            id: ConversationId(id),
            kind,
            recipient_id: None,
            guild_id: None,
            lobby_id: None,
            title: None,
            last_message_id: last_activity.map(|_| MessageId(999)),
            last_activity_at: last_activity.map(Timestamp::from_millis),
        }
    }

    #[test]
    fn upsert_does_not_regress_activity() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &conv(1, ConversationKind::DirectMessage, Some(100)),
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        let c = db
            .write(|tx| {
                upsert(
                    tx,
                    &conv(1, ConversationKind::DirectMessage, Some(50)),
                    Origin::Synthetic,
                    Timestamp::from_millis(2),
                )
            })
            .unwrap();
        assert!(!c.changed());
        let rec = db.read(|r| get(r, ConversationId(1))).unwrap().unwrap();
        assert_eq!(
            rec.conversation.last_activity_at,
            Some(Timestamp::from_millis(100))
        );
    }

    #[test]
    fn busy_server_channels_do_not_crowd_out_dms() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| -> StoreResult<()> {
            // An old DM, then many channels with newer activity.
            upsert(
                tx,
                &conv(1, ConversationKind::DirectMessage, Some(10)),
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )?;
            for i in 0..5u64 {
                let mut c = conv(
                    100 + i,
                    ConversationKind::GuildChannel,
                    Some(1_000 + i as i64),
                );
                c.guild_id = None;
                upsert(tx, &c, Origin::Synthetic, Timestamp::from_millis(1))?;
            }
            Ok(())
        })
        .unwrap();
        let dms = db.read(|r| list_recent_split(r, false, 1)).unwrap();
        assert_eq!(dms.len(), 1);
        assert_eq!(dms[0].conversation.id, ConversationId(1));
        let channels = db.read(|r| list_recent_split(r, true, 2)).unwrap();
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].conversation.id, ConversationId(104));
    }

    #[test]
    fn upsert_never_demotes_known_kind() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            ensure(
                tx,
                ConversationId(1),
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &conv(1, ConversationKind::DirectMessage, None),
                Origin::Synthetic,
                Timestamp::from_millis(2),
            )
        })
        .unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &conv(1, ConversationKind::Unknown, None),
                Origin::Synthetic,
                Timestamp::from_millis(3),
            )
        })
        .unwrap();
        let rec = db.read(|r| get(r, ConversationId(1))).unwrap().unwrap();
        assert_eq!(rec.conversation.kind, ConversationKind::DirectMessage);
    }

    #[test]
    fn visibility_survives_upsert() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &conv(1, ConversationKind::DirectMessage, Some(1)),
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        db.write(|tx| set_visibility(tx, ConversationId(1), Some(AgentVisibility::Hidden)))
            .unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &conv(1, ConversationKind::DirectMessage, Some(2)),
                Origin::Synthetic,
                Timestamp::from_millis(2),
            )
        })
        .unwrap();
        let v = db
            .read(|r| effective_visibility(r, ConversationId(1), AgentVisibility::Allowed))
            .unwrap();
        assert_eq!(v, AgentVisibility::Hidden);
    }
}
