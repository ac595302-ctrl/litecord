//! The compiler proper. See the crate docs for the pipeline.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use litecord_core::metrics::Metrics;
use litecord_core::Error;
use litecord_retrieval::{
    DocKind, RetrievalQuery, RetrievedDoc, RetrievedItem, Retriever, ScoringWeights,
    VisibilityPolicy,
};
use litecord_store::repos::messages::MessageRecord;
use litecord_store::repos::{self, Connection};
use litecord_store::Database;
use litecord_types::entity::EntityId;
use litecord_types::ids::*;
use litecord_types::memory::{MemoryItem, MemoryStatus};
use litecord_types::provenance::DiscordIdentity;
use litecord_types::tasks::{ReminderStatus, TaskStatus};
use litecord_types::trust::{AgentVisibility, TrustLevel};
use litecord_types::{DurationMs, Timestamp};

use crate::budget::{estimate_tokens, BudgetTracker};
use crate::intent::analyze;
use crate::pack::*;

#[derive(Debug, Clone)]
pub struct CompilerConfig {
    pub default_visibility: AgentVisibility,
    pub weights: ScoringWeights,
    pub utc_offset_minutes: i32,
    /// Look-back for pending replies and browse queries.
    pub default_lookback: DurationMs,
}

impl Default for CompilerConfig {
    fn default() -> Self {
        Self {
            default_visibility: AgentVisibility::Allowed,
            weights: ScoringWeights::default(),
            utc_offset_minutes: 0,
            default_lookback: DurationMs::from_days(7),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContextCompiler {
    db: Database,
    retriever: Retriever,
    cfg: CompilerConfig,
    metrics: Option<Arc<Metrics>>,
}

/// Why a conversation is in the pack.
struct ConvCandidate {
    id: ConversationId,
    reason: String,
    awaiting_reply: bool,
}

fn user_name(conn: &Connection, id: UserId) -> Result<String, Error> {
    Ok(repos::users::get(conn, id)?
        .filter(|u| !u.is_stub)
        .map(|u| u.user.display_name().to_owned())
        .unwrap_or_else(|| format!("user {id}")))
}

fn message_ctx(conn: &Connection, m: &MessageRecord, score: f32) -> Result<MessageContext, Error> {
    Ok(MessageContext {
        kind: "external_message",
        message_id: m.message.id,
        conversation_id: m.message.conversation_id,
        author: user_name(conn, m.message.author_id)?,
        author_id: m.message.author_id,
        sent_at: m.message.sent_at,
        content: m.message.content.to_string(),
        trust: TrustLevel::ExternalDiscordContent,
        trusted_as_instruction: false,
        origin: m.origin,
        score,
    })
}

fn memory_ctx(m: &MemoryItem, score: f32) -> MemoryContext {
    let trust = if m.status == MemoryStatus::UserConfirmed {
        TrustLevel::UserConfirmedMemory
    } else {
        TrustLevel::for_origin(m.origin)
    };
    MemoryContext {
        memory_id: m.id,
        kind: m.kind,
        status: m.status,
        content: m.content.to_string(),
        origin: m.origin,
        confidence: m.confidence.get(),
        trust,
        trusted_as_instruction: trust.trusted_as_instruction(),
        superseded_by: m.superseded_by,
        score,
    }
}

impl ContextCompiler {
    pub fn new(db: Database, retriever: Retriever, cfg: CompilerConfig) -> Self {
        Self {
            db,
            retriever,
            cfg,
            metrics: None,
        }
    }

    pub fn with_metrics(mut self, m: Arc<Metrics>) -> Self {
        self.metrics = Some(m);
        self
    }

    /// Compile a pack. All reads happen in one snapshot; the pack's
    /// `as_of_revision` is that snapshot's revision.
    pub fn compile(&self, req: &AgentRequest, budget: TokenBudget) -> Result<ContextPack, Error> {
        let _span = tracing::info_span!("context_compile", budget = budget.max_tokens).entered();
        let start = Instant::now();
        let now = self.db.now();
        let mut pack = self.db.read(|r| self.compile_in(r, req, budget, now))?;
        let micros = start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        pack.stats.compile_micros = micros;
        if let Some(m) = &self.metrics {
            m.context_compile.record(start.elapsed());
        }
        Ok(pack)
    }

    fn compile_in(
        &self,
        r: &litecord_store::ReadTx<'_>,
        req: &AgentRequest,
        budget: TokenBudget,
        now: Timestamp,
    ) -> Result<ContextPack, Error> {
        let mut intent = analyze(&req.instruction, now, self.cfg.utc_offset_minutes);
        let vis = VisibilityPolicy::load(r, self.cfg.default_visibility)?;
        let mut excluded = ExclusionStats::default();

        // 1. Identity.
        let me = repos::accounts::current(r, DiscordIdentity::UserSocialSdk)?.map(|a| a.user_id);
        let identity = match me {
            Some(id) => Some(UserContext {
                user_id: id,
                display_name: user_name(r, id)?,
                trust: TrustLevel::ExternalDiscordContent,
            }),
            None => None,
        };

        // 2. Entity resolution.
        let mut entities: Vec<EntityContext> = Vec::new();
        let mut resolved_users: Vec<(UserId, &'static str)> = req
            .focus_users
            .iter()
            .map(|u| (*u, "selected in UI"))
            .collect();
        let mut name_keywords = Vec::new();
        for k in &intent.keywords {
            if k.chars().count() < 3 {
                continue;
            }
            for (u, _) in repos::users::search_by_name(r, k, 3)? {
                let names = [
                    Some(u.user.username.to_lowercase()),
                    u.user.global_name.as_ref().map(|g| g.to_lowercase()),
                ];
                let matches = names
                    .iter()
                    .flatten()
                    .any(|n| n == k || n.starts_with(k.as_str()));
                if matches
                    && Some(u.user.id) != me
                    && !resolved_users.iter().any(|(id, _)| *id == u.user.id)
                {
                    resolved_users.push((u.user.id, "named in request"));
                    name_keywords.push(k.clone());
                }
            }
            for (note, _) in repos::notes::search_notes(r, k, 3)? {
                if note
                    .alias
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(k))
                    && !resolved_users.iter().any(|(id, _)| *id == note.user_id)
                {
                    resolved_users.push((note.user_id, "alias named in request"));
                    name_keywords.push(k.clone());
                }
            }
        }
        intent.keywords.retain(|k| !name_keywords.contains(k));

        let mut conversations: Vec<ConvCandidate> = Vec::new();
        let push_conv = |list: &mut Vec<ConvCandidate>, id, reason: &str, awaiting| {
            if let Some(c) = list.iter_mut().find(|c: &&mut ConvCandidate| c.id == id) {
                c.awaiting_reply |= awaiting;
            } else {
                list.push(ConvCandidate {
                    id,
                    reason: reason.to_owned(),
                    awaiting_reply: awaiting,
                });
            }
        };
        if let Some(c) = req.focus_conversation {
            push_conv(&mut conversations, c, "open in UI", false);
        }
        for (uid, reason) in &resolved_users {
            let Some(u) = repos::users::get(r, *uid)? else {
                continue;
            };
            let note = repos::notes::get_note(r, *uid)?;
            entities.push(EntityContext {
                entity: EntityId::User(*uid),
                display_name: u.user.display_name().to_owned(),
                relationship: repos::relationships::get(r, *uid)?.map(|rel| rel.discord),
                presence: Some(u.presence.status),
                user_note: note
                    .and_then(|n| n.note)
                    .map(|n| n.chars().take(200).collect()),
                origin: u.origin,
                reason: (*reason).to_owned(),
            });
            intent.resolved_entities.push(EntityId::User(*uid));
            if let Some(dm) = repos::conversations::find_dm_by_recipient(r, *uid)? {
                push_conv(
                    &mut conversations,
                    dm.conversation.id,
                    "conversation with named person",
                    false,
                );
            }
        }

        // Pending replies (latest incoming message unanswered).
        let lookback_since = now.saturating_sub(self.cfg.default_lookback);
        let mut pending_last: Vec<MessageRecord> = Vec::new();
        if intent.wants_pending_replies {
            if let Some(me) = me {
                for p in repos::messages::pending_replies(r, me, lookback_since, 10)? {
                    push_conv(
                        &mut conversations,
                        p.conversation_id,
                        "awaiting your reply",
                        true,
                    );
                    pending_last.push(p.last_message);
                }
            }
        }

        // Visibility on conversations.
        let mut conv_ctx = Vec::new();
        let mut content_convs: Vec<ConversationId> = Vec::new();
        for c in &conversations {
            match vis.visibility_of(c.id) {
                AgentVisibility::Hidden => {
                    excluded.hidden_conversations += 1;
                    continue;
                }
                AgentVisibility::MetadataOnly => excluded.metadata_only_conversations += 1,
                AgentVisibility::Allowed => content_convs.push(c.id),
            }
            let Some(rec) = repos::conversations::get(r, c.id)? else {
                continue;
            };
            let title = match (&rec.conversation.title, rec.conversation.recipient_id) {
                (Some(t), _) => t.to_string(),
                (None, Some(u)) => format!("DM with {}", user_name(r, u)?),
                _ => format!("conversation {}", c.id),
            };
            conv_ctx.push(ConversationContext {
                conversation_id: c.id,
                title,
                visibility: vis.visibility_of(c.id),
                last_activity_at: rec.conversation.last_activity_at,
                awaiting_reply: c.awaiting_reply,
                reason: c.reason.clone(),
            });
        }
        // Entities pointing at hidden conversations must not leak them.
        pending_last.retain(|m| vis.allows_content(m.message.conversation_id));

        // 3. Retrieval.
        if intent.since.is_none() && intent.wants_catch_up {
            intent.since = Some(now.saturating_sub(DurationMs::from_hours(24)));
        }
        let mut q = RetrievalQuery::new(intent.keywords.join(" "));
        q.kinds = vec![
            DocKind::Message,
            DocKind::Memory,
            DocKind::Task,
            DocKind::Summary,
        ];
        q.limit_per_kind = budget.max_messages.max(budget.max_memories);
        q.filters.since = intent.since;
        q.filters.until = intent.until;
        q.focus_conversation = req.focus_conversation;
        q.focus_entities = intent
            .resolved_entities
            .iter()
            .copied()
            .chain(content_convs.iter().map(|c| EntityId::Conversation(*c)))
            .collect();
        if intent.keywords.is_empty() && !content_convs.is_empty() {
            q.filters.conversation_ids = Some(content_convs.clone());
        }
        if req.include_history {
            q.filters.memory_statuses = Some(vec![
                MemoryStatus::Candidate,
                MemoryStatus::Derived,
                MemoryStatus::UserConfirmed,
                MemoryStatus::Superseded,
            ]);
        }
        let browse = intent.keywords.is_empty();
        let run_retrieval = !browse || !content_convs.is_empty() || intent.wants_catch_up;
        let items: Vec<RetrievedItem> = if run_retrieval {
            self.retriever.search(r, &q, &vis, now)?
        } else {
            Vec::new()
        };

        // 4. Relevance gate.
        let in_range = |t: Timestamp| {
            intent.since.is_none_or(|s| t >= s) && intent.until.is_none_or(|u| t < u)
        };
        let mut messages: Vec<(MessageRecord, f32)> = Vec::new();
        let mut memories: Vec<(MemoryItem, f32)> = Vec::new();
        let mut task_ids: Vec<TaskId> = Vec::new();
        for it in items {
            let relevant_lex = it.score.lexical > 0.0;
            match it.doc {
                RetrievedDoc::Message(m) => {
                    let in_focus = content_convs.contains(&m.message.conversation_id);
                    let catch_up = intent.wants_catch_up && in_range(m.message.sent_at);
                    if relevant_lex || in_focus || catch_up {
                        messages.push((m, it.total));
                    } else {
                        excluded.below_relevance += 1;
                    }
                }
                RetrievedDoc::Memory(m) => {
                    let linked = it.score.entity > 0.0 || it.score.conversation > 0.0;
                    if relevant_lex || linked || m.pinned {
                        memories.push((m, it.total));
                    } else {
                        excluded.below_relevance += 1;
                    }
                }
                RetrievedDoc::Task(t) => {
                    if relevant_lex || it.score.entity > 0.0 {
                        task_ids.push(t.id);
                    }
                }
                RetrievedDoc::Note(_) | RetrievedDoc::Summary(_) => {}
            }
        }
        for m in pending_last {
            if !messages.iter().any(|(x, _)| x.message.id == m.message.id) {
                messages.push((m, f32::MAX));
            }
        }
        if !req.include_history && !intent.keywords.is_empty() {
            let superseded = repos::memory::search(
                r,
                &repos::memory::MemorySearch {
                    query: &intent.keywords.join(" "),
                    statuses: Some(&[MemoryStatus::Superseded]),
                    kinds: None,
                    entity: None,
                    since: None,
                    mode: repos::fts::FtsMode::Any,
                    limit: 100,
                },
            )?;
            excluded.superseded_memories = superseded.len() as u32;
        }

        // Tasks & reminders.
        let mut tasks = Vec::new();
        let wants_tasks =
            intent.wants_tasks || !intent.resolved_entities.is_empty() || !task_ids.is_empty();
        if wants_tasks {
            for t in repos::tasks::list(
                r,
                &repos::tasks::TaskFilter {
                    statuses: Some(vec![TaskStatus::Open, TaskStatus::Candidate]),
                    limit: 50,
                    ..Default::default()
                },
            )? {
                let related = t
                    .related_users
                    .iter()
                    .any(|u| intent.resolved_entities.contains(&EntityId::User(*u)));
                // Tasks extracted from a conversation carry its text.
                if !t.conversation_id.is_none_or(|c| vis.allows_content(c)) {
                    continue;
                }
                if intent.wants_tasks || related || task_ids.contains(&t.id) {
                    tasks.push(TaskContext {
                        task_id: t.id,
                        title: t.title,
                        status: t.status,
                        due_at: t.due_at,
                        origin: t.origin,
                    });
                }
            }
        }
        let mut reminders = Vec::new();
        if intent.wants_tasks || intent.wants_catch_up {
            for rem in repos::reminders::list(
                r,
                &repos::reminders::ReminderFilter {
                    statuses: Some(vec![ReminderStatus::Pending, ReminderStatus::Fired]),
                    due_before: Some(now.saturating_add(DurationMs::from_days(7))),
                    limit: 20,
                    ..Default::default()
                },
            )? {
                if !rem.conversation_id.is_none_or(|c| vis.allows_content(c)) {
                    continue;
                }
                reminders.push(ReminderContext {
                    reminder_id: rem.id,
                    title: rem.title,
                    due_at: rem.trigger.due_at(),
                });
            }
        }

        // 5. Budget.
        let mut tracker = BudgetTracker::new(budget.max_tokens);
        let mut sections: BTreeMap<&'static str, SectionStat> = BTreeMap::new();
        let mut take = |section: &'static str, text: &str, tracker: &mut BudgetTracker| -> bool {
            let tokens = estimate_tokens(text);
            if tracker.try_take(tokens) {
                let s = sections.entry(section).or_insert_with(|| SectionStat {
                    section: section.to_owned(),
                    ..Default::default()
                });
                s.items += 1;
                s.tokens += tokens;
                true
            } else {
                false
            }
        };
        let identity = identity.filter(|i| take("identity", &i.display_name, &mut tracker));
        let entities: Vec<EntityContext> = entities
            .into_iter()
            .filter(|e| {
                take(
                    "entities",
                    &format!(
                        "{} {}",
                        e.display_name,
                        e.user_note.as_deref().unwrap_or("")
                    ),
                    &mut tracker,
                )
            })
            .collect();
        let conversations: Vec<ConversationContext> = conv_ctx
            .into_iter()
            .take(budget.max_conversations as usize)
            .filter(|c| take("conversations", &c.title, &mut tracker))
            .collect();
        let tasks: Vec<TaskContext> = tasks
            .into_iter()
            .take(budget.max_tasks as usize)
            .filter(|t| take("tasks", &t.title, &mut tracker))
            .collect();
        let reminders: Vec<ReminderContext> = reminders
            .into_iter()
            .filter(|rem| take("reminders", &rem.title, &mut tracker))
            .collect();
        memories.sort_by(|a, b| b.1.total_cmp(&a.1));
        let memories: Vec<MemoryContext> = memories
            .into_iter()
            .take(budget.max_memories as usize)
            .filter(|(m, _)| take("memories", &m.content, &mut tracker))
            .map(|(m, s)| memory_ctx(&m, s))
            .collect();
        messages.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| b.0.message.sent_at.cmp(&a.0.message.sent_at))
        });
        let over_cap = messages.len().saturating_sub(budget.max_messages as usize) as u32;
        let mut selected: Vec<(MessageRecord, f32)> = messages
            .into_iter()
            .take(budget.max_messages as usize)
            .filter(|(m, _)| take("messages", &m.message.content, &mut tracker))
            .collect();
        selected.sort_by_key(|(m, _)| m.message.sent_at);
        let messages = selected
            .iter()
            .map(|(m, s)| message_ctx(r, m, if *s == f32::MAX { 1.0 } else { *s }))
            .collect::<Result<Vec<_>, _>>()?;

        excluded.dropped_for_budget = tracker.dropped() + over_cap;
        let stats = ContextStats {
            budget_tokens: budget.max_tokens,
            used_tokens: tracker.used(),
            included: sections.into_values().collect(),
            excluded,
            compile_micros: 0,
        };
        Ok(ContextPack {
            as_of_revision: r.revision(),
            generated_at: now,
            request: req.instruction.clone(),
            request_trust: TrustLevel::UserInstruction,
            intent,
            identity,
            entities,
            conversations,
            messages,
            memories,
            tasks,
            reminders,
            capabilities: AgentCapabilities::default(),
            stats,
        })
    }
}
