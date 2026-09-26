//! The retriever: candidate generation (FTS or recency browse), filtering,
//! visibility enforcement and explainable scoring.

use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use litecord_core::metrics::Metrics;
use litecord_store::repos::fts::{self, FtsMode};
use litecord_store::repos::messages::{MessageRecord, MessageSearch};
use litecord_store::repos::summaries::Summary;
use litecord_store::repos::{self, Connection};
use litecord_types::entity::EntityId;
use litecord_types::ids::*;
use litecord_types::memory::{MemoryItem, MemoryStatus};
use litecord_types::notes::UserNote;
use litecord_types::provenance::Origin;
use litecord_types::tasks::{Task, TaskStatus};
use litecord_types::{DurationMs, Timestamp};

use crate::embedding::{cosine01, EmbeddingProvider};
use crate::error::RetrievalError;
use crate::score::{lexical_from_bm25, recency, RetrievalScore, ScoringWeights};
use crate::visibility::VisibilityPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DocKind {
    Message,
    Memory,
    Task,
    Note,
    Summary,
}

/// Structured filters. `memory_statuses = None` means active memories only.
#[derive(Debug, Clone, Default)]
pub struct RetrievalFilters {
    pub since: Option<Timestamp>,
    pub until: Option<Timestamp>,
    pub conversation_ids: Option<Vec<ConversationId>>,
    pub guild_id: Option<GuildId>,
    pub author_id: Option<UserId>,
    pub entity: Option<EntityId>,
    pub origins: Option<Vec<Origin>>,
    pub memory_statuses: Option<Vec<MemoryStatus>>,
}

#[derive(Debug, Clone)]
pub struct RetrievalQuery {
    pub text: String,
    pub kinds: Vec<DocKind>,
    pub filters: RetrievalFilters,
    pub limit_per_kind: u32,
    pub focus_conversation: Option<ConversationId>,
    pub focus_entities: Vec<EntityId>,
    pub mode: FtsMode,
}

impl RetrievalQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kinds: vec![
                DocKind::Message,
                DocKind::Memory,
                DocKind::Task,
                DocKind::Note,
                DocKind::Summary,
            ],
            filters: RetrievalFilters::default(),
            limit_per_kind: 20,
            focus_conversation: None,
            focus_entities: Vec::new(),
            mode: FtsMode::Any,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetrievedDoc {
    Message(MessageRecord),
    Memory(MemoryItem),
    Task(Task),
    Note(UserNote),
    Summary(Summary),
}

impl RetrievedDoc {
    pub fn kind(&self) -> DocKind {
        match self {
            RetrievedDoc::Message(_) => DocKind::Message,
            RetrievedDoc::Memory(_) => DocKind::Memory,
            RetrievedDoc::Task(_) => DocKind::Task,
            RetrievedDoc::Note(_) => DocKind::Note,
            RetrievedDoc::Summary(_) => DocKind::Summary,
        }
    }

    pub fn timestamp(&self) -> Timestamp {
        match self {
            RetrievedDoc::Message(m) => m.message.sent_at,
            RetrievedDoc::Memory(m) => m.observed_at.unwrap_or(m.created_at),
            RetrievedDoc::Task(t) => t.created_at,
            RetrievedDoc::Note(n) => n.updated_at,
            RetrievedDoc::Summary(s) => s.period_end.unwrap_or(s.created_at),
        }
    }

    pub fn conversation_id(&self) -> Option<ConversationId> {
        match self {
            RetrievedDoc::Message(m) => Some(m.message.conversation_id),
            RetrievedDoc::Task(t) => t.conversation_id,
            RetrievedDoc::Summary(s) => s.conversation_id,
            RetrievedDoc::Memory(m) => m.entities.iter().find_map(|e| match e {
                EntityId::Conversation(c) => Some(*c),
                _ => None,
            }),
            RetrievedDoc::Note(_) => None,
        }
    }

    pub fn entity(&self) -> EntityId {
        match self {
            RetrievedDoc::Message(m) => EntityId::Message(m.message.id),
            RetrievedDoc::Memory(m) => EntityId::Memory(m.id),
            RetrievedDoc::Task(t) => EntityId::Task(t.id),
            RetrievedDoc::Note(n) => EntityId::User(n.user_id),
            RetrievedDoc::Summary(s) => EntityId::Local(
                litecord_types::entity::LocalEntityKind::Other,
                LocalEntityId(s.id),
            ),
        }
    }

    pub fn text(&self) -> &str {
        match self {
            RetrievedDoc::Message(m) => &m.message.content,
            RetrievedDoc::Memory(m) => &m.content,
            RetrievedDoc::Task(t) => &t.title,
            RetrievedDoc::Note(n) => n.note.as_deref().or(n.alias.as_deref()).unwrap_or(""),
            RetrievedDoc::Summary(s) => &s.content,
        }
    }

    fn origin(&self) -> Origin {
        match self {
            RetrievedDoc::Message(m) => m.origin,
            RetrievedDoc::Memory(m) => m.origin,
            RetrievedDoc::Task(t) => t.origin,
            RetrievedDoc::Note(_) => Origin::UserProvided,
            RetrievedDoc::Summary(s) => s.origin,
        }
    }

