//! Action engine persistence: proposals, single-use approvals and the audit
//! trail. Hashing and policy decisions live in `litecord-actions`; this
//! module only stores what that crate hands it.

use rusqlite::{params, types::Value, Connection, OptionalExtension, Row};

use litecord_core::events::UnifiedEvent;
use litecord_types::actions::{
    ActionAuditEntry, ActionProposal, ActionStatus, Actor, AgentAction, AuditEvent, CapabilityClass,
};
use litecord_types::provenance::{DiscordIdentity, Origin};
use litecord_types::{ActionId, Revision, Timestamp, ValidationError};

use crate::db::WriteTx;
use crate::error::StoreResult;
use crate::sql::{col_err, push_in_str, rev};

fn identity_str(identity: DiscordIdentity) -> &'static str {
    match identity {
        DiscordIdentity::UserSocialSdk => "user_social_sdk",
        DiscordIdentity::UserSession => "user_session",
        DiscordIdentity::ApplicationBot => "application_bot",
    }
}

fn parse_identity(s: &str) -> Result<DiscordIdentity, ValidationError> {
    match s {
        "user_social_sdk" => Ok(DiscordIdentity::UserSocialSdk),
        "user_session" => Ok(DiscordIdentity::UserSession),
        "application_bot" => Ok(DiscordIdentity::ApplicationBot),
        _ => Err(ValidationError::Parse {
            what: "DiscordIdentity",
            input: s.to_owned(),
        }),
    }
}

fn actor_origin(actor: &Actor) -> Origin {
    match actor {
        Actor::User => Origin::UserProvided,
        Actor::Agent { .. } => Origin::AgentDerived,
        Actor::System => Origin::LocalApplication,
    }
}

/// Input for [`insert_proposal`].
#[derive(Debug, Clone)]
pub struct NewProposal<'a> {
    pub action: &'a AgentAction,
    pub actor: &'a Actor,
    pub class: CapabilityClass,
    pub identity: DiscordIdentity,
    pub payload_json: &'a str,
    pub payload_hash: &'a str,
    pub status: ActionStatus,
    pub based_on_revision: Revision,
    pub rationale: Option<&'a str>,
}

/// Persist a new action proposal. Emits [`UnifiedEvent::ActionProposed`] with
/// an origin derived from `actor` (agent -> `AgentDerived`, user ->
/// `UserProvided`, system -> `LocalApplication`).
pub fn insert_proposal(tx: &WriteTx<'_>, p: &NewProposal<'_>) -> StoreResult<ActionId> {
    let actor_json = serde_json::to_string(p.actor)?;
    let now = tx.now();
    tx.execute(
        "INSERT INTO action_proposals \
            (kind, class, identity, actor_json, payload_json, payload_hash, status, \
             based_on_revision, rationale, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            p.action.kind(),
            p.class.as_str(),
            identity_str(p.identity),
            actor_json,
            p.payload_json,
            p.payload_hash,
            p.status.as_str(),
            p.based_on_revision.get() as i64,
            p.rationale,
            now.as_millis(),
            now.as_millis(),
        ],
    )?;
    let id = ActionId(tx.last_insert_rowid());
    tx.emit(
        UnifiedEvent::ActionProposed { action_id: id },
        actor_origin(p.actor),
    )?;
    Ok(id)
}

const SELECT_COLUMNS: &str =
    "id, class, identity, actor_json, payload_json, payload_hash, status, \
     based_on_revision, rationale, created_at, updated_at";

