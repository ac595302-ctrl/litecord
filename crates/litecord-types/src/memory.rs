//! Unified-memory items and the entity graph.
//!
//! Layers (V2 §3):
//!
//! 1. **Source data** / 2. **canonical state** live in the normalized social
//!    tables (users, messages, ...). They never contain speculation.
//! 3. **Derived memory** and 4. **operational memory** are [`MemoryItem`]s.
//!
//! Supersession is explicit: an item is never overwritten by a contradicting
//! one; instead the old item moves to [`MemoryStatus::Superseded`] and points at
//! its replacement via `superseded_by`. History stays queryable.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::entity::EntityId;
use crate::ids::*;
use crate::provenance::{Confidence, Origin, SourceRef};
use crate::{Revision, Timestamp};

str_enum! {
    pub enum MemoryKind {
        /// A salient observation worth remembering ("User A shipped v2").
        Observation => "observation",
        /// Conversation or relationship summary.
        Summary => "summary",
        /// Someone committed to do something.
        Commitment => "commitment",
        /// An incoming message that likely needs a reply.
        PendingReply => "pending_reply",
        /// A date/time that matters ("meeting Friday").
        ImportantDate => "important_date",
        /// A user preference.
        Preference => "preference",
        /// A durable fact about an entity.
        Fact => "fact",
        /// Local note attached to an entity.
        Note => "note",
        /// Operational: what an agent was doing, scheduled checks, ...
        Operational => "operational",
    }
}

/// Which V2 memory layer a kind belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLayer {
    Derived,
    Operational,
}

impl MemoryKind {
    pub const fn layer(self) -> MemoryLayer {
        match self {
            MemoryKind::Operational => MemoryLayer::Operational,
            _ => MemoryLayer::Derived,
        }
    }
}

str_enum! {
    pub enum MemoryStatus {
        /// Proposed by extraction; not yet trusted.
        Candidate => "candidate",
        /// Accepted derived knowledge (still not a Discord fact).
        Derived => "derived",
        /// Explicitly confirmed by the user.
        UserConfirmed => "user_confirmed",
        /// Replaced by a newer item (`superseded_by`).
        Superseded => "superseded",
        /// Past `expires_at` or garbage-collected.
        Expired => "expired",
        /// Rejected by the user.
        Rejected => "rejected",
    }
}

impl MemoryStatus {
    /// Statuses that should be served to agents by default.
    pub const fn is_active(self) -> bool {
        matches!(
            self,
            MemoryStatus::Candidate | MemoryStatus::Derived | MemoryStatus::UserConfirmed
        )
    }
}

/// Optional structured payload. Kept as a small closed set instead of an
/// arbitrary JSON blob so consumers can pattern-match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryPayload {
    Commitment {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<UserId>,
        what: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_at: Option<Timestamp>,
    },
    Date {
        at: Timestamp,
        label: String,
    },
    PendingReply {
        from: UserId,
        conversation_id: ConversationId,
        message_id: MessageId,
    },
    Attributes {
        values: BTreeMap<String, String>,
    },
}

/// Stable semantic identity used for deduplication (V2 §43). Two memories
/// with the same fingerprint describe the same claim.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MemoryFingerprint(pub String);

