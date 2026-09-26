//! Messages.

use rusqlite::{params, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::provenance::Origin;
use litecord_types::social::{Message, MessageExtra};
use litecord_types::{ConversationId, GuildId, MessageId, Revision, Timestamp, UserId};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::repos::conversations;
use crate::repos::fts::{match_expr, FtsMode};
use crate::repos::users;
use crate::sql::ts;

/// A stored message row plus bookkeeping.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MessageRecord {
    pub message: Message,
    pub deleted: bool,
    pub origin: Origin,
    pub observed_at: Timestamp,
    pub revision: Revision,
}

/// What [`upsert`] actually did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageWrite {
    Created,
    Updated,
    Unchanged,
}

/// Full result of [`upsert`], including which placeholder rows it had to
/// create along the way (useful to schedule hydration follow-ups).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageWriteOutcome {
    pub write: MessageWrite,
    pub created_conversation_stub: bool,
    pub created_author_stub: bool,
}

const COLUMNS: &str = "id, conversation_id, author_id, content, sent_at, edited_at, reply_to, \
     extras_json, deleted, origin, observed_at, revision";

/// `(deleted, content, edited_at, reply_to, extras_json)` for the pre-write
/// row, as read back inside [`upsert`].
type ExistingMessage = (bool, String, Option<i64>, Option<i64>, Option<String>);

fn map_row(row: &Row<'_>) -> rusqlite::Result<MessageRecord> {
    let id = MessageId::from_sql(row.get(0)?);
    let conversation_id = ConversationId::from_sql(row.get(1)?);
    let author_id = UserId::from_sql(row.get(2)?);
    let content: String = row.get(3)?;
    let sent_at = Timestamp::from_millis(row.get(4)?);
    let edited_at = ts(row.get(5)?);
    let reply_to: Option<i64> = row.get(6)?;
    let extras_json: Option<String> = row.get(7)?;
    let extras: Vec<MessageExtra> = match extras_json {
        Some(s) => serde_json::from_str(&s).map_err(|e| crate::sql::col_err(7, e))?,
        None => Vec::new(),
    };
    let deleted: bool = row.get::<_, i64>(8)? != 0;
    let origin_s: String = row.get(9)?;
    let origin = crate::sql::origin(9, &origin_s)?;
    let observed_at = Timestamp::from_millis(row.get(10)?);
    let revision = crate::sql::rev(row.get(11)?);

    Ok(MessageRecord {
        message: Message {
            id,
            conversation_id,
            author_id,
            content: content.into(),
            sent_at,
            edited_at,
            reply_to: reply_to.map(MessageId::from_sql),
            extras,
        },
        deleted,
        origin,
        observed_at,
        revision,
    })
}