    /// Entities this document is about (for entity relevance/filters).
    fn related(&self) -> Vec<EntityId> {
        let mut out = Vec::new();
        match self {
            RetrievedDoc::Message(m) => {
                out.push(EntityId::User(m.message.author_id));
                out.push(EntityId::Conversation(m.message.conversation_id));
            }
            RetrievedDoc::Memory(m) => out.extend(m.entities.iter().copied()),
            RetrievedDoc::Task(t) => {
                out.extend(t.related_users.iter().map(|u| EntityId::User(*u)));
                out.extend(t.conversation_id.map(EntityId::Conversation));
            }
            RetrievedDoc::Note(n) => out.push(EntityId::User(n.user_id)),
            RetrievedDoc::Summary(s) => out.extend(s.conversation_id.map(EntityId::Conversation)),
        }
        out
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RetrievedItem {
    pub doc: RetrievedDoc,
    pub score: RetrievalScore,
    pub total: f32,
}

#[derive(Clone, Default)]
pub struct Retriever {
    weights: ScoringWeights,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    metrics: Option<Arc<Metrics>>,
}

impl std::fmt::Debug for Retriever {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Retriever")
            .field("weights", &self.weights)
            .field("semantic", &self.embedder.is_some())
            .finish()
    }
}

const ACTIVE: [MemoryStatus; 3] = [
    MemoryStatus::Candidate,
    MemoryStatus::Derived,
    MemoryStatus::UserConfirmed,
];

impl Retriever {
    pub fn new(weights: ScoringWeights) -> Self {
        Self {
            weights,
            embedder: None,
            metrics: None,
        }
    }

    pub fn with_embedder(mut self, e: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedder = Some(e);
        self
    }

    pub fn with_metrics(mut self, m: Arc<Metrics>) -> Self {
        self.metrics = Some(m);
        self
    }

    pub fn weights(&self) -> &ScoringWeights {
        &self.weights
    }