impl MemoryFingerprint {
    /// `predicate|sorted entities|temporal scope`. Deterministic and readable.
    pub fn compute(kind: MemoryKind, entities: &[EntityId], temporal_scope: &str) -> Self {
        let mut ents: Vec<String> = entities.iter().map(ToString::to_string).collect();
        ents.sort();
        ents.dedup();
        MemoryFingerprint(format!(
            "{}|{}|{}",
            kind.as_str(),
            ents.join(","),
            temporal_scope.trim().to_lowercase()
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: MemoryId,
    pub kind: MemoryKind,
    pub content: Arc<str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<MemoryPayload>,
    pub origin: Origin,
    pub status: MemoryStatus,
    pub confidence: Confidence,
    #[serde(default)]
    pub source_refs: Vec<SourceRef>,
    #[serde(default)]
    pub entities: Vec<EntityId>,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
    pub revision: Revision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<MemoryId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<MemoryFingerprint>,
    /// `[0,1]`, used for ranking and garbage collection.
    pub importance: f32,
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_retrieved_at: Option<Timestamp>,
    #[serde(default)]
    pub retrieval_count: u32,
}

/// Input for creating a memory item. The store assigns id, revision and
/// `created_at`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewMemory {
    pub kind: MemoryKind,
    pub content: String,
    #[serde(default)]
    pub payload: Option<MemoryPayload>,
    pub origin: Origin,
    pub status: MemoryStatus,
    pub confidence: Confidence,
    #[serde(default)]
    pub source_refs: Vec<SourceRef>,
    #[serde(default)]
    pub entities: Vec<EntityId>,
    #[serde(default)]
    pub observed_at: Option<Timestamp>,
    #[serde(default)]
    pub expires_at: Option<Timestamp>,
    #[serde(default)]
    pub fingerprint: Option<MemoryFingerprint>,
    #[serde(default = "default_importance")]
    pub importance: f32,
    #[serde(default)]
    pub pinned: bool,
}

fn default_importance() -> f32 {
    0.5
}

impl NewMemory {
    /// Convenience constructor with sensible defaults.
    pub fn new(kind: MemoryKind, content: impl Into<String>, origin: Origin) -> Self {
        Self {
            kind,
            content: content.into(),
            payload: None,
            origin,
            status: if origin == Origin::UserProvided {
                MemoryStatus::UserConfirmed
            } else {
                MemoryStatus::Candidate
            },
            confidence: Confidence::default(),
            source_refs: Vec::new(),
            entities: Vec::new(),
            observed_at: None,
            expires_at: None,
            fingerprint: None,
            importance: default_importance(),
            pinned: false,
        }
    }
}

str_enum! {
    pub enum RelationType {
        MemberOf => "member_of",
        FriendOf => "friend_of",
        Sent => "sent",
        Mentioned => "mentioned",
        ParticipatesIn => "participates_in",
        Discusses => "discusses",
        AssignedTo => "assigned_to",
        RelatedTo => "related_to",
        LinkedTo => "linked_to",
        Contains => "contains",
    }
}

/// A directed, provenance-carrying edge in the entity graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub from: EntityId,
    pub relation: RelationType,
    pub to: EntityId,
    pub origin: Origin,
    pub confidence: Confidence,
    pub updated_at: Timestamp,
    pub revision: Revision,
}

/// A graph-only local entity (topic, project, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalEntity {
    pub id: EntityId,
    pub name: Arc<str>,
    pub origin: Origin,
    pub created_at: Timestamp,
}

/// Hot/warm/cold classification (V2 §13). Only HOT data is materialized in
/// RAM; WARM and COLD are served from SQLite on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTier {
    Hot,
    Warm,
    Cold,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_order_independent() {
        let a = MemoryFingerprint::compute(
            MemoryKind::Commitment,
            &[EntityId::User(UserId(2)), EntityId::User(UserId(1))],
            "2026-W39",
        );
        let b = MemoryFingerprint::compute(
            MemoryKind::Commitment,
            &[EntityId::User(UserId(1)), EntityId::User(UserId(2))],
            "2026-w39 ",
        );
        assert_eq!(a, b);
    }

    #[test]
    fn user_provided_memories_start_confirmed() {
        let m = NewMemory::new(MemoryKind::Preference, "likes tea", Origin::UserProvided);
        assert_eq!(m.status, MemoryStatus::UserConfirmed);
        let d = NewMemory::new(MemoryKind::Fact, "maybe", Origin::AgentDerived);
        assert_eq!(d.status, MemoryStatus::Candidate);
    }
}
