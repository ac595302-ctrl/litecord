//! The Action Engine proper.
//!
//! Authority is split by type:
//!
//! * [`ActionProposer`] — can propose actions and read proposals. This is the
//!   only handle given to agents/tools (via `litecord-agent`).
//! * [`ActionEngine`] — additionally approves, rejects and executes. Held
//!   only by the application layer, which calls `approve` exclusively in
//!   response to explicit user input.
//!
//! Approval tokens are therefore unreachable from agent code paths.

use std::sync::Arc;

use litecord_core::config::AgentConfig;
use litecord_store::repos;
use litecord_store::repos::actions::NewProposal;
use litecord_store::repos::Connection;
use litecord_store::Database;
use litecord_types::actions::*;
use litecord_types::capability::Capability;
use litecord_types::ids::ActionId;
use litecord_types::provenance::{DiscordIdentity, Origin};
use litecord_types::social::ConversationKind;
use litecord_types::tasks::TaskStatus;
use litecord_types::trust::AgentVisibility;
use litecord_types::{DurationMs, Revision};

use crate::error::ActionError;
use crate::executor::{origin_for, ActionExecutor, ExecutionOutcome};
use crate::hash::{canonical_json, hash_json, payload_hash};
use crate::policy::{ActionPolicy, PolicyDecision};
use crate::token::{ApprovalToken, TokenIssuer};

/// Maximum message length accepted for Discord writes.
pub const MAX_MESSAGE_CHARS: usize = 2_000;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProposeOutcome {
    /// Policy allowed immediate execution; it has been executed and audited.
    Executed {
        action_id: ActionId,
        result: ExecutionOutcome,
    },
    /// Persisted; waiting for the user.
    PendingApproval { action_id: ActionId },
}

struct Inner {
    db: Database,
    policy: ActionPolicy,
    issuer: TokenIssuer,
    approval_ttl: DurationMs,
    default_visibility: AgentVisibility,
    executor: Arc<dyn ActionExecutor>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionEngine")
            .field("policy", &self.policy)
            .field("approval_ttl", &self.approval_ttl)
            .finish_non_exhaustive()
    }
}

/// Full-authority engine handle (application layer only).
#[derive(Debug, Clone)]
pub struct ActionEngine {
    inner: Arc<Inner>,
}

/// Propose-only handle (agents, tools, features).
#[derive(Debug, Clone)]
pub struct ActionProposer {
    inner: Arc<Inner>,
}

