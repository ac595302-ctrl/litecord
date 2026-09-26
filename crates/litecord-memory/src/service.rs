//! [`MemoryService`]: the entry point for derived memory (V2 layers 3-4).
//!
//! It owns three responsibilities that must stay together:
//!
//! 1. **Deduplication and supersession** ([`record_tx`]/[`record`]): the same
//!    claim (by [`litecord_types::memory::MemoryFingerprint`]) either
//!    reinforces the existing item or, if the new observation contradicts it,
//!    supersedes it. An item is never overwritten in place and a
//!    user-confirmed item is not exempt from being superseded by a newer
//!    observation — confirmation raises trust in what was *true then*, it
//!    does not freeze the claim forever. The full chain stays queryable via
//!    [`MemoryService::history`].
//! 2. **Extraction wiring** ([`MemoryService::on_message_created`]): runs the
//!    configured [`CandidateExtractor`] over a newly created message and
//!    records whatever it finds, plus conversation-graph bookkeeping and
//!    task creation for local commitments.
//! 3. **Lifecycle** ([`MemoryService::confirm`], [`reject`](MemoryService::reject),
//!    [`gc`](MemoryService::gc), [`apply_retention`](MemoryService::apply_retention)):
//!    status transitions and cleanup that keep active memory small and
//!    relevant without ever deleting the provenance trail (rows move to a
//!    terminal status; they are not dropped, except for the unrelated
//!    event/agent-run retention pruning).

use std::sync::Arc;

use litecord_core::config::RetentionConfig;
use litecord_store::repos::memory::MemoryFilter;
use litecord_store::repos::tasks::TaskFilter;
use litecord_store::{repos, Database, WriteTx};
use litecord_types::entity::EntityId;
use litecord_types::memory::{
    MemoryItem, MemoryKind, MemoryPayload, MemoryStatus, NewMemory, RelationType,
};
use litecord_types::provenance::{Confidence, DiscordIdentity, Origin, SourceRef};
use litecord_types::tasks::{TaskDraft, TaskPriority, TaskStatus};
use litecord_types::{DurationMs, MemoryId, MessageId, Timestamp};

use crate::error::MemoryResult;
use crate::extract::{CandidateExtractor, ExtractionInput, HeuristicExtractor};

/// A large-but-finite `LIMIT` used where a repo query wants "all of them" and
/// the underlying `repos::*::Filter` only offers an explicit cap (never
/// `0`/unlimited). Comfortably larger than anything this desktop-scale store
/// will hold; documented here rather than silently relied upon.
const EFFECTIVELY_UNLIMITED: u32 = 1_000_000;

/// Maximum length (in characters) of a task title derived from a commitment.
const TASK_TITLE_MAX_CHARS: usize = 120;

/// Candidate items decay (expire) after this many days with zero retrievals.
const CANDIDATE_DECAY: DurationMs = DurationMs::from_days(30);

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.trim().to_owned()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// What [`record_tx`]/[`MemoryService::record`] actually did.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub enum RecordOutcome {
    /// No prior active memory shared this fingerprint (or it had none); a
    /// fresh row was inserted.
    Inserted(MemoryId),
    /// An active memory with the same fingerprint and an equal payload (or,
    /// for payload-less memories, equal content) already existed; its
    /// sources were merged and its confidence raised to the max of the two.
    Reinforced(MemoryId),
    /// An active memory with the same fingerprint existed but its payload
    /// (or content) differs from the new observation: this is treated as a
    /// contradiction/update. The new item was inserted and the old one moved
    /// to `Superseded`, pointing at the new one. Both remain queryable via
    /// [`MemoryService::history`].
    Superseded { old: MemoryId, new: MemoryId },
}

