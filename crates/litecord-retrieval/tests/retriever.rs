#![allow(clippy::unwrap_used, clippy::expect_used)]
//! End-to-end [`Retriever`] behaviour against a real (in-memory) store: FTS
//! relevance, recency, structured filters, agent-visibility enforcement,
//! browse mode, pinning, semantic re-ranking, superseded-memory handling and
//! determinism.

use std::sync::Arc;

use litecord_core::clock::ManualClock;
use litecord_core::events::{DiscordEvent, SourceEnvelope};
use litecord_retrieval::embedding::HashingEmbedder;
use litecord_retrieval::{
    DocKind, RetrievalFilters, RetrievalQuery, RetrievedDoc, Retriever, ScoringWeights,
    VisibilityPolicy,
};
use litecord_store::reducer::{self, ReducerConfig};
use litecord_store::{repos, Database};
use litecord_types::entity::EntityId;
use litecord_types::ids::*;
use litecord_types::memory::{MemoryStatus, NewMemory};
use litecord_types::notes::Bookmark;
use litecord_types::provenance::{DiscordSource, Origin};
use litecord_types::social::*;
use litecord_types::trust::AgentVisibility;
use litecord_types::{DurationMs, Timestamp};

// Thursday 2026-09-24T15:00:00Z
const NOW: Timestamp = Timestamp(1_790_262_000_000);
const ME: UserId = UserId(1);
const ADA: UserId = UserId(2);
const LINUS: UserId = UserId(3);
const KEN: UserId = UserId(4);
const IKE: UserId = UserId(5);

const A: ConversationId = ConversationId(10); // allowed (default), ada
const B: ConversationId = ConversationId(20); // explicitly metadata_only, linus
const C: ConversationId = ConversationId(30); // explicitly hidden, ken
const D: ConversationId = ConversationId(40); // no explicit setting, ike

fn apply(db: &Database, event: DiscordEvent) {
    reducer::apply(
        db,
        &SourceEnvelope::new(DiscordSource::Synthetic, NOW, event),
        &ReducerConfig::default(),
    )
    .unwrap();
}

fn user(id: UserId, name: &str) -> User {
    User {
        id,
        username: name.into(),
        global_name: None,
        avatar_url: None,
        is_bot: false,
        is_provisional: false,
    }
}

fn conversation(id: ConversationId, recipient: UserId) -> Conversation {
    Conversation {
        id,
        kind: ConversationKind::DirectMessage,
        recipient_id: Some(recipient),
        guild_id: None,
        lobby_id: None,
        title: None,
        last_message_id: None,
        last_activity_at: None,
    }
}

fn msg(id: u64, conv: ConversationId, author: UserId, at: Timestamp, text: &str) -> Message {
    Message {
        id: MessageId(id),
        conversation_id: conv,
        author_id: author,
        content: text.into(),
        sent_at: at,
        edited_at: None,
        reply_to: None,
        extras: vec![],
    }
}

fn send(db: &Database, id: u64, conv: ConversationId, author: UserId, at: Timestamp, text: &str) {
    apply(
        db,
        DiscordEvent::MessageCreated {
            message: msg(id, conv, author, at, text),
        },
    );
}

