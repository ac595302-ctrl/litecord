//! Durable outbound intent and exact receipt reconciliation. Never retransmits.
use crate::{StoreResult, WriteTx};
use litecord_types::actions::OutboundState;
use litecord_types::provenance::{DiscordIdentity, Origin};
use litecord_types::{ActionId, ConversationId, MessageId, Timestamp, UserId};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct OutboundOperation {
    pub action_id: ActionId,
    pub account_id: UserId,
    pub conversation_id: Option<ConversationId>,
    pub kind: String,
    pub state: OutboundState,
    pub remote_id: Option<MessageId>,
    pub started_at: Timestamp,
    pub confirmed_at: Option<Timestamp>,
    pub error: Option<String>,
}

pub fn start(
    tx: &WriteTx<'_>,
    action: ActionId,
    account: UserId,
    conversation: Option<ConversationId>,
    kind: &str,
    nonce: &str,
    remote: Option<MessageId>,
) -> StoreResult<()> {
    tx.execute("INSERT INTO outbound_operations (action_id,nonce,account_id,conversation_id,kind,state,remote_id,started_at) VALUES (?1,?2,?3,?4,?5,'submitted',?6,?7)", params![action.get(),nonce,account.get() as i64,conversation.map(|v|v.get() as i64),kind,remote.map(|v|v.get() as i64),tx.now().as_millis()])?;
    Ok(())
}

pub fn finish(
    tx: &WriteTx<'_>,
    action: ActionId,
    state: OutboundState,
    remote: Option<MessageId>,
    error: Option<&str>,
) -> StoreResult<()> {
    tx.execute("UPDATE outbound_operations SET state=?2,remote_id=COALESCE(?3,remote_id),confirmed_at=CASE WHEN ?2='confirmed' THEN ?4 ELSE confirmed_at END,error=?5 WHERE action_id=?1 AND state != 'confirmed'", params![action.get(),state.as_str(),remote.map(|v|v.get() as i64),tx.now().as_millis(),error])?;
    Ok(())
}

pub fn confirmed_message(conn: &Connection, action: ActionId) -> StoreResult<Option<MessageId>> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT remote_id FROM outbound_operations WHERE action_id=?1 AND state='confirmed'",
            [action.get()],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(id.map(|v| MessageId(v as u64)))
}

pub fn is_confirmed(conn: &Connection, action: ActionId) -> StoreResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM outbound_operations WHERE action_id=?1 AND state='confirmed')",
        [action.get()],
        |r| r.get(0),
    )?)
}

/// Only an exact nonce, account and channel receipt can confirm a send.
pub fn observe_send(
    tx: &WriteTx<'_>,
    nonce: &str,
    message: &litecord_types::social::Message,
) -> StoreResult<()> {
    let me = super::accounts::current(tx, DiscordIdentity::UserSession)?.map(|a| a.user_id);
    if me != Some(message.author_id) {
        return Ok(());
    }
    let n=tx.execute("UPDATE outbound_operations SET state='confirmed',remote_id=?1,conversation_id=?2,confirmed_at=?3,error=NULL WHERE nonce=?4 AND account_id=?5 AND kind='send_message' AND state IN ('submitted','uncertain') AND (conversation_id IS NULL OR conversation_id=?2)",params![message.id.get() as i64,message.conversation_id.get() as i64,tx.now().as_millis(),nonce,message.author_id.get() as i64])?;
    if n > 0 {
        publish_receipt(tx)?;
    }
    Ok(())
}

pub fn observe_edit(
    tx: &WriteTx<'_>,
    message: &litecord_types::social::Message,
) -> StoreResult<()> {
    let n=tx.execute("UPDATE outbound_operations SET state='confirmed',confirmed_at=?1,error=NULL WHERE kind='edit_message' AND remote_id=?2 AND conversation_id=?3 AND account_id=?4 AND state IN ('submitted','uncertain') AND EXISTS (SELECT 1 FROM action_proposals a WHERE a.id=action_id AND json_extract(a.payload_json,'$.content')=?5)",params![tx.now().as_millis(),message.id.get() as i64,message.conversation_id.get() as i64,message.author_id.get() as i64,message.content.as_ref()])?;
    if n > 0 {
        publish_receipt(tx)?;
    }
    Ok(())
}

pub fn observe_delete(
    tx: &WriteTx<'_>,
    message: MessageId,
    conversation: ConversationId,
) -> StoreResult<()> {
    let me =
        super::accounts::current(tx, DiscordIdentity::UserSession)?.map(|a| a.user_id.get() as i64);
    let n=tx.execute("UPDATE outbound_operations SET state='confirmed',confirmed_at=?1,error=NULL WHERE kind='delete_message' AND remote_id=?2 AND conversation_id=?3 AND account_id=?4 AND state IN ('submitted','uncertain')",params![tx.now().as_millis(),message.get() as i64,conversation.get() as i64,me])?;
    if n > 0 {
        publish_receipt(tx)?;
    }
    Ok(())
}