/// Insert-or-reconcile `m` inside an already open write transaction.
///
/// This is a free function (not a method) so other services composing a
/// larger transaction (e.g. the reducer's memory-extraction follow-up, or a
/// future action that both records a memory and does something else) can
/// call it without going through a nested [`Database::write`].
///
/// Dedup rule (V2 §43): if `m.fingerprint` is `Some` and an active memory
/// (`Candidate`/`Derived`/`UserConfirmed`) already carries it,
/// * equal payloads (or both `None` and equal content) are the *same claim*
///   observed again → [`repos::memory::reinforce`] (confidence becomes the
///   max of the two, sources are merged) → [`RecordOutcome::Reinforced`];
/// * anything else is a *changed* claim → the new item is inserted and
///   [`repos::memory::supersede`] retires the old one →
///   [`RecordOutcome::Superseded`].
///
/// Note that this applies even when the existing item is
/// `UserConfirmed`: a user confirming "meeting is Friday" does not pin that
/// claim against being corrected by "meeting moved to Saturday" later. What
/// is pinned forever is *provenance* (`origin` never changes), not the
/// content of a specific observation.
///
/// Supersession direction follows `observed_at`: if `m` was observed
/// *before* the existing item (e.g. history backfilled by hydration), `m`
/// is stored and immediately superseded by the existing, newer item.
///
/// With no fingerprint, `m` is always inserted fresh (there is nothing to
/// compare it against).
pub fn record_tx(tx: &WriteTx<'_>, m: &NewMemory) -> MemoryResult<RecordOutcome> {
    if let Some(fp) = &m.fingerprint {
        if let Some(existing) = repos::memory::find_active_by_fingerprint(tx, fp)? {
            let same_claim = match (&existing.payload, &m.payload) {
                (Some(a), Some(b)) => a == b,
                (None, None) => existing.content.as_ref() == m.content,
                _ => false,
            };
            if same_claim {
                repos::memory::reinforce(tx, existing.id, &m.source_refs, m.confidence)?;
                return Ok(RecordOutcome::Reinforced(existing.id));
            }
            let new_id = repos::memory::insert(tx, m)?;
            // Supersession follows *observation* order, not processing
            // order: backfilled history (older observations arriving after
            // newer ones) is recorded as already superseded by the newer item.
            let new_is_older = match (m.observed_at, existing.observed_at) {
                (Some(new_at), Some(existing_at)) => new_at < existing_at,
                _ => false,
            };
            if new_is_older {
                repos::memory::supersede(tx, new_id, existing.id)?;
                return Ok(RecordOutcome::Superseded {
                    old: new_id,
                    new: existing.id,
                });
            }
            repos::memory::supersede(tx, existing.id, new_id)?;
            return Ok(RecordOutcome::Superseded {
                old: existing.id,
                new: new_id,
            });
        }
    }
    let id = repos::memory::insert(tx, m)?;
    Ok(RecordOutcome::Inserted(id))
}

/// Tally from [`MemoryService::gc`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct GcReport {
    /// Active items whose `expires_at` had passed.
    pub expired: usize,
    /// Active, unpinned, never-retrieved `Candidate` items older than 30
    /// days, treated as noise nobody looked at.
    pub decayed: usize,
}

/// Tally from [`MemoryService::apply_retention`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct RetentionReport {
    pub events_pruned: usize,
    pub agent_runs_pruned: usize,
}

/// The unified-memory service: dedup, supersession, confirmation, expiry/GC,
/// and the wiring between message ingestion and the (pluggable, non-LLM by
/// default) candidate extractor.
///
/// Cheap to clone: `Database` is a shared handle and the extractor is behind
/// an `Arc`.
#[derive(Clone, Debug)]
pub struct MemoryService {
    db: Database,
    extractor: Arc<dyn CandidateExtractor>,
}

impl MemoryService {
    /// Build a service around any [`CandidateExtractor`] (e.g. a future
    /// model-backed one, which must still emit `Origin::AgentDerived`
    /// candidates per that trait's contract).
    pub fn new(db: Database, extractor: Arc<dyn CandidateExtractor>) -> Self {
        Self { db, extractor }
    }

    /// Build a service using the deterministic, dependency-free
    /// [`HeuristicExtractor`] — the default and, today, the only extractor
    /// this workspace ships. No LLM required.
    pub fn with_heuristics(db: Database) -> Self {
        Self::new(db, Arc::new(HeuristicExtractor))
    }

    /// Record one memory in its own transaction. See [`record_tx`] for the
    /// dedup/supersession contract.
    pub fn record(&self, m: NewMemory) -> MemoryResult<RecordOutcome> {
        let committed = self.db.write(|tx| record_tx(tx, &m))?;
        Ok(committed.value)
    }

