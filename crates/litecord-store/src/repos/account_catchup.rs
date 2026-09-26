//! Verified account REST coverage; live observations cannot advance it.
use crate::{db::WriteTx, error::StoreResult};
use litecord_types::{ConversationId, MessageId, Timestamp};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub conversation_id: ConversationId,
    pub newest_message_id: Option<MessageId>,
}

pub fn track(tx: &WriteTx<'_>, id: ConversationId, newest: Option<MessageId>) -> StoreResult<()> {
    tx.execute(
        "INSERT OR IGNORE INTO account_catchup (conversation_id,newest_message_id) VALUES (?1,?2)",
        params![id.to_sql(), newest.map(MessageId::to_sql)],
    )?;
    Ok(())
}

pub fn next_due(conn: &Connection, now: Timestamp) -> StoreResult<Option<Cursor>> {
    Ok(conn
        .query_row(
            "SELECT c.conversation_id,c.newest_message_id FROM account_catchup c
        JOIN conversations v ON v.id=c.conversation_id
        LEFT JOIN guilds g ON g.id=v.guild_id
        WHERE c.retry_at<=?1 AND c.checked_at<=?1-30000 AND COALESCE(g.departed,0)=0
        ORDER BY c.checked_at,c.conversation_id LIMIT 1",
            params![now.as_millis()],
            |r| {
                Ok(Cursor {
                    conversation_id: ConversationId::from_sql(r.get(0)?),
                    newest_message_id: r.get::<_, Option<i64>>(1)?.map(MessageId::from_sql),
                })
            },
        )
        .optional()?)
}

pub fn advance(
    tx: &WriteTx<'_>,
    id: ConversationId,
    newest: Option<MessageId>,
    more: bool,
) -> StoreResult<()> {
    tx.execute("UPDATE account_catchup SET newest_message_id=CASE
        WHEN ?2 IS NULL THEN newest_message_id
        WHEN newest_message_id IS NULL OR newest_message_id<?2 THEN ?2 ELSE newest_message_id END,
        checked_at=CASE WHEN ?3 THEN 0 ELSE ?4 END,retry_at=0,last_error=NULL WHERE conversation_id=?1",
        params![id.to_sql(),newest.map(MessageId::to_sql),more,tx.now().as_millis()])?;
    Ok(())
}

pub fn retry(tx: &WriteTx<'_>, id: ConversationId, after_ms: u64, reason: &str) -> StoreResult<()> {
    let reason: String = reason.chars().take(200).collect();
    tx.execute(
        "UPDATE account_catchup SET retry_at=?2,last_error=?3 WHERE conversation_id=?1",
        params![
            id.to_sql(),
            tx.now()
                .as_millis()
                .saturating_add(after_ms.min(i64::MAX as u64) as i64),
            reason
        ],
    )?;
    Ok(())
}

pub fn reconnect(tx: &WriteTx<'_>) -> StoreResult<()> {
    // Keep rate-limit/permission cooldowns; make verified coverage due again.
    tx.execute("UPDATE account_catchup SET checked_at=0", [])?;
    Ok(())
}