impl ActionEngine {
    pub fn new(
        db: Database,
        cfg: &AgentConfig,
        executor: Arc<dyn ActionExecutor>,
    ) -> Result<Self, ActionError> {
        recover_interrupted(&db)?;
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                policy: ActionPolicy::from_config(cfg),
                issuer: TokenIssuer::new()?,
                approval_ttl: DurationMs::from_secs(cfg.approval_ttl_secs),
                default_visibility: cfg.default_visibility,
                executor,
            }),
        })
    }

    /// A handle that can only propose.
    pub fn proposer(&self) -> ActionProposer {
        ActionProposer {
            inner: self.inner.clone(),
        }
    }

    pub async fn propose(
        &self,
        action: AgentAction,
        actor: Actor,
        based_on: Revision,
        rationale: Option<String>,
    ) -> Result<ProposeOutcome, ActionError> {
        propose(
            &self.inner,
            action,
            actor,
            DiscordIdentity::UserSocialSdk,
            based_on,
            rationale,
        )
        .await
    }

    /// Propose an action to be performed as a specific Discord identity.
    /// Bot and user identities are never substituted for one another: the
    /// chosen identity is part of the proposal, shown to the user, and used
    /// by the executor.
    pub async fn propose_as(
        &self,
        action: AgentAction,
        actor: Actor,
        identity: DiscordIdentity,
        based_on: Revision,
        rationale: Option<String>,
    ) -> Result<ProposeOutcome, ActionError> {
        propose(&self.inner, action, actor, identity, based_on, rationale).await
    }

    pub fn get(&self, id: ActionId) -> Result<Option<ActionProposal>, ActionError> {
        get(&self.inner, id)
    }

    pub fn pending(&self, limit: u32) -> Result<Vec<ActionProposal>, ActionError> {
        pending(&self.inner, limit)
    }

    pub fn audit(&self, id: ActionId) -> Result<Vec<ActionAuditEntry>, ActionError> {
        Ok(self.inner.db.read(|r| repos::actions::audit_for(r, id))?)
    }

    /// Approve a pending proposal on behalf of the **user**, optionally with
    /// an edited payload (must be the same kind of action). Returns a token
    /// bound to the exact approved payload. Any previous approval for this
    /// action is revoked.
    pub fn approve(
        &self,
        id: ActionId,
        edited: Option<AgentAction>,
    ) -> Result<ApprovalToken, ActionError> {
        let inner = &self.inner;
        let now = inner.db.now();
        let committed = inner.db.write(|tx| -> Result<ApprovalToken, ActionError> {
            let proposal = repos::actions::get(tx, id)?.ok_or(ActionError::NotFound(id))?;
            if !matches!(
                proposal.status,
                ActionStatus::PendingApproval | ActionStatus::Approved
            ) {
                return Err(ActionError::WrongStatus {
                    id,
                    status: proposal.status,
                    expected: "pending_approval",
                });
            }
            let is_edit = edited.as_ref().is_some_and(|e| *e != proposal.action);
            let action = match edited {
                Some(e) if is_edit => {
                    if e.kind() != proposal.action.kind() {
                        return Err(ActionError::Invalid(
                            "an edit must keep the same kind of action".into(),
                        ));
                    }
                    validate_static(&e)?;
                    let json = canonical_json(&e)?;
                    let hash = hash_json(&json);
                    repos::actions::update_payload(tx, id, &e, &json, &hash)?;
                    e
                }
                _ => proposal.action,
            };
            validate_state(inner, tx, &action, &Actor::User, proposal.identity)?;
            let hash = payload_hash(&action)?;
            repos::actions::revoke_approvals(tx, id, now)?;
            let token = inner.issuer.issue(id, &hash, now, inner.approval_ttl)?;
            repos::actions::insert_approval(
                tx,
                id,
                &hash,
                token.nonce(),
                token.approved_at(),
                token.expires_at(),
                is_edit,
            )?;
            repos::actions::set_status(tx, id, ActionStatus::Approved, Origin::UserProvided)?;
            audit(
                tx,
                id,
                &Actor::User,
                AuditEvent::Approved { edited: is_edit },
            )?;
            Ok(token)
        })?;
        tracing::info!(action_id = %id, "action approved by user");
        Ok(committed.value)
    }

    /// Reject a pending or approved proposal (user decision).
    pub fn reject(&self, id: ActionId) -> Result<(), ActionError> {
        let now = self.inner.db.now();
        self.inner.db.write(|tx| -> Result<(), ActionError> {
            let p = repos::actions::get(tx, id)?.ok_or(ActionError::NotFound(id))?;
            if p.status.is_terminal() || p.status == ActionStatus::Executing {
                return Err(ActionError::WrongStatus {
                    id,
                    status: p.status,
                    expected: "pending_approval or approved",
                });
            }
            repos::actions::revoke_approvals(tx, id, now)?;
            repos::actions::set_status(tx, id, ActionStatus::Rejected, Origin::UserProvided)?;
            audit(tx, id, &Actor::User, AuditEvent::Rejected)
        })?;
        Ok(())
    }

    /// Execute an approved proposal. Verifies the token (authenticity,
    /// expiry, single use, payload hash), **revalidates** against current
    /// state, then runs the executor and records the result.
    pub async fn execute(&self, token: &ApprovalToken) -> Result<ExecutionOutcome, ActionError> {
        let inner = &self.inner;
        let (action, actor, identity) = claim_for_execution(inner, token)?;
        run_executor(inner, token.action_id(), &action, &actor, identity).await
    }
}