    /// React to a newly created message: run extraction, record whatever
    /// candidates it finds, keep the entity graph and pending-reply/task
    /// bookkeeping in sync. Everything after the (read-only) extraction step
    /// runs in one transaction.
    ///
    /// Returns `Ok(vec![])` if the message is missing or has been deleted
    /// (nothing to extract from).
    pub fn on_message_created(&self, message_id: MessageId) -> MemoryResult<Vec<RecordOutcome>> {
        let prepared = self.db.read(|r| -> MemoryResult<_> {
            let Some(record) = repos::messages::get(r, message_id)? else {
                return Ok(None);
            };
            if record.deleted {
                return Ok(None);
            }
            let author_name = repos::users::get(r, record.message.author_id)?
                .map(|u| u.user.display_name().to_string())
                .unwrap_or_else(|| record.message.author_id.to_string());
            let me =
                repos::accounts::current(r, DiscordIdentity::UserSocialSdk)?.map(|a| a.user_id);
            Ok(Some((record, author_name, me)))
        })?;

        let Some((record, author_name, me)) = prepared else {
            return Ok(Vec::new());
        };

        let candidates = self.extractor.extract(&ExtractionInput {
            message: &record.message,
            me,
            author_name: &author_name,
        });

        let conv = EntityId::Conversation(record.message.conversation_id);
        let author_entity = EntityId::User(record.message.author_id);
        let from_me = me == Some(record.message.author_id);
        let sent_at = record.message.sent_at;
        let message_origin = record.origin;

        let committed = self.db.write(|tx| -> MemoryResult<Vec<RecordOutcome>> {
            let mut outcomes = Vec::with_capacity(candidates.len());
            for candidate in &candidates {
                outcomes.push(record_tx(tx, candidate)?);
            }

            repos::edges::upsert(
                tx,
                &author_entity,
                RelationType::ParticipatesIn,
                &conv,
                message_origin,
                Confidence::CERTAIN,
            )?;

            if from_me {
                let pending = repos::memory::list(
                    tx,
                    &MemoryFilter {
                        statuses: Some(vec![
                            MemoryStatus::Candidate,
                            MemoryStatus::Derived,
                            MemoryStatus::UserConfirmed,
                        ]),
                        kinds: Some(vec![MemoryKind::PendingReply]),
                        entity: Some(conv),
                        limit: EFFECTIVELY_UNLIMITED,
                        ..Default::default()
                    },
                )?;
                // Only questions asked before this reply are answered by it;
                // backfilled older replies must not expire newer questions.
                for item in pending
                    .into_iter()
                    .filter(|i| i.observed_at.is_none_or(|at| at <= sent_at))
                {
                    repos::memory::set_status(tx, item.id, MemoryStatus::Expired)?;
                }

                if let Some(commitment) =
                    candidates.iter().find(|c| c.kind == MemoryKind::Commitment)
                {
                    create_commitment_task_if_new(
                        tx,
                        commitment,
                        message_id,
                        record.message.conversation_id,
                    )?;
                }
            }

            Ok(outcomes)
        })?;
        Ok(committed.value)
    }

    /// Explicitly confirm a memory. `origin` is never touched (see
    /// [`repos::memory::confirm`]).
    pub fn confirm(&self, id: MemoryId) -> MemoryResult<bool> {
        let committed = self
            .db
            .write(|tx| -> MemoryResult<bool> { Ok(repos::memory::confirm(tx, id)?) })?;
        Ok(committed.value)
    }

    /// Reject a memory (status becomes `Rejected`).
    pub fn reject(&self, id: MemoryId) -> MemoryResult<bool> {
        let committed = self.db.write(|tx| -> MemoryResult<bool> {
            Ok(repos::memory::set_status(tx, id, MemoryStatus::Rejected)?)
        })?;
        Ok(committed.value)
    }

    /// Replace `old` with `new` (see [`repos::memory::supersede`]).
    pub fn supersede(&self, old: MemoryId, new: MemoryId) -> MemoryResult<()> {
        self.db
            .write(|tx| -> MemoryResult<()> { Ok(repos::memory::supersede(tx, old, new)?) })?;
        Ok(())
    }

    /// Pin or unpin a memory; pinned items are exempt from [`Self::gc`]'s
    /// expiry and decay.
    pub fn set_pinned(&self, id: MemoryId, pinned: bool) -> MemoryResult<bool> {
        let committed = self
            .db
            .write(|tx| -> MemoryResult<bool> { Ok(repos::memory::set_pinned(tx, id, pinned)?) })?;
        Ok(committed.value)
    }

    /// The full supersession chain containing `id`, oldest first.
    pub fn history(&self, id: MemoryId) -> MemoryResult<Vec<MemoryItem>> {
        self.db
            .read(|r| -> MemoryResult<_> { Ok(repos::memory::history(r, id)?) })
    }

    /// List memories matching `filter`.
    pub fn list(&self, filter: &MemoryFilter) -> MemoryResult<Vec<MemoryItem>> {
        self.db
            .read(|r| -> MemoryResult<_> { Ok(repos::memory::list(r, filter)?) })
    }

