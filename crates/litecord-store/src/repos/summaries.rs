//! Conversation summaries at increasing time horizons (segment/recent/
//! weekly/long_term). There is no `UnifiedEvent` for summaries: they are
//! compaction of already-observed content, not new facts agents need to be
//! nudged about.
//!
//! Supersession here is explicit like [`crate::repos::memory`]: a new summary
//! for the same `(conversation_id, level)` does not overwrite the old one in
//! place, it sets the old row's `superseded_by` and leaves it queryable.

use rusqlite::{params, types::Value, Connection, OptionalExtension, Row};

use litecord_types::provenance::Origin;
use litecord_types::{ConversationId, MessageId, Revision, Timestamp, ValidationError};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::repos::fts::{self, FtsMode};
use crate::sql::rev;

/// Which time horizon a summary compacts.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum SummaryLevel {
    #[serde(rename = "segment")]
    Segment,
    #[serde(rename = "recent")]
    Recent,
    #[serde(rename = "weekly")]
    Weekly,
    #[serde(rename = "long_term")]
    LongTerm,
}

impl SummaryLevel {
    pub const ALL: &'static [SummaryLevel] = &[
        SummaryLevel::Segment,
        SummaryLevel::Recent,
        SummaryLevel::Weekly,
        SummaryLevel::LongTerm,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            SummaryLevel::Segment => "segment",
            SummaryLevel::Recent => "recent",
            SummaryLevel::Weekly => "weekly",
            SummaryLevel::LongTerm => "long_term",
        }
    }

    pub fn parse(s: &str) -> Result<Self, ValidationError> {
        Self::ALL
            .iter()
            .copied()
            .find(|l| l.as_str() == s)
            .ok_or_else(|| ValidationError::Parse {
                what: "SummaryLevel",
                input: s.to_owned(),
            })
    }
}

impl std::fmt::Display for SummaryLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub id: i64,
    pub conversation_id: Option<ConversationId>,
    pub level: SummaryLevel,
    pub content: String,
    pub from_message_id: Option<MessageId>,
    pub to_message_id: Option<MessageId>,
    pub period_start: Option<Timestamp>,
    pub period_end: Option<Timestamp>,
    pub origin: Origin,
    pub created_at: Timestamp,
    pub revision: Revision,
    pub superseded_by: Option<i64>,
}

/// Input for creating a summary.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSummary {
    pub conversation_id: Option<ConversationId>,
    pub level: SummaryLevel,
    pub content: String,
    pub from_message_id: Option<MessageId>,
    pub to_message_id: Option<MessageId>,
    pub period_start: Option<Timestamp>,
    pub period_end: Option<Timestamp>,
    pub origin: Origin,
}

const SELECT_COLUMNS: &str =
    "id, conversation_id, level, content, from_message_id, to_message_id, \
     period_start, period_end, origin, created_at, revision, superseded_by";

fn row_to_summary(row: &Row<'_>) -> rusqlite::Result<Summary> {
    let id: i64 = row.get(0)?;
    let conversation_id: Option<i64> = row.get(1)?;
    let level: String = row.get(2)?;
    let level = SummaryLevel::parse(&level).map_err(|e| crate::sql::col_err(2, e))?;
    let content: String = row.get(3)?;
    let from_message_id: Option<i64> = row.get(4)?;
    let to_message_id: Option<i64> = row.get(5)?;
    let period_start: Option<i64> = row.get(6)?;
    let period_end: Option<i64> = row.get(7)?;
    let origin_s: String = row.get(8)?;
    let origin = crate::sql::origin(8, &origin_s)?;
    let created_at: i64 = row.get(9)?;
    let revision: i64 = row.get(10)?;
    let superseded_by: Option<i64> = row.get(11)?;
    Ok(Summary {
        id,
        conversation_id: conversation_id.map(ConversationId::from_sql),
        level,
        content,
        from_message_id: from_message_id.map(MessageId::from_sql),
        to_message_id: to_message_id.map(MessageId::from_sql),
        period_start: crate::sql::ts(period_start),
        period_end: crate::sql::ts(period_end),
        origin,
        created_at: Timestamp::from_millis(created_at),
        revision: rev(revision),
        superseded_by,
    })
}

/// Insert a new summary. If a current (not yet superseded) summary exists for
/// the same `(conversation_id, level)`, it is explicitly superseded by the
/// new one rather than overwritten.
pub fn insert(tx: &WriteTx<'_>, s: &NewSummary) -> StoreResult<i64> {
    let now = tx.now();
    let revision = tx.revision().get() as i64;
    let conv = s.conversation_id.map(|c| c.to_sql());
    tx.execute(
        "INSERT INTO summaries \
            (conversation_id, level, content, from_message_id, to_message_id, period_start, \
             period_end, origin, created_at, revision, superseded_by) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
        params![
            conv,
            s.level.as_str(),
            s.content,
            s.from_message_id.map(|m| m.to_sql()),
            s.to_message_id.map(|m| m.to_sql()),
            s.period_start.map(Timestamp::as_millis),
            s.period_end.map(Timestamp::as_millis),
            s.origin.as_str(),
            now.as_millis(),
            revision,
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "UPDATE summaries SET superseded_by = ? \
         WHERE id != ? AND superseded_by IS NULL AND level = ? \
           AND ((conversation_id IS NULL AND ? IS NULL) OR conversation_id = ?)",
        params![id, id, s.level.as_str(), conv, conv],
    )?;
    Ok(id)
}

/// The current (not superseded) summary for `(conversation_id, level)`, if
/// any.
pub fn latest(
    conn: &Connection,
    conversation_id: Option<ConversationId>,
    level: SummaryLevel,
) -> StoreResult<Option<Summary>> {
    let conv = conversation_id.map(|c| c.to_sql());
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM summaries \
         WHERE superseded_by IS NULL AND level = ? \
           AND ((conversation_id IS NULL AND ? IS NULL) OR conversation_id = ?) \
         ORDER BY COALESCE(period_end, created_at) DESC, id DESC LIMIT 1"
    );
    Ok(conn
        .query_row(&sql, params![level.as_str(), conv, conv], row_to_summary)
        .optional()?)
}