/// Builds a small, deterministic dataset spanning four conversations:
/// * `A` — no explicit visibility (relies on the configured default).
/// * `B` — explicitly `metadata_only`.
/// * `C` — explicitly `hidden`.
/// * `D` — like `A`, no explicit visibility (used to probe the default
///   independently of `A`'s other test data).
fn setup() -> Database {
    let db = Database::open_in_memory_with(Arc::new(ManualClock::new(NOW)), None).unwrap();
    apply(
        &db,
        DiscordEvent::CurrentUser {
            user: user(ME, "you"),
        },
    );
    for (u, n) in [(ADA, "ada"), (LINUS, "linus"), (KEN, "ken"), (IKE, "ike")] {
        apply(&db, DiscordEvent::UserUpserted { user: user(u, n) });
    }
    for (c, u) in [(A, ADA), (B, LINUS), (C, KEN), (D, IKE)] {
        apply(
            &db,
            DiscordEvent::ConversationUpserted {
                conversation: conversation(c, u),
            },
        );
    }
    db.write(|tx| repos::conversations::set_visibility(tx, B, Some(AgentVisibility::MetadataOnly)))
        .unwrap();
    db.write(|tx| repos::conversations::set_visibility(tx, C, Some(AgentVisibility::Hidden)))
        .unwrap();

    // Conversation A: the bulk of the message fixtures. Messages 1 and 2 are
    // deliberately the same length and share most of their words, differing
    // only in whether "prototype" is present — isolating term-overlap from
    // BM25's document-length normalization for the relevance-ordering test.
    send(
        &db,
        1,
        A,
        ADA,
        NOW.saturating_sub(DurationMs::from_hours(3)),
        "prototype review notes shared",
    );
    send(
        &db,
        2,
        A,
        ADA,
        NOW.saturating_sub(DurationMs::from_hours(3)),
        "team review notes shared",
    );
    send(
        &db,
        3,
        A,
        ADA,
        NOW.saturating_sub(DurationMs::from_hours(3)),
        "status update",
    );
    send(
        &db,
        4,
        A,
        ADA,
        NOW.saturating_sub(DurationMs::from_hours(1)),
        "status update",
    );
    send(
        &db,
        7,
        A,
        ADA,
        NOW.saturating_sub(DurationMs::from_hours(10)),
        "old prototype notes from last month",
    );
    send(
        &db,
        8,
        A,
        ADA,
        NOW.saturating_sub(DurationMs::from_mins(30)),
        "final review complete",
    );

    // Conversation B: metadata_only — must never surface content.
    send(
        &db,
        5,
        B,
        LINUS,
        NOW.saturating_sub(DurationMs::from_hours(2)),
        "prototype review notes",
    );

    // Conversation C: hidden — must never surface content.
    send(
        &db,
        6,
        C,
        KEN,
        NOW.saturating_sub(DurationMs::from_hours(2)),
        "secret prototype plans",
    );

    // Conversation D: no explicit visibility, used for the default-policy test.
    send(
        &db,
        9,
        D,
        IKE,
        NOW.saturating_sub(DurationMs::from_hours(1)),
        "prototype default visibility test",
    );

    db.write(|tx| {
        repos::notes::add_bookmark(
            tx,
            &Bookmark {
                message_id: MessageId(8),
                conversation_id: A,
                note: None,
                created_at: NOW,
            },
            Origin::UserProvided,
        )
    })
    .unwrap();

    // A superseded memory chain, scoped to conversation A.
    let mk_meeting = |text: &str| {
        let mut m = NewMemory::new(
            litecord_types::memory::MemoryKind::ImportantDate,
            text,
            Origin::LocalApplication,
        );
        m.entities = vec![EntityId::Conversation(A)];
        m
    };
    let old = db
        .write(|tx| repos::memory::insert(tx, &mk_meeting("meeting friday")))
        .unwrap()
        .value;
    let new = db
        .write(|tx| repos::memory::insert(tx, &mk_meeting("meeting saturday")))
        .unwrap()
        .value;
    db.write(|tx| repos::memory::supersede(tx, old, new))
        .unwrap();

    // A memory linked only to the hidden conversation.
    let mut hidden_memory = NewMemory::new(
        litecord_types::memory::MemoryKind::Fact,
        "secret prototype plans mentioned by ken",
        Origin::AgentDerived,
    );
    hidden_memory.entities = vec![EntityId::Conversation(C)];
    db.write(|tx| repos::memory::insert(tx, &hidden_memory))
        .unwrap();

    // A summary linked only to the hidden conversation.
    db.write(|tx| {
        repos::summaries::insert(
            tx,
            &repos::summaries::NewSummary {
                conversation_id: Some(C),
                level: repos::summaries::SummaryLevel::Weekly,
                content: "secret weekly digest about prototype plans".into(),
                from_message_id: None,
                to_message_id: None,
                period_start: None,
                period_end: None,
                origin: Origin::LocalApplication,
            },
        )
    })
    .unwrap();

    db
}

fn retriever() -> Retriever {
    Retriever::new(ScoringWeights::default())
}

fn vis(db: &Database, default: AgentVisibility) -> VisibilityPolicy {
    db.read(|r| VisibilityPolicy::load(r, default)).unwrap()
}

fn query(text: &str) -> RetrievalQuery {
    RetrievalQuery::new(text)
}

fn message_ids(items: &[litecord_retrieval::RetrievedItem]) -> Vec<u64> {
    items
        .iter()
        .filter_map(|i| match &i.doc {
            RetrievedDoc::Message(m) => Some(m.message.id.get()),
            _ => None,
        })
        .collect()
}