fn row_to_proposal(row: &Row<'_>) -> rusqlite::Result<ActionProposal> {
    let id = ActionId(row.get(0)?);
    let class: String = row.get(1)?;
    let class = CapabilityClass::parse(&class).map_err(|e| col_err(1, e))?;
    let identity: String = row.get(2)?;
    let identity = parse_identity(&identity).map_err(|e| col_err(2, e))?;
    let actor_json: String = row.get(3)?;
    let actor: Actor = serde_json::from_str(&actor_json).map_err(|e| col_err(3, e))?;
    let payload_json: String = row.get(4)?;
    let action: AgentAction = serde_json::from_str(&payload_json).map_err(|e| col_err(4, e))?;
    let payload_hash: String = row.get(5)?;
    let status: String = row.get(6)?;
    let status = ActionStatus::parse(&status).map_err(|e| col_err(6, e))?;
    let based_on_revision: i64 = row.get(7)?;
    let rationale: Option<String> = row.get(8)?;
    let created_at: i64 = row.get(9)?;
    let updated_at: i64 = row.get(10)?;
    Ok(ActionProposal {
        id,
        action,
        actor,
        class,
        identity,
        status,
        payload_hash,
        based_on_revision: rev(based_on_revision),
        rationale,
        created_at: Timestamp::from_millis(created_at),
        updated_at: Timestamp::from_millis(updated_at),
    })
}

/// Load a proposal by id, reconstructing the typed [`AgentAction`] and
/// [`Actor`].
pub fn get(conn: &Connection, id: ActionId) -> StoreResult<Option<ActionProposal>> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM action_proposals WHERE id = ?");
    Ok(conn
        .query_row(&sql, params![id.get()], row_to_proposal)
        .optional()?)
}

/// The raw stored payload JSON (used by the caller to re-verify the approval
/// hash without re-deserializing and re-serializing the action).
pub fn get_payload_json(conn: &Connection, id: ActionId) -> StoreResult<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT payload_json FROM action_proposals WHERE id = ?",
            params![id.get()],
            |r| r.get(0),
        )
        .optional()?)
}

/// List proposals, optionally filtered by status, newest first. `limit = 0`
/// means unlimited.
pub fn list(
    conn: &Connection,
    statuses: Option<&[ActionStatus]>,
    limit: u32,
) -> StoreResult<Vec<ActionProposal>> {
    let mut sql = format!("SELECT {SELECT_COLUMNS} FROM action_proposals");
    let mut conditions = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(statuses) = statuses {
        push_in_str(
            &mut conditions,
            &mut params,
            "status",
            statuses.iter().map(|s| s.as_str()),
        );
    }
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC");
    if limit > 0 {
        sql.push_str(" LIMIT ?");
        params.push(Value::Integer(limit as i64));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), row_to_proposal)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Replace a proposal's action payload (e.g. after a user edit before
/// approval). Stamps `updated_at`; emits no event.
pub fn update_payload(
    tx: &WriteTx<'_>,
    id: ActionId,
    action: &AgentAction,
    payload_json: &str,
    payload_hash: &str,
) -> StoreResult<bool> {
    let n = tx.execute(
        "UPDATE action_proposals SET kind = ?, payload_json = ?, payload_hash = ?, updated_at = ? WHERE id = ?",
        params![action.kind(), payload_json, payload_hash, tx.now().as_millis(), id.get()],
    )?;
    Ok(n > 0)
}

/// Change a proposal's status, emitting the matching lifecycle event
/// (`Approved`/`Rejected`/`Executed`/`Failed`/`Invalidated`|`Expired` ->
/// `ActionInvalidated`; other statuses emit nothing). No-op if unchanged.
pub fn set_status(
    tx: &WriteTx<'_>,
    id: ActionId,
    status: ActionStatus,
    origin: Origin,
) -> StoreResult<bool> {
    let n = tx.execute(
        "UPDATE action_proposals SET status = ?, updated_at = ? WHERE id = ? AND status IS NOT ?",
        params![
            status.as_str(),
            tx.now().as_millis(),
            id.get(),
            status.as_str()
        ],
    )?;
    let changed = n > 0;
    if changed {
        let event = match status {
            ActionStatus::Approved => Some(UnifiedEvent::ActionApproved { action_id: id }),
            ActionStatus::Rejected => Some(UnifiedEvent::ActionRejected { action_id: id }),
            ActionStatus::Executed => Some(UnifiedEvent::ActionExecuted { action_id: id }),
            ActionStatus::Failed | ActionStatus::Uncertain => {
                Some(UnifiedEvent::ActionFailed { action_id: id })
            }
            ActionStatus::Invalidated | ActionStatus::Expired => {
                Some(UnifiedEvent::ActionInvalidated { action_id: id })
            }
            _ => None,
        };
        if let Some(event) = event {
            tx.emit(event, origin)?;
        }
    }
    Ok(changed)
}