impl ActionProposer {
    pub async fn propose(
        &self,
        action: AgentAction,
        actor: Actor,
        based_on: Revision,
        rationale: Option<String>,
    ) -> Result<ProposeOutcome, ActionError> {
        propose(
            &self.inner,
            action,
            actor,
            DiscordIdentity::UserSocialSdk,
            based_on,
            rationale,
        )
        .await
    }

    /// Propose an action to be performed as a specific Discord identity.
    /// Bot and user identities are never substituted for one another: the
    /// chosen identity is part of the proposal, shown to the user, and used
    /// by the executor.
    pub async fn propose_as(
        &self,
        action: AgentAction,
        actor: Actor,
        identity: DiscordIdentity,
        based_on: Revision,
        rationale: Option<String>,
    ) -> Result<ProposeOutcome, ActionError> {
        propose(&self.inner, action, actor, identity, based_on, rationale).await
    }

    pub fn get(&self, id: ActionId) -> Result<Option<ActionProposal>, ActionError> {
        get(&self.inner, id)
    }

    pub fn pending(&self, limit: u32) -> Result<Vec<ActionProposal>, ActionError> {
        pending(&self.inner, limit)
    }
}

/// Verify the token and atomically move the proposal to `Executing` after
/// revalidating it against the current state.
fn claim_for_execution(
    inner: &Inner,
    token: &ApprovalToken,
) -> Result<(AgentAction, Actor, DiscordIdentity), ActionError> {
    let id = token.action_id();
    let now = inner.db.now();

    if let Err(e) = inner.issuer.verify(token, now) {
        if matches!(e, ActionError::TokenExpired) {
            inner.db.write(|tx| -> Result<(), ActionError> {
                repos::actions::revoke_approvals(tx, id, now)?;
                let p = repos::actions::get(tx, id)?;
                if p.is_some_and(|p| p.status == ActionStatus::Approved) {
                    repos::actions::set_status(
                        tx,
                        id,
                        ActionStatus::Expired,
                        Origin::LocalApplication,
                    )?;
                    audit(tx, id, &Actor::System, AuditEvent::Expired)?;
                }
                Ok(())
            })?;
        }
        return Err(e);
    }

    // A failed revalidation must still commit the invalidation, hence the
    // nested Result: the outer error aborts the transaction, the inner one
    // is returned after commit.
    type Claimed = Result<(AgentAction, Actor, DiscordIdentity), ActionError>;
    let committed = inner.db.write(|tx| -> Result<Claimed, ActionError> {
        let p = repos::actions::get(tx, id)?.ok_or(ActionError::NotFound(id))?;
        if p.status != ActionStatus::Approved {
            return Err(ActionError::WrongStatus {
                id,
                status: p.status,
                expected: "approved",
            });
        }
        // The stored payload must still hash to the approved hash.
        let stored_json =
            repos::actions::get_payload_json(tx, id)?.ok_or(ActionError::NotFound(id))?;
        if hash_json(&stored_json) != token.payload_hash()
            || p.payload_hash != token.payload_hash()
        {
            return Err(ActionError::PayloadMismatch);
        }
        let approval =
            repos::actions::get_approval(tx, token.nonce())?.ok_or(ActionError::InvalidToken)?;
        if approval.action_id != id || approval.payload_hash != token.payload_hash() {
            return Err(ActionError::InvalidToken);
        }
        if !repos::actions::consume_approval(tx, token.nonce(), now)? {
            return Err(ActionError::TokenConsumed);
        }
        let current = tx.revision();
        if current.is_after(p.based_on_revision) {
            tracing::debug!(action_id = %id, based_on = %p.based_on_revision, %current, "state advanced since proposal; revalidating");
        }
        if let Err(e) = validate_state(inner, tx, &p.action, &p.actor, p.identity) {
            repos::actions::set_status(
                tx,
                id,
                ActionStatus::Invalidated,
                Origin::LocalApplication,
            )?;
            audit(
                tx,
                id,
                &Actor::System,
                AuditEvent::Invalidated {
                    reason: e.to_string(),
                },
            )?;
            return Ok(Err(ActionError::RevalidationFailed(e.to_string())));
        }
        repos::actions::set_status(tx, id, ActionStatus::Executing, Origin::UserProvided)?;
        audit(tx, id, &Actor::User, AuditEvent::ExecutionStarted)?;
        Ok(Ok((p.action, p.actor, p.identity)))
    })?;
    committed.value
}

