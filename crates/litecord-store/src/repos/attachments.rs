//! Attachment listing ("Files" views). Attachments are stored as message
//! extras; this module projects them without loading message bodies.

use rusqlite::{params, Connection};
use serde::Serialize;

use litecord_types::ids::{ConversationId, MessageId, UserId};
use litecord_types::provenance::Origin;
use litecord_types::social::MessageExtra;
use litecord_types::Timestamp;

use crate::error::StoreResult;
use crate::sql;

/// One attachment of a non-deleted message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AttachmentRecord {
    pub message_id: MessageId,
    pub conversation_id: ConversationId,
    pub author_id: UserId,
    pub sent_at: Timestamp,
    pub filename: String,
    pub content_type: Option<String>,
    pub size_bytes: u64,
    pub origin: Origin,
}

/// Attachments in a conversation, newest first. `before` pages backwards by
/// message time. At most `limit` attachments are returned.
pub fn for_conversation(
    conn: &Connection,
    conversation_id: ConversationId,
    before: Option<Timestamp>,
    limit: u32,
) -> StoreResult<Vec<AttachmentRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, author_id, sent_at, extras_json, origin FROM messages
         WHERE conversation_id = ?1 AND deleted = 0 AND extras_json IS NOT NULL
           AND sent_at < ?2
         ORDER BY sent_at DESC, id DESC",
    )?;
    let before = before.map(|t| t.as_millis()).unwrap_or(i64::MAX);
    let mut rows = stmt.query(params![conversation_id.to_sql(), before])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let extras_json: String = row.get(3)?;
        // A corrupt extras column must not break the whole listing.
        let extras: Vec<MessageExtra> = match serde_json::from_str(&extras_json) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "skipping unreadable message extras");
                continue;
            }
        };
        let origin_str: String = row.get(4)?;
        let origin = sql::origin(4, &origin_str)?;
        for extra in extras {
            if let MessageExtra::Attachment {
                filename,
                content_type,
                size_bytes,
            } = extra
            {
                out.push(AttachmentRecord {
                    message_id: MessageId::from_sql(row.get(0)?),
                    conversation_id,
                    author_id: UserId::from_sql(row.get(1)?),
                    sent_at: Timestamp::from_millis(row.get(2)?),
                    filename,
                    content_type,
                    size_bytes,
                    origin,
                });
                if out.len() >= limit as usize {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}