/// A recorded approval token for an action proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub id: i64,
    pub action_id: ActionId,
    pub payload_hash: String,
    pub nonce: String,
    pub approved_at: Timestamp,
    pub expires_at: Timestamp,
    pub edited: bool,
    pub consumed_at: Option<Timestamp>,
}

const APPROVAL_COLUMNS: &str =
    "id, action_id, payload_hash, nonce, approved_at, expires_at, edited, consumed_at";

fn row_to_approval(row: &Row<'_>) -> rusqlite::Result<ApprovalRecord> {
    let id: i64 = row.get(0)?;
    let action_id = ActionId(row.get(1)?);
    let payload_hash: String = row.get(2)?;
    let nonce: String = row.get(3)?;
    let approved_at: i64 = row.get(4)?;
    let expires_at: i64 = row.get(5)?;
    let edited: i64 = row.get(6)?;
    let consumed_at: Option<i64> = row.get(7)?;
    Ok(ApprovalRecord {
        id,
        action_id,
        payload_hash,
        nonce,
        approved_at: Timestamp::from_millis(approved_at),
        expires_at: Timestamp::from_millis(expires_at),
        edited: edited != 0,
        consumed_at: crate::sql::ts(consumed_at),
    })
}

/// Record a new approval token for `action_id`.
#[allow(clippy::too_many_arguments)]
pub fn insert_approval(
    tx: &WriteTx<'_>,
    action_id: ActionId,
    payload_hash: &str,
    nonce: &str,
    approved_at: Timestamp,
    expires_at: Timestamp,
    edited: bool,
) -> StoreResult<i64> {
    tx.execute(
        "INSERT INTO action_approvals (action_id, payload_hash, nonce, approved_at, expires_at, edited, consumed_at) \
         VALUES (?, ?, ?, ?, ?, ?, NULL)",
        params![
            action_id.get(),
            payload_hash,
            nonce,
            approved_at.as_millis(),
            expires_at.as_millis(),
            edited as i64,
        ],
    )?;
    Ok(tx.last_insert_rowid())
}

/// Load an approval by its nonce.
pub fn get_approval(conn: &Connection, nonce: &str) -> StoreResult<Option<ApprovalRecord>> {
    let sql = format!("SELECT {APPROVAL_COLUMNS} FROM action_approvals WHERE nonce = ?");
    Ok(conn
        .query_row(&sql, params![nonce], row_to_approval)
        .optional()?)
}

/// Consume an approval (single use): succeeds only if it has not already
/// been consumed.
pub fn consume_approval(tx: &WriteTx<'_>, nonce: &str, at: Timestamp) -> StoreResult<bool> {
    let n = tx.execute(
        "UPDATE action_approvals SET consumed_at = ? WHERE nonce = ? AND consumed_at IS NULL",
        params![at.as_millis(), nonce],
    )?;
    Ok(n > 0)
}

/// Mark every unconsumed approval for `action_id` as consumed (e.g. because
/// the underlying state changed and they must be re-validated). Returns how
/// many were revoked.
pub fn revoke_approvals(
    tx: &WriteTx<'_>,
    action_id: ActionId,
    at: Timestamp,
) -> StoreResult<usize> {
    Ok(tx.execute(
        "UPDATE action_approvals SET consumed_at = ? WHERE action_id = ? AND consumed_at IS NULL",
        params![at.as_millis(), action_id.get()],
    )?)
}