/// Actions left `Executing` by a previous run (the app closed or crashed
/// mid-action). Nothing would ever finish them: they could not be rejected
/// or approved again and dropped out of the approvals list. Whether Discord
/// applied them is unknown, so they become `Uncertain`, with the reason in
/// the audit log.
fn recover_interrupted(db: &litecord_store::Database) -> Result<(), ActionError> {
    db.write(|tx| -> Result<(), ActionError> {
        for p in repos::actions::list(tx, Some(&[ActionStatus::Executing]), 10_000)? {
            repos::actions::set_status(tx, p.id, ActionStatus::Uncertain, Origin::LocalApplication)?;
            audit(
                tx,
                p.id,
                &Actor::System,
                AuditEvent::Failed {
                    error: "Litecord closed before this action finished; check Discord to see whether it happened".into(),
                },
            )?;
        }
        Ok(())
    })?;
    Ok(())
}

fn get(inner: &Inner, id: ActionId) -> Result<Option<ActionProposal>, ActionError> {
    Ok(inner.db.read(|r| repos::actions::get(r, id))?)
}

fn pending(inner: &Inner, limit: u32) -> Result<Vec<ActionProposal>, ActionError> {
    Ok(inner.db.read(|r| {
        repos::actions::list(
            r,
            Some(&[ActionStatus::PendingApproval, ActionStatus::Approved]),
            limit,
        )
    })?)
}

fn audit(
    tx: &litecord_store::WriteTx<'_>,
    id: ActionId,
    actor: &Actor,
    event: AuditEvent,
) -> Result<(), ActionError> {
    repos::actions::append_audit(
        tx,
        &ActionAuditEntry {
            action_id: id,
            at: tx.now(),
            actor: actor.clone(),
            event,
            revision: tx.revision(),
        },
    )?;
    Ok(())
}

async fn propose(
    inner: &Inner,
    action: AgentAction,
    actor: Actor,
    identity: DiscordIdentity,
    based_on: Revision,
    rationale: Option<String>,
) -> Result<ProposeOutcome, ActionError> {
    let span = tracing::info_span!("action_propose", kind = action.kind(), actor = %actor.label());
    let (id, decision) =
        span.in_scope(|| record_proposal(inner, &action, &actor, identity, based_on, rationale))?;
    match decision {
        PolicyDecision::Execute => {
            let result = run_executor(inner, id, &action, &actor, identity).await?;
            Ok(ProposeOutcome::Executed {
                action_id: id,
                result,
            })
        }
        _ => {
            tracing::info!(action_id = %id, "action awaiting approval");
            Ok(ProposeOutcome::PendingApproval { action_id: id })
        }
    }
}