/// Insert or update a message. Ensures the conversation and author exist
/// (creating stubs as needed), never resurrects a deleted message, and
/// advances the conversation's activity marker on a real change.
pub fn upsert(
    tx: &WriteTx<'_>,
    message: &Message,
    origin: Origin,
    observed_at: Timestamp,
) -> StoreResult<MessageWriteOutcome> {
    let created_conversation_stub =
        conversations::ensure(tx, message.conversation_id, origin, observed_at)?;
    let created_author_stub = users::ensure_stub(tx, message.author_id, origin, observed_at)?;

    let extras_json = if message.extras.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&message.extras)?)
    };

    let existing: Option<ExistingMessage> = tx
        .query_row(
            "SELECT deleted, content, edited_at, reply_to, extras_json FROM messages WHERE id = ?1",
            params![message.id.to_sql()],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? != 0,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                ))
            },
        )
        .optional()?;

    let new_edited_at = message.edited_at.map(Timestamp::as_millis);
    let new_reply_to = message.reply_to.map(|m| m.to_sql());

    let write = match existing {
        None => {
            tx.execute(
                "INSERT INTO messages (id, conversation_id, author_id, content, sent_at, edited_at, reply_to, extras_json, deleted, origin, observed_at, revision)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, ?10, ?11)",
                params![
                    message.id.to_sql(),
                    message.conversation_id.to_sql(),
                    message.author_id.to_sql(),
                    message.content.as_ref(),
                    message.sent_at.as_millis(),
                    new_edited_at,
                    new_reply_to,
                    extras_json,
                    origin.as_str(),
                    observed_at.as_millis(),
                    tx.revision().get() as i64,
                ],
            )?;
            tx.emit(
                UnifiedEvent::MessageCreated {
                    message_id: message.id,
                    conversation_id: message.conversation_id,
                },
                origin,
            )?;
            MessageWrite::Created
        }
        Some((deleted, old_content, old_edited_at, old_reply_to, old_extras)) => {
            let unchanged = deleted
                || (old_content == message.content.as_ref()
                    && old_edited_at == new_edited_at
                    && old_reply_to == new_reply_to
                    && old_extras == extras_json);
            if unchanged {
                MessageWrite::Unchanged
            } else {
                tx.execute(
                    "UPDATE messages SET content = ?1, edited_at = ?2, reply_to = ?3, extras_json = ?4, origin = ?5, observed_at = ?6, revision = ?7
                     WHERE id = ?8",
                    params![
                        message.content.as_ref(),
                        new_edited_at,
                        new_reply_to,
                        extras_json,
                        origin.as_str(),
                        observed_at.as_millis(),
                        tx.revision().get() as i64,
                        message.id.to_sql(),
                    ],
                )?;
                tx.emit(
                    UnifiedEvent::MessageUpdated {
                        message_id: message.id,
                        conversation_id: message.conversation_id,
                    },
                    origin,
                )?;
                MessageWrite::Updated
            }
        }
    };

    if write != MessageWrite::Unchanged {
        conversations::touch_activity(tx, message.conversation_id, message.id, message.sent_at)?;
    }

    Ok(MessageWriteOutcome {
        write,
        created_conversation_stub,
        created_author_stub,
    })
}

/// Mark a message deleted. When `purge_content` is set, the content and
/// extras are also cleared. Emits [`UnifiedEvent::MessageDeleted`] if a row
/// actually changed.
pub fn mark_deleted(
    tx: &WriteTx<'_>,
    id: MessageId,
    conversation_id: ConversationId,
    purge_content: bool,
    origin: Origin,
) -> StoreResult<bool> {
    let n = tx.execute(
        "UPDATE messages SET
             deleted = 1,
             content = CASE WHEN ?1 THEN '' ELSE content END,
             extras_json = CASE WHEN ?1 THEN NULL ELSE extras_json END,
             revision = ?2
         WHERE id = ?3 AND deleted = 0",
        params![purge_content, tx.revision().get() as i64, id.to_sql()],
    )?;
    let changed = n > 0;
    if changed {
        tx.emit(
            UnifiedEvent::MessageDeleted {
                message_id: id,
                conversation_id,
            },
            origin,
        )?;
    }
    Ok(changed)
}

/// Look up one message (deleted or not).
pub fn get(conn: &Connection, id: MessageId) -> StoreResult<Option<MessageRecord>> {
    let sql = format!("SELECT {COLUMNS} FROM messages WHERE id = ?1");
    Ok(conn
        .query_row(&sql, params![id.to_sql()], map_row)
        .optional()?)
}

/// Non-deleted messages in a conversation, newest first, optionally before a
/// given time (for backward pagination).
pub fn recent(
    conn: &Connection,
    conversation_id: ConversationId,
    limit: u32,
    before: Option<Timestamp>,
) -> StoreResult<Vec<MessageRecord>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM messages
         WHERE conversation_id = ?1 AND deleted = 0 AND (?2 IS NULL OR sent_at < ?2)
         ORDER BY sent_at DESC
         LIMIT ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![
            conversation_id.to_sql(),
            before.map(Timestamp::as_millis),
            limit
        ],
        map_row,
    )?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Non-deleted messages within `[since, until]`, oldest first, optionally
/// scoped to one conversation.
pub fn in_range(
    conn: &Connection,
    conversation_id: Option<ConversationId>,
    since: Timestamp,
    until: Option<Timestamp>,
    limit: u32,
) -> StoreResult<Vec<MessageRecord>> {
    let sql = format!(
        "SELECT {COLUMNS} FROM messages
         WHERE deleted = 0 AND sent_at >= ?1 AND (?2 IS NULL OR sent_at <= ?2)
               AND (?3 IS NULL OR conversation_id = ?3)
         ORDER BY sent_at ASC
         LIMIT ?4"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![
            since.as_millis(),
            until.map(Timestamp::as_millis),
            conversation_id.map(|c| c.to_sql()),
            limit
        ],
        map_row,
    )?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Id of the oldest stored message (deleted or not) in a conversation.