/// Append one audit entry.
pub fn append_audit(tx: &WriteTx<'_>, entry: &ActionAuditEntry) -> StoreResult<()> {
    let event_json_value = serde_json::to_value(&entry.event)?;
    let event_kind = event_json_value
        .get("event")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let actor_json = serde_json::to_string(&entry.actor)?;
    let event_json = serde_json::to_string(&entry.event)?;
    tx.execute(
        "INSERT INTO action_history (action_id, at, actor_json, event_kind, event_json, revision) \
         VALUES (?, ?, ?, ?, ?, ?)",
        params![
            entry.action_id.get(),
            entry.at.as_millis(),
            actor_json,
            event_kind,
            event_json,
            entry.revision.get() as i64,
        ],
    )?;
    Ok(())
}

fn row_to_audit(row: &Row<'_>) -> rusqlite::Result<ActionAuditEntry> {
    let action_id = ActionId(row.get(0)?);
    let at: i64 = row.get(1)?;
    let actor_json: String = row.get(2)?;
    let actor: Actor = serde_json::from_str(&actor_json).map_err(|e| col_err(2, e))?;
    let event_json: String = row.get(3)?;
    let event: AuditEvent = serde_json::from_str(&event_json).map_err(|e| col_err(3, e))?;
    let revision: i64 = row.get(4)?;
    Ok(ActionAuditEntry {
        action_id,
        at: Timestamp::from_millis(at),
        actor,
        event,
        revision: rev(revision),
    })
}

const AUDIT_COLUMNS: &str = "action_id, at, actor_json, event_json, revision";