#[test]
fn relevance_orders_by_term_overlap_at_equal_recency() {
    let db = setup();
    let policy = vis(&db, AgentVisibility::Allowed);
    let mut q = query("prototype review");
    q.kinds = vec![DocKind::Message];
    q.filters = RetrievalFilters {
        conversation_ids: Some(vec![A]),
        ..Default::default()
    };
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    let ids = message_ids(&items);
    // Message 1 matches both "prototype" and "review"; message 2 matches
    // only "review". Both are sent at the same instant, so only lexical
    // overlap can explain message 1 outranking message 2.
    let pos = |id: u64| ids.iter().position(|x| *x == id).expect("present");
    assert!(pos(1) < pos(2), "{ids:?}");
}

#[test]
fn recency_breaks_ties_between_equally_relevant_messages() {
    let db = setup();
    let policy = vis(&db, AgentVisibility::Allowed);
    let mut q = query("status update");
    q.kinds = vec![DocKind::Message];
    q.filters = RetrievalFilters {
        conversation_ids: Some(vec![A]),
        ..Default::default()
    };
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    let ids = message_ids(&items);
    // Messages 3 and 4 have identical content (identical lexical score);
    // message 4 is more recent and must rank first.
    let pos = |id: u64| ids.iter().position(|x| *x == id).expect("present");
    assert!(pos(4) < pos(3), "{ids:?}");
}

#[test]
fn structured_filters_scope_results() {
    let db = setup();
    let policy = vis(&db, AgentVisibility::Allowed);

    // conversation_ids
    let mut q = query("prototype");
    q.kinds = vec![DocKind::Message];
    q.filters.conversation_ids = Some(vec![A]);
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    assert!(message_ids(&items).iter().all(|id| [1, 7].contains(id)));

    // author_id
    let mut q = query("prototype");
    q.kinds = vec![DocKind::Message];
    q.filters.author_id = Some(ADA);
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    assert!(
        !message_ids(&items).contains(&5),
        "linus's message excluded"
    );

    // since/until
    let mut q = query("prototype");
    q.kinds = vec![DocKind::Message];
    q.filters = RetrievalFilters {
        conversation_ids: Some(vec![A]),
        since: Some(NOW.saturating_sub(DurationMs::from_hours(4))),
        ..Default::default()
    };
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    assert!(
        !message_ids(&items).contains(&7),
        "the 10h-old message is excluded by `since`"
    );

    // origins
    let mut q = query("prototype");
    q.kinds = vec![DocKind::Message];
    q.filters = RetrievalFilters {
        conversation_ids: Some(vec![A]),
        origins: Some(vec![Origin::UserProvided]),
        ..Default::default()
    };
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    assert!(items.is_empty(), "all fixture messages are Synthetic");

    // entity
    let mut q = query("prototype");
    q.kinds = vec![DocKind::Message];
    q.filters.entity = Some(EntityId::User(ADA));
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    assert!(!items.is_empty());
    assert!(items.iter().all(|i| match &i.doc {
        RetrievedDoc::Message(m) => m.message.author_id == ADA,
        _ => true,
    }));
}

#[test]
fn hidden_and_metadata_only_exclude_linked_messages_memories_and_summaries() {
    let db = setup();
    let policy = vis(&db, AgentVisibility::Allowed);

    let mut q = query("prototype");
    q.kinds = vec![DocKind::Message];
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    let ids = message_ids(&items);
    assert!(
        !ids.contains(&5),
        "metadata_only conversation B is excluded"
    );
    assert!(!ids.contains(&6), "hidden conversation C is excluded");

    let mut q = query("secret");
    q.kinds = vec![DocKind::Memory];
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    assert!(
        items.is_empty(),
        "the memory linked only to hidden conversation C must not surface: {items:?}"
    );

    let mut q = query("secret");
    q.kinds = vec![DocKind::Summary];
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    assert!(
        items.is_empty(),
        "the summary linked only to hidden conversation C must not surface: {items:?}"
    );
}

#[test]
fn default_visibility_hides_conversations_with_no_explicit_setting() {
    let db = setup();
    // No explicit setting exists for A or D; a Hidden default must hide both,
    // proving the default (not just the explicit lists) is applied.
    let policy = vis(&db, AgentVisibility::Hidden);

    let mut q = query("prototype");
    q.kinds = vec![DocKind::Message];
    q.filters.conversation_ids = Some(vec![D]);
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    assert!(
        items.is_empty(),
        "D has no explicit setting and inherits the Hidden default"
    );

    let allowing = vis(&db, AgentVisibility::Allowed);
    let items = db
        .read(|r| retriever().search(r, &q, &allowing, NOW))
        .unwrap();
    assert!(!items.is_empty(), "an Allowed default surfaces D's content");
}