pub fn oldest_in(
    conn: &Connection,
    conversation_id: ConversationId,
) -> StoreResult<Option<MessageId>> {
    let v: Option<i64> = conn.query_row(
        "SELECT MIN(id) FROM messages WHERE conversation_id = ?1",
        params![conversation_id.to_sql()],
        |r| r.get(0),
    )?;
    Ok(v.map(MessageId::from_sql))
}

/// Count non-deleted messages, optionally scoped to one conversation.
pub fn count(conn: &Connection, conversation: Option<ConversationId>) -> StoreResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE deleted = 0 AND (?1 IS NULL OR conversation_id = ?1)",
        params![conversation.map(|c| c.to_sql())],
        |r| r.get(0),
    )?)
}

/// A conversation whose most recent message awaits `me`'s reply.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PendingReply {
    pub conversation_id: ConversationId,
    pub last_message: MessageRecord,
}

/// Conversations whose latest non-deleted message is from someone other than
/// `me`, sent at or after `since`, newest first.
pub fn pending_replies(
    conn: &Connection,
    me: UserId,
    since: Timestamp,
    limit: u32,
) -> StoreResult<Vec<PendingReply>> {
    let sql = format!(
        "SELECT {cols} FROM messages m
         JOIN (
             SELECT conversation_id, MAX(sent_at) AS max_sent
             FROM messages WHERE deleted = 0
             GROUP BY conversation_id
         ) latest ON latest.conversation_id = m.conversation_id AND latest.max_sent = m.sent_at
         WHERE m.deleted = 0 AND m.author_id != ?1 AND m.sent_at >= ?2
         ORDER BY m.sent_at DESC
         LIMIT ?3",
        cols = COLUMNS
            .split(", ")
            .map(|c| format!("m.{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![me.to_sql(), since.as_millis(), limit], map_row)?;
    let mut out = Vec::new();
    for r in rows {
        let record = r?;
        out.push(PendingReply {
            conversation_id: record.message.conversation_id,
            last_message: record,
        });
    }
    Ok(out)
}

/// Whether `author` has sent a non-deleted message in `conversation_id`
/// since `since`.
pub fn has_message_from_since(
    conn: &Connection,
    conversation_id: ConversationId,
    author: UserId,
    since: Timestamp,
) -> StoreResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE conversation_id = ?1 AND author_id = ?2 AND sent_at >= ?3 AND deleted = 0)",
        params![conversation_id.to_sql(), author.to_sql(), since.as_millis()],
        |r| r.get::<_, i64>(0),
    )? != 0)
}

/// A full-text search request over messages.
#[derive(Debug, Clone)]
pub struct MessageSearch<'a> {
    pub query: &'a str,
    pub conversation_ids: Option<&'a [ConversationId]>,
    pub exclude_conversation_ids: &'a [ConversationId],
    pub author_id: Option<UserId>,
    pub guild_id: Option<GuildId>,
    pub since: Option<Timestamp>,
    pub until: Option<Timestamp>,
    pub origins: Option<&'a [Origin]>,
    pub mode: FtsMode,
    pub limit: u32,
}

/// One search hit: the message plus its raw bm25 score (lower is better).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MessageHit {
    pub record: MessageRecord,
    pub bm25: f64,
}

/// Permanently delete messages with `sent_at < before`, for raw-message
/// retention.
///
/// This is bulk maintenance, not an individually-observed write: unlike
/// every other write in this module it emits **no** `UnifiedEvent` per row
/// (a purge is not something an agent or the UI needs to be nudged about,
/// and one event per pruned row would flood the event log for exactly the
/// data retention exists to shrink). The `messages_fts_ad` trigger still
/// fires per deleted row and keeps `messages_fts` consistent with the base
/// table, so pruned content stops matching [`search`] immediately.
///
/// When `keep_bookmarked` is set, messages the user bookmarked
/// ([`crate::repos::notes::add_bookmark`]) are kept regardless of age —
/// bookmarking is an explicit signal to retain, and a bookmark pointing at a
/// vanished message would be a dangling reference.
///
/// Applies to both live and already soft-deleted rows (deletion here is
/// unconditional removal, not the `deleted` flag `mark_deleted` sets).
/// Returns the number of rows removed.
pub fn prune_before(
    tx: &WriteTx<'_>,
    before: Timestamp,
    keep_bookmarked: bool,
) -> StoreResult<usize> {
    let sql = if keep_bookmarked {
        "DELETE FROM messages WHERE sent_at < ?1 AND id NOT IN (SELECT message_id FROM bookmarks)"
    } else {
        "DELETE FROM messages WHERE sent_at < ?1"
    };
    Ok(tx.execute(sql, params![before.as_millis()])?)
}