/// The audit trail for one action, oldest first.
pub fn audit_for(conn: &Connection, action_id: ActionId) -> StoreResult<Vec<ActionAuditEntry>> {
    let sql = format!(
        "SELECT {AUDIT_COLUMNS} FROM action_history WHERE action_id = ? ORDER BY at ASC, id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![action_id.get()], row_to_audit)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// The most recent audit entries across all actions, newest first. `limit =
/// 0` means unlimited.
pub fn recent_audit(conn: &Connection, limit: u32) -> StoreResult<Vec<ActionAuditEntry>> {
    let mut sql = format!("SELECT {AUDIT_COLUMNS} FROM action_history ORDER BY at DESC, id DESC");
    if limit > 0 {
        sql.push_str(" LIMIT ?");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![limit], row_to_audit)?;
        return Ok(rows.collect::<Result<Vec<_>, _>>()?);
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_audit)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::db::Database;
    use litecord_types::tasks::{TaskDraft, TaskPriority};

    fn sample_action() -> AgentAction {
        AgentAction::CreateTask {
            task: TaskDraft {
                title: "test".into(),
                description: None,
                priority: TaskPriority::default(),
                due_at: None,
                related_users: Vec::new(),
                conversation_id: None,
                parent_id: None,
                source: None,
            },
        }
    }

    #[test]
    fn proposal_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        let action = sample_action();
        let actor = Actor::Agent {
            harness: "test".into(),
            run_id: None,
        };
        let payload_json = serde_json::to_string(&action).unwrap();
        let id = db
            .write(|tx| {
                insert_proposal(
                    tx,
                    &NewProposal {
                        action: &action,
                        actor: &actor,
                        class: CapabilityClass::LocalWrite,
                        identity: DiscordIdentity::UserSocialSdk,
                        payload_json: &payload_json,
                        payload_hash: "hash1",
                        status: ActionStatus::PendingApproval,
                        based_on_revision: Revision(1),
                        rationale: Some("because"),
                    },
                )
            })
            .unwrap()
            .value;

        let loaded = db.read(|r| get(r, id)).unwrap().unwrap();
        assert_eq!(loaded.action, action);
        assert_eq!(loaded.actor, actor);
        assert_eq!(loaded.status, ActionStatus::PendingApproval);

        db.write(|tx| set_status(tx, id, ActionStatus::Approved, Origin::UserProvided))
            .unwrap();
        assert_eq!(
            db.read(|r| get(r, id)).unwrap().unwrap().status,
            ActionStatus::Approved
        );
    }

    #[test]
    fn approval_is_single_use_and_revocable() {
        let db = Database::open_in_memory().unwrap();
        let action = sample_action();
        let actor = Actor::User;
        let payload_json = serde_json::to_string(&action).unwrap();
        let action_id = db
            .write(|tx| {
                insert_proposal(
                    tx,
                    &NewProposal {
                        action: &action,
                        actor: &actor,
                        class: CapabilityClass::LocalWrite,
                        identity: DiscordIdentity::UserSocialSdk,
                        payload_json: &payload_json,
                        payload_hash: "hash1",
                        status: ActionStatus::PendingApproval,
                        based_on_revision: Revision(1),
                        rationale: None,
                    },
                )
            })
            .unwrap()
            .value;

        db.write(|tx| {
            insert_approval(
                tx,
                action_id,
                "hash1",
                "nonce-1",
                Timestamp::from_millis(0),
                Timestamp::from_millis(10_000),
                false,
            )
        })
        .unwrap();

        let consumed1 = db
            .write(|tx| consume_approval(tx, "nonce-1", Timestamp::from_millis(1)))
            .unwrap()
            .value;
        assert!(consumed1);
        let consumed2 = db
            .write(|tx| consume_approval(tx, "nonce-1", Timestamp::from_millis(2)))
            .unwrap()
            .value;
        assert!(!consumed2, "approval must be single-use");

        db.write(|tx| {
            insert_approval(
                tx,
                action_id,
                "hash1",
                "nonce-2",
                Timestamp::from_millis(0),
                Timestamp::from_millis(10_000),
                false,
            )
        })
        .unwrap();
        let revoked = db
            .write(|tx| revoke_approvals(tx, action_id, Timestamp::from_millis(3)))
            .unwrap()
            .value;
        assert_eq!(revoked, 1);
        let rec = db.read(|r| get_approval(r, "nonce-2")).unwrap().unwrap();
        assert!(rec.consumed_at.is_some());
    }

    #[test]
    fn audit_trail_is_ordered() {
        let db = Database::open_in_memory().unwrap();
        let action = sample_action();
        let actor = Actor::User;
        let payload_json = serde_json::to_string(&action).unwrap();
        let action_id = db
            .write(|tx| {
                insert_proposal(
                    tx,
                    &NewProposal {
                        action: &action,
                        actor: &actor,
                        class: CapabilityClass::LocalWrite,
                        identity: DiscordIdentity::UserSocialSdk,
                        payload_json: &payload_json,
                        payload_hash: "hash1",
                        status: ActionStatus::PendingApproval,
                        based_on_revision: Revision(1),
                        rationale: None,
                    },
                )
            })
            .unwrap()
            .value;
        db.write(|tx| {
            append_audit(
                tx,
                &ActionAuditEntry {
                    action_id,
                    at: Timestamp::from_millis(1),
                    actor: Actor::User,
                    event: AuditEvent::Proposed,
                    revision: Revision(1),
                },
            )
        })
        .unwrap();
        db.write(|tx| {
            append_audit(
                tx,
                &ActionAuditEntry {
                    action_id,
                    at: Timestamp::from_millis(2),
                    actor: Actor::User,
                    event: AuditEvent::Approved { edited: false },
                    revision: Revision(2),
                },
            )
        })
        .unwrap();

        let trail = db.read(|r| audit_for(r, action_id)).unwrap();
        assert_eq!(trail.len(), 2);
        assert_eq!(trail[0].event, AuditEvent::Proposed);
        assert_eq!(trail[1].event, AuditEvent::Approved { edited: false });

        let recent = db.read(|r| recent_audit(r, 1)).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].event, AuditEvent::Approved { edited: false });
    }
}