#[test]
fn browse_mode_with_no_query_text_returns_recent_items() {
    let db = setup();
    let policy = vis(&db, AgentVisibility::Allowed);
    let mut q = query("");
    q.kinds = vec![DocKind::Message];
    q.filters.conversation_ids = Some(vec![A]);
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    assert!(!items.is_empty());
    // Most recently sent message in A (id 8, 30 minutes ago) should sort
    // first in a pure-recency browse.
    assert_eq!(message_ids(&items).first(), Some(&8));
}

#[test]
fn bookmarked_message_gets_full_pinned_score() {
    let db = setup();
    let policy = vis(&db, AgentVisibility::Allowed);
    let mut q = query("review complete");
    q.kinds = vec![DocKind::Message];
    q.filters.conversation_ids = Some(vec![A]);
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    let bookmarked = items
        .iter()
        .find(|i| matches!(&i.doc, RetrievedDoc::Message(m) if m.message.id == MessageId(8)))
        .expect("bookmarked message present");
    assert_eq!(bookmarked.score.pinned, 1.0);
    assert!(items
        .iter()
        .filter(|i| !matches!(&i.doc, RetrievedDoc::Message(m) if m.message.id == MessageId(8)))
        .all(|i| i.score.pinned == 0.0));
}

#[test]
fn hashing_embedder_gives_positive_semantic_score_for_related_text() {
    let db = setup();
    let policy = vis(&db, AgentVisibility::Allowed);
    let retriever = retriever().with_embedder(Arc::new(HashingEmbedder::default()));
    let mut q = query("prototype review");
    q.kinds = vec![DocKind::Message];
    q.filters.conversation_ids = Some(vec![A]);
    let items = db.read(|r| retriever.search(r, &q, &policy, NOW)).unwrap();
    let related = items
        .iter()
        .find(|i| matches!(&i.doc, RetrievedDoc::Message(m) if m.message.id == MessageId(1)))
        .expect("message 1 present");
    assert!(related.score.semantic > 0.0, "{:?}", related.score);
}

#[test]
fn superseded_memories_excluded_by_default_and_included_on_request() {
    let db = setup();
    let policy = vis(&db, AgentVisibility::Allowed);

    let mut q = query("meeting");
    q.kinds = vec![DocKind::Memory];
    let default_items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    assert!(default_items.iter().any(
        |i| matches!(&i.doc, RetrievedDoc::Memory(m) if m.content.as_ref() == "meeting saturday")
    ));
    assert!(!default_items.iter().any(
        |i| matches!(&i.doc, RetrievedDoc::Memory(m) if m.content.as_ref() == "meeting friday")
    ));

    let mut q = query("meeting");
    q.kinds = vec![DocKind::Memory];
    q.filters.memory_statuses = Some(vec![MemoryStatus::Superseded]);
    let superseded_items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    assert!(superseded_items.iter().any(
        |i| matches!(&i.doc, RetrievedDoc::Memory(m) if m.content.as_ref() == "meeting friday")
    ));
    assert!(!superseded_items.iter().any(
        |i| matches!(&i.doc, RetrievedDoc::Memory(m) if m.content.as_ref() == "meeting saturday")
    ));
}

#[test]
fn results_are_deterministic_across_calls() {
    let db = setup();
    let policy = vis(&db, AgentVisibility::Allowed);
    let mut q = query("prototype review status meeting");
    q.filters.conversation_ids = Some(vec![A]);
    let r = retriever();
    let first = db.read(|conn| r.search(conn, &q, &policy, NOW)).unwrap();
    let second = db.read(|conn| r.search(conn, &q, &policy, NOW)).unwrap();
    let fingerprint = |items: &[litecord_retrieval::RetrievedItem]| {
        items
            .iter()
            .map(|i| (i.doc.entity().to_string(), i.total))
            .collect::<Vec<_>>()
    };
    assert_eq!(fingerprint(&first), fingerprint(&second));
}

#[test]
fn unrestricted_policy_sees_hidden_and_metadata_only_content() {
    let db = setup();
    let policy = VisibilityPolicy::unrestricted();

    let mut q = query("prototype");
    q.kinds = vec![DocKind::Message];
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    let ids = message_ids(&items);
    assert!(
        ids.contains(&5),
        "metadata_only content is visible: {ids:?}"
    );
    assert!(ids.contains(&6), "hidden content is visible: {ids:?}");

    let mut q = query("secret");
    q.kinds = vec![DocKind::Memory];
    let items = db
        .read(|r| retriever().search(r, &q, &policy, NOW))
        .unwrap();
    assert!(
        !items.is_empty(),
        "hidden-conversation memory is visible too"
    );
}