fn publish_receipt(tx: &WriteTx<'_>) -> StoreResult<()> {
    let ids = {
        let mut stmt=tx.prepare("SELECT id FROM action_proposals WHERE identity='user_session' AND status IN ('uncertain','executing','failed') AND id IN (SELECT action_id FROM outbound_operations WHERE state='confirmed')")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for id in ids {
        super::actions::set_status(
            tx,
            ActionId(id),
            litecord_types::actions::ActionStatus::Executed,
            Origin::LocalApplication,
        )?;
        super::actions::append_audit(
            tx,
            &litecord_types::actions::ActionAuditEntry {
                action_id: ActionId(id),
                at: tx.now(),
                actor: litecord_types::actions::Actor::System,
                event: litecord_types::actions::AuditEvent::Executed {
                    summary: "confirmed by Discord observation".into(),
                },
                revision: tx.revision(),
            },
        )?;
    }
    Ok(())
}

/// Crashed processes cannot know whether a submitted request reached Discord.
pub fn recover_interrupted(tx: &WriteTx<'_>) -> StoreResult<()> {
    tx.execute("UPDATE outbound_operations SET state='uncertain',error='Application stopped before confirmation; refresh to reconcile' WHERE state IN ('pending','submitted')",[])?;
    tx.execute("UPDATE action_proposals SET status='uncertain' WHERE identity='user_session' AND status='executing' AND id IN (SELECT action_id FROM outbound_operations WHERE state='uncertain')",[])?;
    Ok(())
}

pub fn recent(conn: &Connection, limit: u32) -> StoreResult<Vec<OutboundOperation>> {
    let mut stmt=conn.prepare("SELECT action_id,account_id,conversation_id,kind,state,remote_id,started_at,confirmed_at,error FROM outbound_operations ORDER BY started_at DESC,action_id DESC LIMIT ?1")?;
    let rows = stmt.query_map([limit.min(100) as i64], |r| {
        let state: String = r.get(4)?;
        Ok(OutboundOperation {
            action_id: ActionId(r.get(0)?),
            account_id: UserId(r.get::<_, i64>(1)? as u64),
            conversation_id: r
                .get::<_, Option<i64>>(2)?
                .map(|v| ConversationId(v as u64)),
            kind: r.get(3)?,
            state: OutboundState::parse(&state).map_err(|e| crate::sql::col_err(4, e))?,
            remote_id: r.get::<_, Option<i64>>(5)?.map(|v| MessageId(v as u64)),
            started_at: Timestamp::from_millis(r.get(6)?),
            confirmed_at: r.get::<_, Option<i64>>(7)?.map(Timestamp::from_millis),
            error: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// A verified account relationship observation may settle a submitted mutation.
pub fn observe_relationships(tx: &WriteTx<'_>) -> StoreResult<()> {
    use litecord_types::actions::{AgentAction, RelationshipAction};
    use litecord_types::social::RelationshipKind;
    let Some(me) = super::accounts::current(tx, DiscordIdentity::UserSession)? else {
        return Ok(());
    };
    let ids = {
        let mut stmt=tx.prepare("SELECT action_id FROM outbound_operations WHERE account_id=?1 AND kind='relationship_change' AND state IN ('submitted','uncertain') LIMIT 100")?;
        let rows = stmt.query_map([me.user_id.get() as i64], |r| r.get::<_, i64>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut changed = false;
    for id in ids {
        let Some(proposal) = super::actions::get(tx, ActionId(id))? else {
            continue;
        };
        let AgentAction::RelationshipChange { user_id, action } = proposal.action else {
            continue;
        };
        let kind = super::relationships::get(tx, user_id)?
            .map(|r| r.discord)
            .unwrap_or(RelationshipKind::None);
        let satisfied = match action {
            RelationshipAction::Block => kind == RelationshipKind::Blocked,
            RelationshipAction::AcceptFriendRequest => kind == RelationshipKind::Friend,
            RelationshipAction::SendFriendRequest => matches!(
                kind,
                RelationshipKind::Friend | RelationshipKind::PendingOutgoing
            ),
            _ => matches!(kind, RelationshipKind::None | RelationshipKind::Implicit),
        };
        if satisfied {
            finish(tx, ActionId(id), OutboundState::Confirmed, None, None)?;
            changed = true;
        }
    }
    if changed {
        publish_receipt(tx)?;
    }
    Ok(())
}
