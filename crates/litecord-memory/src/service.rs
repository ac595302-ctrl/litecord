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
use litecord_store::repos::summaries::{NewSummary, SummaryLevel};
use litecord_store::repos::tasks::TaskFilter;
use litecord_store::{repos, Database, WriteTx};
use litecord_types::entity::EntityId;
use litecord_types::memory::{
    MemoryItem, MemoryKind, MemoryPayload, MemoryStatus, NewMemory, RelationType,
};
use litecord_types::provenance::{Confidence, Origin, SourceRef};
use litecord_types::social::Message;
use litecord_types::tasks::{TaskDraft, TaskPriority, TaskStatus};
use litecord_types::{ConversationId, DurationMs, MemoryId, MessageId, Timestamp};

use crate::error::MemoryResult;
use crate::extract::{CandidateExtractor, ExtractionInput, HeuristicExtractor};
use crate::summary::{HeuristicSummarizer, Summarizer, SummaryInput};

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
    /// Raw messages pruned by [`repos::messages::prune_before`]. Always `0`
    /// when `RetentionConfig::raw_messages_days` is `None` (the default:
    /// keep everything the SDK gave us).
    pub messages_pruned: usize,
}

/// Messages read for one [`MemoryService::summarize_conversation`] window.
/// Conversations busier than this within `[since, now]` are summarized from
/// only their first `SUMMARIZE_MESSAGE_CAP` messages in the window — an
/// honest undercount (the digest still describes only what it saw) rather
/// than an unbounded read.
const SUMMARIZE_MESSAGE_CAP: u32 = 500;

/// How far back [`MemoryService::refresh_recent_summaries`] looks for
/// "recently active" conversations, and the width of the weekly window it
/// (re)computes for each one.
const RECENT_ACTIVITY_WINDOW: DurationMs = DurationMs::from_days(7);

/// How many of the most recently active conversations
/// [`MemoryService::refresh_recent_summaries`] considers per call.
const REFRESH_CONVERSATION_LIMIT: u32 = 50;

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
    summarizer: Arc<dyn Summarizer>,
}

impl MemoryService {
    /// Build a service around any [`CandidateExtractor`] (e.g. a future
    /// model-backed one, which must still emit `Origin::AgentDerived`
    /// candidates per that trait's contract). Summarization defaults to
    /// [`HeuristicSummarizer`]; use [`Self::with_summarizer`] to replace it.
    pub fn new(db: Database, extractor: Arc<dyn CandidateExtractor>) -> Self {
        Self {
            db,
            extractor,
            summarizer: Arc::new(HeuristicSummarizer),
        }
    }

    /// Build a service using the deterministic, dependency-free
    /// [`HeuristicExtractor`] — the default and, today, the only extractor
    /// this workspace ships. No LLM required.
    pub fn with_heuristics(db: Database) -> Self {
        Self::new(db, Arc::new(HeuristicExtractor))
    }