/// Validate, apply policy and persist the proposal (synchronous part).
fn record_proposal(
    inner: &Inner,
    action: &AgentAction,
    actor: &Actor,
    identity: DiscordIdentity,
    based_on: Revision,
    rationale: Option<String>,
) -> Result<(ActionId, PolicyDecision), ActionError> {
    validate_static(action)?;
    let decision = inner.policy.decide(action, actor);
    let json = canonical_json(action)?;
    let hash = hash_json(&json);
    let class = action.capability_class();

    let committed = inner
        .db
        .write(|tx| -> Result<Result<ActionId, ActionError>, ActionError> {
            // State validation failures are returned to the caller but not
            // persisted: nothing was proposed.
            if let Err(e) = validate_state(inner, tx, action, actor, identity) {
                return Ok(Err(e));
            }
            let status = match &decision {
                PolicyDecision::Deny { .. } => ActionStatus::Rejected,
                PolicyDecision::RequireApproval => ActionStatus::PendingApproval,
                PolicyDecision::Execute => ActionStatus::Executing,
            };
            let id = repos::actions::insert_proposal(
                tx,
                &NewProposal {
                    action,
                    actor,
                    class,
                    identity,
                    payload_json: &json,
                    payload_hash: &hash,
                    status,
                    based_on_revision: based_on,
                    rationale: rationale.as_deref(),
                },
            )?;
            audit(tx, id, actor, AuditEvent::Proposed)?;
            match &decision {
                PolicyDecision::Deny { reason } => {
                    audit(
                        tx,
                        id,
                        &Actor::System,
                        AuditEvent::Denied {
                            reason: reason.clone(),
                        },
                    )?;
                    // Commit the audit trail, then report the denial.
                    return Ok(Err(ActionError::PolicyDenied(reason.clone())));
                }
                PolicyDecision::Execute => {
                    audit(tx, id, &Actor::System, AuditEvent::AutoApproved)?;
                    audit(tx, id, &Actor::System, AuditEvent::ExecutionStarted)?;
                }
                PolicyDecision::RequireApproval => {}
            }
            Ok(Ok(id))
        })?;
    Ok((committed.value?, decision))
}

async fn run_executor(
    inner: &Inner,
    id: ActionId,
    action: &AgentAction,
    actor: &Actor,
    identity: DiscordIdentity,
) -> Result<ExecutionOutcome, ActionError> {
    let mut result = inner.executor.execute(id, action, actor, identity).await;
    inner.db.write(|tx| -> Result<(), ActionError> {
        if result.is_err() && identity == DiscordIdentity::UserSession {
            if let Some(outcome) = crate::executor::observed_outcome(tx, id, action)? {
                result = Ok(outcome);
            }
        }
        match &result {
            Ok(outcome) => {
                repos::actions::set_status(tx, id, ActionStatus::Executed, origin_for(actor))?;
                audit(
                    tx,
                    id,
                    &Actor::System,
                    AuditEvent::Executed {
                        summary: outcome.summary.clone(),
                    },
                )
            }
            Err(e) => {
                repos::actions::set_status(
                    tx,
                    id,
                    if matches!(e, ActionError::Uncertain(_)) {
                        ActionStatus::Uncertain
                    } else {
                        ActionStatus::Failed
                    },
                    origin_for(actor),
                )?;
                audit(
                    tx,
                    id,
                    &Actor::System,
                    AuditEvent::Failed {
                        error: e.to_string(),
                    },
                )
            }
        }
    })?;
    result
}

/// Checks that need no database.
pub fn validate_static(action: &AgentAction) -> Result<(), ActionError> {
    let non_empty = |s: &str, what: &str| {
        if s.trim().is_empty() {
            Err(ActionError::Invalid(format!("{what} must not be empty")))
        } else {
            Ok(())
        }
    };
    match action {
        AgentAction::SendMessage { content, .. }
        | AgentAction::EditMessage { content, .. }
        | AgentAction::DraftMessage { content, .. } => {
            non_empty(content, "message content")?;
            if content.chars().count() > MAX_MESSAGE_CHARS {
                return Err(ActionError::Invalid(format!(
                    "message content exceeds {MAX_MESSAGE_CHARS} characters"
                )));
            }
        }
        AgentAction::CreateReminder { reminder } => non_empty(&reminder.title, "reminder title")?,
        AgentAction::CreateTask { task } => non_empty(&task.title, "task title")?,
        AgentAction::AddNote { note, .. } => non_empty(note, "note")?,
        _ => {}
    }
    Ok(())
}