    /// Expire due items and decay stale, unretrieved candidates, in one
    /// transaction.
    ///
    /// Decay: active `Candidate` items older than 30 days (by `created_at`)
    /// with `retrieval_count == 0` and not pinned move to `Expired`. This is
    /// distinct from [`repos::memory::expire_due`] (which only looks at
    /// `expires_at`): decay catches candidates nobody ever looked at *and*
    /// that never got an explicit expiry set by the extractor.
    pub fn gc(&self, now: Timestamp) -> MemoryResult<GcReport> {
        let committed = self.db.write(|tx| -> MemoryResult<GcReport> {
            let expired = repos::memory::expire_due(tx, now)?.len();
            let decayed = decay_stale_candidates(tx, now)?;
            Ok(GcReport { expired, decayed })
        })?;
        Ok(committed.value)
    }

    /// Apply time-based retention to the event log and agent-run history.
    ///
    /// Raw-message retention (`RetentionConfig::raw_messages_days`) is
    /// **not** implemented here: pruning canonical `messages` rows safely
    /// needs a dedicated `repos::messages` prune function (respecting
    /// `deleted`/undeleted semantics and any foreign keys) plus FTS5
    /// external-content cleanup (`messages_fts` triggers assume the base
    /// row still exists), which is out of scope for this batch. Wiring it up
    /// is a small, self-contained follow-up once that repo function exists.
    pub fn apply_retention(
        &self,
        now: Timestamp,
        cfg: &RetentionConfig,
    ) -> MemoryResult<RetentionReport> {
        let committed = self.db.write(|tx| -> MemoryResult<RetentionReport> {
            let events_pruned = repos::events::prune_before(
                tx,
                now.saturating_sub(DurationMs::from_days(cfg.event_log_days as u64)),
            )?;
            let agent_runs_pruned = repos::agent_runs::prune_before(
                tx,
                now.saturating_sub(DurationMs::from_days(cfg.agent_runs_days as u64)),
            )?;
            Ok(RetentionReport {
                events_pruned,
                agent_runs_pruned,
            })
        })?;
        Ok(committed.value)
    }
}

fn decay_stale_candidates(tx: &WriteTx<'_>, now: Timestamp) -> MemoryResult<usize> {
    let cutoff = now.saturating_sub(CANDIDATE_DECAY);
    let candidates = repos::memory::list(
        tx,
        &MemoryFilter {
            statuses: Some(vec![MemoryStatus::Candidate]),
            limit: EFFECTIVELY_UNLIMITED,
            ..Default::default()
        },
    )?;
    let mut decayed = 0usize;
    for item in candidates {
        if item.pinned || item.retrieval_count != 0 || item.created_at >= cutoff {
            continue;
        }
        if repos::memory::set_status(tx, item.id, MemoryStatus::Expired)? {
            decayed += 1;
        }
    }
    Ok(decayed)
}

/// Create a `Candidate` task for a local commitment, unless a task with the
/// same `source` (the triggering message) already exists — so calling
/// [`MemoryService::on_message_created`] twice for the same message never
/// duplicates the task.
fn create_commitment_task_if_new(
    tx: &WriteTx<'_>,
    commitment: &NewMemory,
    message_id: MessageId,
    conversation_id: litecord_types::ConversationId,
) -> MemoryResult<()> {
    let source = SourceRef::new(EntityId::Message(message_id));
    let existing = repos::tasks::list(
        tx,
        &TaskFilter {
            conversation_id: Some(conversation_id),
            limit: EFFECTIVELY_UNLIMITED,
            ..Default::default()
        },
    )?;
    if existing.iter().any(|t| t.source.as_ref() == Some(&source)) {
        return Ok(());
    }

    let (what, due_at) = match &commitment.payload {
        Some(MemoryPayload::Commitment { what, due_at, .. }) => (what.as_str(), *due_at),
        _ => (commitment.content.as_str(), None),
    };
    let title = truncate_chars(what, TASK_TITLE_MAX_CHARS);

    let draft = TaskDraft {
        title,
        description: None,
        priority: TaskPriority::default(),
        due_at,
        related_users: Vec::new(),
        conversation_id: Some(conversation_id),
        parent_id: None,
        source: Some(source),
    };
    repos::tasks::create(tx, &draft, TaskStatus::Candidate, Origin::LocalApplication)?;
    Ok(())
}