    /// Search. Deterministic for a given database state and `now`.
    pub fn search(
        &self,
        conn: &Connection,
        q: &RetrievalQuery,
        vis: &VisibilityPolicy,
        now: Timestamp,
    ) -> Result<Vec<RetrievedItem>, RetrievalError> {
        let _span = tracing::debug_span!("retrieval", kinds = q.kinds.len()).entered();
        let start = Instant::now();
        let has_terms = !fts::terms(&q.text).is_empty();
        let mut candidates: Vec<(RetrievedDoc, f32)> = Vec::new();
        let limit = q.limit_per_kind.max(1);
        let statuses = q
            .filters
            .memory_statuses
            .clone()
            .unwrap_or_else(|| ACTIVE.to_vec());
        let excluded = vis.content_excluded();

        for kind in &q.kinds {
            match kind {
                DocKind::Message => {
                    if has_terms {
                        let hits = repos::messages::search(
                            conn,
                            &MessageSearch {
                                query: &q.text,
                                conversation_ids: q.filters.conversation_ids.as_deref(),
                                exclude_conversation_ids: &excluded,
                                author_id: q.filters.author_id,
                                guild_id: q.filters.guild_id,
                                since: q.filters.since,
                                until: q.filters.until,
                                origins: q.filters.origins.as_deref(),
                                mode: q.mode,
                                limit: limit * 2,
                            },
                        )?;
                        candidates.extend(
                            hits.into_iter().map(|h| {
                                (RetrievedDoc::Message(h.record), lexical_from_bm25(h.bm25))
                            }),
                        );
                    } else {
                        let since = q
                            .filters
                            .since
                            .unwrap_or_else(|| now.saturating_sub(DurationMs::from_days(7)));
                        let convs: Vec<Option<ConversationId>> = match &q.filters.conversation_ids {
                            Some(ids) => ids.iter().copied().map(Some).collect(),
                            None => vec![None],
                        };
                        for c in convs {
                            for m in repos::messages::in_range(
                                conn,
                                c,
                                since,
                                q.filters.until,
                                limit * 2,
                            )? {
                                candidates.push((RetrievedDoc::Message(m), 0.0));
                            }
                        }
                    }
                }
                DocKind::Memory => {
                    if has_terms {
                        let hits = repos::memory::search(
                            conn,
                            &repos::memory::MemorySearch {
                                query: &q.text,
                                statuses: Some(&statuses),
                                kinds: None,
                                entity: q.filters.entity,
                                since: q.filters.since,
                                mode: q.mode,
                                limit: limit * 2,
                            },
                        )?;
                        candidates.extend(
                            hits.into_iter()
                                .map(|(m, b)| (RetrievedDoc::Memory(m), lexical_from_bm25(b))),
                        );
                    } else {
                        let items = repos::memory::list(
                            conn,
                            &repos::memory::MemoryFilter {
                                statuses: Some(statuses.clone()),
                                entity: q.filters.entity,
                                since: q.filters.since,
                                limit: limit * 2,
                                ..Default::default()
                            },
                        )?;
                        candidates
                            .extend(items.into_iter().map(|m| (RetrievedDoc::Memory(m), 0.0)));
                    }
                }
                DocKind::Task => {
                    let open = [TaskStatus::Open, TaskStatus::Candidate];
                    if has_terms {
                        let hits = repos::tasks::search(conn, &q.text, Some(&open), limit * 2)?;
                        candidates.extend(
                            hits.into_iter()
                                .map(|(t, b)| (RetrievedDoc::Task(t), lexical_from_bm25(b))),
                        );
                    } else {
                        let tasks = repos::tasks::list(
                            conn,
                            &repos::tasks::TaskFilter {
                                statuses: Some(open.to_vec()),
                                limit: limit * 2,
                                ..Default::default()
                            },
                        )?;
                        candidates.extend(tasks.into_iter().map(|t| (RetrievedDoc::Task(t), 0.0)));
                    }
                }
                DocKind::Note if has_terms => {
                    let hits = repos::notes::search_notes(conn, &q.text, limit * 2)?;
                    candidates.extend(
                        hits.into_iter()
                            .map(|(n, b)| (RetrievedDoc::Note(n), lexical_from_bm25(b))),
                    );
                }
                DocKind::Summary if has_terms => {
                    let conv = q.filters.conversation_ids.as_ref().and_then(|c| {
                        if c.len() == 1 {
                            c.first().copied()
                        } else {
                            None
                        }
                    });
                    let hits = repos::summaries::search(conn, &q.text, conv, limit * 2)?;
                    candidates.extend(
                        hits.into_iter()
                            .map(|(s, b)| (RetrievedDoc::Summary(s), lexical_from_bm25(b))),
                    );
                }
                DocKind::Note | DocKind::Summary => {}
            }
        }

        // Visibility and filters not expressible in every repository query.
        let f = &q.filters;
        candidates.retain(|(doc, _)| {
            let content_ok = match doc {
                RetrievedDoc::Memory(m) => m.entities.iter().all(|e| match e {
                    EntityId::Conversation(c) => vis.allows_content(*c),
                    _ => true,
                }),
                other => other
                    .conversation_id()
                    .is_none_or(|c| vis.allows_content(c)),
            };
            let ts = doc.timestamp();
            content_ok
                && f.since.is_none_or(|s| ts >= s)
                && f.until.is_none_or(|u| ts < u)
                && f.origins.as_ref().is_none_or(|o| o.contains(&doc.origin()))
                && f.entity.is_none_or(|e| doc.related().contains(&e))
                && match (doc, f.author_id) {
                    (RetrievedDoc::Message(m), Some(a)) => m.message.author_id == a,
                    _ => true,
                }
                && match (&f.conversation_ids, doc.conversation_id()) {
                    (Some(ids), Some(c)) => ids.contains(&c),
                    (Some(_), None) => !matches!(doc, RetrievedDoc::Message(_)),
                    _ => true,
                }
        });

        let query_vec = match (&self.embedder, has_terms) {
            (Some(e), true) => match e.embed(&[q.text.as_str()]) {
                Ok(mut v) => v.pop(),
                Err(err) => {
                    tracing::debug!(error = %err, "query embedding unavailable");
                    None
                }
            },
            _ => None,
        };

        let mut items = Vec::with_capacity(candidates.len());
        for (doc, lexical) in candidates {
            let related = doc.related();
            let pinned = match &doc {
                RetrievedDoc::Memory(m) => m.pinned,
                RetrievedDoc::Message(m) => repos::notes::is_bookmarked(conn, m.message.id)?,
                _ => false,
            };
            let semantic = match (&self.embedder, &query_vec) {
                (Some(e), Some(qv)) => e
                    .embed(&[doc.text()])
                    .ok()
                    .and_then(|mut v| v.pop())
                    .map(|dv| cosine01(qv, &dv))
                    .unwrap_or(0.0),
                _ => 0.0,
            };
            let score = RetrievalScore {
                lexical,
                semantic,
                recency: recency(doc.timestamp(), now, self.weights.recency_half_life),
                entity: if q.focus_entities.iter().any(|e| related.contains(e)) {
                    1.0
                } else {
                    0.0
                },
                conversation: if q.focus_conversation.is_some()
                    && doc.conversation_id() == q.focus_conversation
                {
                    1.0
                } else {
                    0.0
                },
                pinned: if pinned { 1.0 } else { 0.0 },
            };
            let total = score.total(&self.weights);
            items.push(RetrievedItem { doc, score, total });
        }
        items.sort_by(|a, b| {
            b.total
                .total_cmp(&a.total)
                .then_with(|| b.doc.timestamp().cmp(&a.doc.timestamp()))
                .then_with(|| a.doc.entity().to_string().cmp(&b.doc.entity().to_string()))
        });
        items.dedup_by(|a, b| a.doc.entity() == b.doc.entity());
        items.truncate((limit as usize).saturating_mul(q.kinds.len().max(1)));
        if let Some(m) = &self.metrics {
            m.retrieval.record(start.elapsed());
        }
        Ok(items)
    }
}