/// The capability an action needs. Message writes in guild channels need
/// `GuildMessages` (a bot can; the Social SDK cannot); in DMs the per-action
/// `Dm*` capability applies.
fn required_capability(
    conn: &Connection,
    action: &AgentAction,
) -> Result<Option<Capability>, ActionError> {
    let conversation = match action {
        AgentAction::SendMessage {
            target: MessageTarget::Conversation { conversation_id },
            ..
        } => Some(*conversation_id),
        AgentAction::EditMessage { message_id, .. } | AgentAction::DeleteMessage { message_id } => {
            repos::messages::get(conn, *message_id)?.map(|m| m.message.conversation_id)
        }
        _ => None,
    };
    if let Some(id) = conversation {
        if repos::conversations::get(conn, id)?
            .is_some_and(|c| c.conversation.kind == ConversationKind::GuildChannel)
        {
            return Ok(Some(Capability::GuildMessages));
        }
    }
    Ok(action.required_capability())
}

/// Checks against current state. Run at proposal, approval **and** right
/// before execution (revalidation).
fn validate_state(
    inner: &Inner,
    conn: &Connection,
    action: &AgentAction,
    actor: &Actor,
    identity: DiscordIdentity,
) -> Result<(), ActionError> {
    let is_agent = matches!(actor, Actor::Agent { .. });
    let invalid = |m: &str| Err(ActionError::Invalid(m.to_owned()));

    // An executor without a backend (e.g. the MCP process) reports no
    // capabilities; Discord writes proposed there are checked again when the
    // application executes them.
    let caps = inner.executor.capabilities(identity);
    if identity == DiscordIdentity::UserSession
        && !caps.entries.is_empty()
        && action.capability_class() == CapabilityClass::DiscordWrite
        && !caps.is_usable(action.required_capability().unwrap_or(Capability::DmSend))
    {
        return invalid(
            "the account connection is signed out or this write capability is disabled",
        );
    }
    if identity == DiscordIdentity::ApplicationBot {
        // The bot identity may only post/edit/delete in guild channels; it
        // never acts on the user's social graph, presence or local data.
        if !matches!(
            action,
            AgentAction::SendMessage {
                target: MessageTarget::Conversation { .. },
                ..
            } | AgentAction::EditMessage { .. }
                | AgentAction::DeleteMessage { .. }
        ) {
            return Err(ActionError::Invalid(format!(
                "`{}` cannot be performed as the application bot",
                action.kind()
            )));
        }
    }
    if let Some(cap) = required_capability(conn, action)? {
        if !caps.entries.is_empty() && !caps.is_usable(cap) {
            return Err(ActionError::Invalid(format!(
                "the connected Discord backend does not support {cap:?}"
            )));
        }
    }

    let visible = |conversation_id| -> Result<AgentVisibility, ActionError> {
        Ok(repos::conversations::effective_visibility(
            conn,
            conversation_id,
            inner.default_visibility,
        )?)
    };

    if let AgentAction::SendMessage {
        target,
        reply_to: Some(reply_to),
        ..
    } = action
    {
        let MessageTarget::Conversation { conversation_id } = target else {
            return invalid("a reply must target the conversation of the replied-to message");
        };
        let Some(m) = repos::messages::get(conn, *reply_to)? else {
            return invalid("replied-to message not found");
        };
        if m.message.conversation_id != *conversation_id {
            return invalid("the replied-to message is in another conversation");
        }
        if !caps.entries.is_empty() && !caps.is_usable(Capability::Replies) {
            return invalid("this identity cannot send replies; send a plain message instead");
        }
    }

    match action {
        AgentAction::SendMessage { target, .. } => match target {
            MessageTarget::Conversation { conversation_id } => {
                let Some(c) = repos::conversations::get(conn, *conversation_id)? else {
                    return invalid("conversation not found");
                };
                if is_agent && visible(*conversation_id)? == AgentVisibility::Hidden {
                    return invalid("conversation is hidden from agents");
                }
                let guild_channel = c.conversation.kind == ConversationKind::GuildChannel;
                if identity == DiscordIdentity::ApplicationBot && !guild_channel {
                    return invalid("the application bot only posts in guild channels");
                }
                // Guild channels need a backend that can write there (a bot;
                // the Social SDK cannot). An executor without a backend (MCP
                // process) defers this check to execution time.
                if guild_channel
                    && !caps.entries.is_empty()
                    && !caps.is_usable(Capability::GuildMessages)
                {
                    return invalid(
                        "this identity cannot post in guild channels; propose it as the bot or open in Discord",
                    );
                }
                if let Some(r) = c.conversation.recipient_id {
                    if repos::relationships::get(conn, r)?.is_some_and(|rel| rel.is_blocked()) {
                        return invalid("recipient is blocked");
                    }
                }
            }
            MessageTarget::User { user_id } => {
                if repos::users::get(conn, *user_id)?.is_none() {
                    return invalid("recipient not found");
                }
                if repos::relationships::get(conn, *user_id)?.is_some_and(|rel| rel.is_blocked()) {
                    return invalid("recipient is blocked");
                }
                if is_agent {
                    if let Some(c) = repos::conversations::find_dm_by_recipient(conn, *user_id)? {
                        if visible(c.conversation.id)? == AgentVisibility::Hidden {
                            return invalid("conversation is hidden from agents");
                        }
                    }
                }
            }
        },
        AgentAction::EditMessage { message_id, .. } | AgentAction::DeleteMessage { message_id } => {
            let Some(m) = repos::messages::get(conn, *message_id)? else {
                return invalid("message not found");
            };
            if m.deleted {
                return invalid("message was deleted");
            }
            // Only messages authored by the acting identity are editable.
            let me = repos::accounts::current(conn, identity)?;
            if me.map(|a| a.user_id) != Some(m.message.author_id) {
                return invalid("only your own messages can be edited or deleted");
            }
            if is_agent && visible(m.message.conversation_id)? == AgentVisibility::Hidden {
                return invalid("conversation is hidden from agents");
            }
        }
        AgentAction::RelationshipChange { user_id, .. } => {
            if repos::users::get(conn, *user_id)?.is_none() {
                return invalid("user not found");
            }
        }
        AgentAction::CompleteTask { task_id } => match repos::tasks::get(conn, *task_id)? {
            None => return invalid("task not found"),
            Some(t)
                if matches!(
                    t.status,
                    TaskStatus::Done | TaskStatus::Cancelled | TaskStatus::Dismissed
                ) =>
            {
                return invalid("task is already closed")
            }
            Some(_) => {}
        },
        AgentAction::BookmarkMessage { message_id, .. } => {
            let Some(m) = repos::messages::get(conn, *message_id)? else {
                return invalid("message not found");
            };
            if is_agent && !visible(m.message.conversation_id)?.allows_content() {
                return invalid("conversation content is not visible to agents");
            }
        }
        AgentAction::DraftMessage {
            conversation_id, ..
        } => {
            if repos::conversations::get(conn, *conversation_id)?.is_none() {
                return invalid("conversation not found");
            }
            if is_agent && visible(*conversation_id)? == AgentVisibility::Hidden {
                return invalid("conversation is hidden from agents");
            }
        }
        AgentAction::CreateReminder { .. }
        | AgentAction::CreateTask { .. }
        | AgentAction::AddNote { .. }
        | AgentAction::ChangePresence { .. } => {}
    }
    Ok(())
}