/// Search non-deleted messages by content, best match first.
pub fn search(conn: &Connection, q: &MessageSearch<'_>) -> StoreResult<Vec<MessageHit>> {
    let Some(expr) = match_expr(q.query, q.mode, false) else {
        return Ok(Vec::new());
    };
    if matches!(q.conversation_ids, Some(ids) if ids.is_empty()) {
        return Ok(Vec::new());
    }
    if matches!(q.origins, Some(os) if os.is_empty()) {
        return Ok(Vec::new());
    }

    let m_cols = COLUMNS
        .split(", ")
        .map(|c| format!("m.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut sql = format!(
        "SELECT {m_cols}, bm25(messages_fts) AS score
         FROM messages_fts
         JOIN messages m ON m.id = messages_fts.rowid
         JOIN conversations c ON c.id = m.conversation_id
         WHERE messages_fts MATCH ?1 AND m.deleted = 0"
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(expr)];

    if let Some(ids) = q.conversation_ids {
        let placeholders = vec!["?"; ids.len()].join(",");
        sql.push_str(&format!(" AND m.conversation_id IN ({placeholders})"));
        for id in ids {
            params.push(Box::new(id.to_sql()));
        }
    }
    if !q.exclude_conversation_ids.is_empty() {
        let placeholders = vec!["?"; q.exclude_conversation_ids.len()].join(",");
        sql.push_str(&format!(" AND m.conversation_id NOT IN ({placeholders})"));
        for id in q.exclude_conversation_ids {
            params.push(Box::new(id.to_sql()));
        }
    }
    if let Some(author) = q.author_id {
        sql.push_str(" AND m.author_id = ?");
        params.push(Box::new(author.to_sql()));
    }
    if let Some(guild_id) = q.guild_id {
        sql.push_str(" AND c.guild_id = ?");
        params.push(Box::new(guild_id.to_sql()));
    }
    if let Some(since) = q.since {
        sql.push_str(" AND m.sent_at >= ?");
        params.push(Box::new(since.as_millis()));
    }
    if let Some(until) = q.until {
        sql.push_str(" AND m.sent_at <= ?");
        params.push(Box::new(until.as_millis()));
    }
    if let Some(origins) = q.origins {
        let placeholders = vec!["?"; origins.len()].join(",");
        sql.push_str(&format!(" AND m.origin IN ({placeholders})"));
        for o in origins {
            params.push(Box::new(o.as_str()));
        }
    }
    sql.push_str(" ORDER BY score ASC LIMIT ?");
    params.push(Box::new(q.limit));

    let mut stmt = conn.prepare(&sql)?;
    let col_count = COLUMNS.split(", ").count();
    let rows = stmt.query_map(
        rusqlite::params_from_iter(params.iter().map(|b| b.as_ref())),
        move |row| {
            let record = map_row(row)?;
            let bm25: f64 = row.get(col_count)?;
            Ok(MessageHit { record, bm25 })
        },
    )?;
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

    fn msg(id: u64, conv: u64, author: u64, content: &str, sent_at: i64) -> Message {
        Message {
            id: MessageId(id),
            conversation_id: ConversationId(conv),
            author_id: UserId(author),
            content: content.into(),
            sent_at: Timestamp::from_millis(sent_at),
            edited_at: None,
            reply_to: None,
            extras: Vec::new(),
        }
    }

    #[test]
    fn create_then_update_then_reobserve() {
        let db = Database::open_in_memory().unwrap();
        let c1 = db
            .write(|tx| {
                upsert(
                    tx,
                    &msg(1, 1, 2, "hello", 10),
                    Origin::Synthetic,
                    Timestamp::from_millis(1),
                )
            })
            .unwrap();
        assert_eq!(c1.value.write, MessageWrite::Created);
        assert!(c1.value.created_conversation_stub);
        assert!(c1.value.created_author_stub);

        let c2 = db
            .write(|tx| {
                upsert(
                    tx,
                    &msg(1, 1, 2, "hello", 10),
                    Origin::Synthetic,
                    Timestamp::from_millis(2),
                )
            })
            .unwrap();
        assert_eq!(c2.value.write, MessageWrite::Unchanged);
        assert!(!c2.changed());

        let c3 = db
            .write(|tx| {
                upsert(
                    tx,
                    &msg(1, 1, 2, "hello edited", 10),
                    Origin::Synthetic,
                    Timestamp::from_millis(3),
                )
            })
            .unwrap();
        assert_eq!(c3.value.write, MessageWrite::Updated);
        assert!(c3.changed());
    }

    #[test]
    fn delete_does_not_get_resurrected() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &msg(1, 1, 2, "hello", 10),
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        db.write(|tx| mark_deleted(tx, MessageId(1), ConversationId(1), true, Origin::Synthetic))
            .unwrap();
        let outcome = db
            .write(|tx| {
                upsert(
                    tx,
                    &msg(1, 1, 2, "hello", 10),
                    Origin::Synthetic,
                    Timestamp::from_millis(2),
                )
            })
            .unwrap();
        assert_eq!(outcome.value.write, MessageWrite::Unchanged);
        let rec = db.read(|r| get(r, MessageId(1))).unwrap().unwrap();
        assert!(rec.deleted);
        assert_eq!(rec.message.content.as_ref(), "");
    }

    #[test]
    fn prune_before_removes_rows_and_fts_entries_but_keeps_bookmarks() {
        use crate::repos::notes;
        use litecord_types::notes::Bookmark;

        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &msg(1, 1, 2, "ancient unicorn tale", 10),
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &msg(2, 1, 2, "ancient bookmarked tale", 20),
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &msg(3, 1, 2, "recent unicorn tale", 1_000),
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        db.write(|tx| {
            notes::add_bookmark(
                tx,
                &Bookmark {
                    message_id: MessageId(2),
                    conversation_id: ConversationId(1),
                    note: None,
                    created_at: Timestamp::from_millis(1),
                },
                Origin::UserProvided,
            )
        })
        .unwrap();

        let pruned = db
            .write(|tx| prune_before(tx, Timestamp::from_millis(500), true))
            .unwrap()
            .value;
        assert_eq!(pruned, 1, "only the non-bookmarked old message is pruned");

        assert!(db.read(|r| get(r, MessageId(1))).unwrap().is_none());
        assert!(db.read(|r| get(r, MessageId(2))).unwrap().is_some());
        assert!(db.read(|r| get(r, MessageId(3))).unwrap().is_some());

        let hits = db
            .read(|r| {
                search(
                    r,
                    &MessageSearch {
                        query: "unicorn",
                        conversation_ids: None,
                        exclude_conversation_ids: &[],
                        author_id: None,
                        guild_id: None,
                        since: None,
                        until: None,
                        origins: None,
                        mode: crate::repos::fts::FtsMode::Any,
                        limit: 10,
                    },
                )
            })
            .unwrap();
        assert_eq!(
            hits.len(),
            1,
            "the pruned message must no longer be searchable"
        );
        assert_eq!(hits[0].record.message.id, MessageId(3));

        let all_pruned = db
            .write(|tx| prune_before(tx, Timestamp::from_millis(2_000), false))
            .unwrap()
            .value;
        assert_eq!(
            all_pruned, 2,
            "without keep_bookmarked everything qualifying is removed"
        );
        assert!(db.read(|r| get(r, MessageId(2))).unwrap().is_none());
    }

    #[test]
    fn pending_replies_only_incoming() {
        let db = Database::open_in_memory().unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &msg(1, 1, 2, "hi from them", 10),
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        db.write(|tx| {
            upsert(
                tx,
                &msg(2, 2, 5, "hi from me", 10),
                Origin::Synthetic,
                Timestamp::from_millis(1),
            )
        })
        .unwrap();
        // conversation 2's last message is authored by `me` (5), so it must
        // not appear.
        let pending = db
            .read(|r| pending_replies(r, UserId(5), Timestamp::from_millis(0), 10))
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].conversation_id, ConversationId(1));
    }
}