    /// Replace the [`Summarizer`] (default: [`HeuristicSummarizer`]). A
    /// future model-backed summarizer must still be paired, by whoever
    /// constructs it, with `Origin::AgentDerived` storage — see
    /// [`Self::summarize_conversation`].
    pub fn with_summarizer(mut self, summarizer: Arc<dyn Summarizer>) -> Self {
        self.summarizer = summarizer;
        self
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
            let me = repos::accounts::current_user(r)?.map(|a| a.user_id);
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

    /// Apply time-based retention to the event log, agent-run history and
    /// (when configured) raw messages, in one transaction.
    ///
    /// Raw-message retention only runs when `cfg.raw_messages_days` is
    /// `Some(d)`: messages with `sent_at` older than `d` days are deleted via
    /// [`repos::messages::prune_before`] with `keep_bookmarked = true` —
    /// bookmarking is an explicit "keep this" signal, so it is never pruned
    /// by an age-based policy. `None` (the default) keeps every message the
    /// SDK gave us and prunes nothing.
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
            let messages_pruned = match cfg.raw_messages_days {
                Some(days) => repos::messages::prune_before(
                    tx,
                    now.saturating_sub(DurationMs::from_days(days as u64)),
                    true,
                )?,
                None => 0,
            };
            Ok(RetentionReport {
                events_pruned,
                agent_runs_pruned,
                messages_pruned,
            })
        })?;
        Ok(committed.value)
    }

    /// Compute and store a heuristic (non-LLM, unless a different
    /// [`Summarizer`] was installed via [`Self::with_summarizer`]) summary of
    /// `conversation_id` over `[since, now]`, at the given [`SummaryLevel`].
    ///
    /// Returns `Ok(None)` — writing nothing — when the conversation does not
    /// exist, or when the configured summarizer declines (e.g.
    /// [`HeuristicSummarizer`] on too few messages). Otherwise the new
    /// summary is inserted with `Origin::LocalApplication`, which
    /// [`repos::summaries::insert`] explicitly supersedes the previous
    /// current summary for `(conversation_id, level)` with, and its id is
    /// returned.
    ///
    /// A model-backed summarizer must be stored under `Origin::AgentDerived`
    /// instead; that choice belongs to the caller wiring one in, since this
    /// method has no way to tell a heuristic summarizer from a model-backed
    /// one.
    pub fn summarize_conversation(
        &self,
        conversation_id: ConversationId,
        level: SummaryLevel,
        since: Timestamp,
        now: Timestamp,
    ) -> MemoryResult<Option<i64>> {
        let prepared = self.db.read(|r| -> MemoryResult<_> {
            let Some(conv) = repos::conversations::get(r, conversation_id)? else {
                return Ok(None);
            };
            let title = conv
                .conversation
                .title
                .map(|t| t.to_string())
                .unwrap_or_default();
            let records = repos::messages::in_range(
                r,
                Some(conversation_id),
                since,
                Some(now),
                SUMMARIZE_MESSAGE_CAP,
            )?;
            let mut named: Vec<(String, Message)> = Vec::with_capacity(records.len());
            for record in records {
                let name = repos::users::get(r, record.message.author_id)?
                    .map(|u| u.user.display_name().to_string())
                    .unwrap_or_else(|| record.message.author_id.to_string());
                named.push((name, record.message));
            }
            Ok(Some((title, named)))
        })?;
        let Some((title, named)) = prepared else {
            return Ok(None);
        };

        let refs: Vec<(String, &Message)> = named.iter().map(|(n, m)| (n.clone(), m)).collect();
        let input = SummaryInput {
            conversation_title: &title,
            messages: &refs,
            period_start: since,
            period_end: now,
        };
        let Some(content) = self.summarizer.summarize(&input) else {
            return Ok(None);
        };
        let from_message_id = named.first().map(|(_, m)| m.id);
        let to_message_id = named.last().map(|(_, m)| m.id);

        let committed = self.db.write(|tx| -> MemoryResult<i64> {
            Ok(repos::summaries::insert(
                tx,
                &NewSummary {
                    conversation_id: Some(conversation_id),
                    level,
                    content,
                    from_message_id,
                    to_message_id,
                    period_start: Some(since),
                    period_end: Some(now),
                    origin: Origin::LocalApplication,
                },
            )?)
        })?;
        Ok(Some(committed.value))
    }

    /// Refresh [`SummaryLevel::Weekly`] summaries for conversations active in
    /// the last 7 days, and return how many were (re)written.
    ///
    /// Considers up to [`REFRESH_CONVERSATION_LIMIT`] of the most recently
    /// active conversations ([`repos::conversations::list_recent`]),
    /// filtered to those whose `last_activity_at` falls within
    /// [`RECENT_ACTIVITY_WINDOW`] of `now`. For each, a weekly summary
    /// covering `[now - 7d, now]` is only (re)computed when there is a
    /// message newer than the existing weekly summary's `period_end` — a
    /// conversation with no new activity since its last weekly summary is
    /// left untouched, so this can be called on a timer without constantly
    /// superseding unchanged summaries.
    pub fn refresh_recent_summaries(&self, now: Timestamp) -> MemoryResult<usize> {
        let window_start = now.saturating_sub(RECENT_ACTIVITY_WINDOW);
        let recent = self.db.read(|r| -> MemoryResult<_> {
            Ok(repos::conversations::list_recent(
                r,
                REFRESH_CONVERSATION_LIMIT,
                0,
            )?)
        })?;

        let mut written = 0usize;
        for conv in recent {
            let Some(last_activity) = conv.conversation.last_activity_at else {
                continue;
            };
            if last_activity < window_start {
                continue;
            }
            let conversation_id = conv.conversation.id;

            let prior_period_end = self.db.read(|r| -> MemoryResult<_> {
                Ok(
                    repos::summaries::latest(r, Some(conversation_id), SummaryLevel::Weekly)?
                        .and_then(|s| s.period_end),
                )
            })?;
            if let Some(prior_end) = prior_period_end {
                let since = prior_end.saturating_add(DurationMs::from_millis(1));
                let has_new = self.db.read(|r| -> MemoryResult<_> {
                    Ok(
                        !repos::messages::in_range(r, Some(conversation_id), since, None, 1)?
                            .is_empty(),
                    )
                })?;
                if !has_new {
                    continue;
                }
            }

            if self
                .summarize_conversation(conversation_id, SummaryLevel::Weekly, window_start, now)?
                .is_some()
            {
                written += 1;
            }
        }
        Ok(written)
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