/// All summaries for a conversation, across levels, newest first per level.
pub fn list_for_conversation(
    conn: &Connection,
    conversation_id: ConversationId,
    include_superseded: bool,
) -> StoreResult<Vec<Summary>> {
    let mut sql = format!("SELECT {SELECT_COLUMNS} FROM summaries WHERE conversation_id = ?");
    if !include_superseded {
        sql.push_str(" AND superseded_by IS NULL");
    }
    sql.push_str(" ORDER BY level ASC, COALESCE(period_end, created_at) DESC");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![conversation_id.to_sql()], row_to_summary)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Full-text search over current (non-superseded) summary content.
pub fn search(
    conn: &Connection,
    query: &str,
    conversation_id: Option<ConversationId>,
    limit: u32,
) -> StoreResult<Vec<(Summary, f64)>> {
    let Some(expr) = fts::match_expr(query, FtsMode::Any, false) else {
        return Ok(Vec::new());
    };
    let mut sql = format!(
        "SELECT {}, bm25(summaries_fts) FROM summaries_fts \
         JOIN summaries s ON s.id = summaries_fts.rowid \
         WHERE summaries_fts MATCH ? AND s.superseded_by IS NULL",
        SELECT_COLUMNS
            .split(", ")
            .map(|c| format!("s.{c}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut params: Vec<Value> = vec![Value::Text(expr)];
    if let Some(c) = conversation_id {
        sql.push_str(" AND s.conversation_id = ?");
        params.push(Value::Integer(c.to_sql()));
    }
    sql.push_str(" ORDER BY bm25(summaries_fts) ASC LIMIT ?");
    let limit = if limit == 0 { 50 } else { limit };
    params.push(Value::Integer(limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
        let summary = row_to_summary(row)?;
        let bm25: f64 = row.get(12)?;
        Ok((summary, bm25))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::error::StoreError;

    /// `summaries.conversation_id` has a foreign key onto `conversations`,
    /// which is owned by a different repository batch (not implemented at the
    /// time this module was written); insert the minimal stub row a test
    /// needs directly.
    fn stub_conversation(tx: &WriteTx<'_>, id: ConversationId) {
        tx.execute(
            "INSERT INTO conversations (id, kind, origin, observed_at, revision) VALUES (?, 'dm', 'synthetic', 0, 0)",
            params![id.to_sql()],
        )
        .unwrap();
    }

    fn new_summary(
        conversation_id: ConversationId,
        level: SummaryLevel,
        content: &str,
    ) -> NewSummary {
        NewSummary {
            conversation_id: Some(conversation_id),
            level,
            content: content.to_string(),
            from_message_id: None,
            to_message_id: None,
            period_start: None,
            period_end: None,
            origin: Origin::AgentDerived,
        }
    }

    #[test]
    fn inserting_supersedes_prior_current_summary() {
        let db = Database::open_in_memory().unwrap();
        let conv = ConversationId(1);
        db.write(|tx| {
            stub_conversation(tx, conv);
            Ok::<_, StoreError>(())
        })
        .unwrap();
        let first = db
            .write(|tx| insert(tx, &new_summary(conv, SummaryLevel::Recent, "first")))
            .unwrap()
            .value;
        let second = db
            .write(|tx| insert(tx, &new_summary(conv, SummaryLevel::Recent, "second")))
            .unwrap()
            .value;

        let all = db.read(|r| list_for_conversation(r, conv, true)).unwrap();
        assert_eq!(all.len(), 2);
        let current = db.read(|r| list_for_conversation(r, conv, false)).unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].id, second);

        let latest = db
            .read(|r| latest(r, Some(conv), SummaryLevel::Recent))
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, second);
        assert_ne!(first, second);
    }

    #[test]
    fn search_only_returns_current() {
        let db = Database::open_in_memory().unwrap();
        let conv = ConversationId(2);
        db.write(|tx| {
            stub_conversation(tx, conv);
            Ok::<_, StoreError>(())
        })
        .unwrap();
        db.write(|tx| {
            insert(
                tx,
                &new_summary(conv, SummaryLevel::Weekly, "talked about rockets"),
            )
        })
        .unwrap();
        db.write(|tx| {
            insert(
                tx,
                &new_summary(conv, SummaryLevel::Weekly, "talked about gardens"),
            )
        })
        .unwrap();

        let hits = db.read(|r| search(r, "rockets", Some(conv), 10)).unwrap();
        assert!(hits.is_empty(), "superseded summary should not be found");

        let hits = db.read(|r| search(r, "gardens", Some(conv), 10)).unwrap();
        assert_eq!(hits.len(), 1);
    }
}
