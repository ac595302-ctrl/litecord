//! Integration tests for the canonical reducer (`litecord_store::reducer`),
//! exercised the way the ingest pipeline actually calls it: build a
//! `SourceEnvelope`, hand it to `reducer::apply`, then inspect the
//! `Committed` result and re-read canonical state through the public repo
//! API.

use litecord_core::events::{DiscordEvent, HydrationKey, SourceEnvelope, UnifiedEvent};
use litecord_types::provenance::{DiscordSource, Origin};
use litecord_types::social::{
    Conversation, ConversationKind, Message, Presence, PresenceStatus, Relationship,
    RelationshipKind,
};
use litecord_types::trust::AgentVisibility;
use litecord_types::{ConversationId, MessageId, Revision, Timestamp, UserId};

use litecord_store::reducer::{self, Followup, ReducerConfig};
use litecord_store::repos::fts::FtsMode;
use litecord_store::repos::{conversations, messages, sync_state, users};
use litecord_store::repos::{hydration_jobs, messages::MessageSearch};
use litecord_store::Database;

fn msg(id: u64, conv: u64, author: u64, content: &str, sent_at: i64) -> Message {
    Message {
        id: MessageId(id),
        conversation_id: ConversationId(conv),
        author_id: UserId(author),
        content: content.into(),
        sent_at: Timestamp::from_millis(sent_at),
        edited_at: None,
        reply_to: None,
        extras: Vec::new(),
    }
}

fn envelope(source: DiscordSource, at: i64, event: DiscordEvent) -> SourceEnvelope {
    SourceEnvelope::new(source, Timestamp::from_millis(at), event)
}

#[allow(clippy::unwrap_used)]
fn search_hits(db: &Database, query: &str) -> Vec<messages::MessageHit> {
    db.read(|r| {
        messages::search(
            r,
            &MessageSearch {
                query,
                conversation_ids: None,
                exclude_conversation_ids: &[],
                author_id: None,
                guild_id: None,
                since: None,
                until: None,
                origins: None,
                mode: FtsMode::All,
                limit: 10,
            },
        )
    })
    .unwrap()
}

/// Test 1: `MessageCreated` persists the message, advances the revision by one,
/// emits `MessageCreated`, is found via search, and yields an
/// `ExtractMemory` followup plus conversation-stub and author-stub hydrate
/// followups.
#[test]
fn message_created_persists_advances_revision_and_follows_up() {
    let db = Database::open_in_memory().unwrap();
    let env = envelope(
        DiscordSource::Synthetic,
        1_000,
        DiscordEvent::MessageCreated {
            message: msg(1, 10, 20, "hello world", 1_000),
        },
    );

    let committed = reducer::apply(&db, &env, &ReducerConfig::default()).unwrap();

    assert_eq!(committed.revision, Revision(1));
    assert!(committed.changed());
    assert!(committed.events.contains(&UnifiedEvent::MessageCreated {
        message_id: MessageId(1),
        conversation_id: ConversationId(10),
    }));

    let followups = &committed.value.followups;
    assert!(followups.contains(&Followup::ExtractMemory {
        message_id: MessageId(1),
        conversation_id: ConversationId(10),
    }));
    assert!(followups.contains(&Followup::Hydrate(HydrationKey::User {
        user_id: UserId(20)
    })));
    assert!(followups.contains(&Followup::Hydrate(HydrationKey::DmSummaries)));

    let hits = search_hits(&db, "hello");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].record.message.id, MessageId(1));
}

/// Test 2: Re-applying the identical event does not advance the revision.
#[test]
fn reapplying_identical_event_is_a_noop() {
    let db = Database::open_in_memory().unwrap();
    let env = envelope(
        DiscordSource::Synthetic,
        1_000,
        DiscordEvent::MessageCreated {
            message: msg(1, 10, 20, "hello world", 1_000),
        },
    );

    reducer::apply(&db, &env, &ReducerConfig::default()).unwrap();
    let second = reducer::apply(&db, &env, &ReducerConfig::default()).unwrap();

    assert!(!second.changed());
    assert_eq!(second.revision, Revision(1));
    assert_eq!(db.current_revision().unwrap(), Revision(1));
}

/// Test 3: `MessageUpdated` with new content: the old text no longer matches
/// search, the new text does.
#[test]
fn message_updated_changes_what_search_finds() {
    let db = Database::open_in_memory().unwrap();
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            1_000,
            DiscordEvent::MessageCreated {
                message: msg(1, 10, 20, "alpha content", 1_000),
            },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();

    let updated = reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            2_000,
            DiscordEvent::MessageUpdated {
                message: msg(1, 10, 20, "beta content", 1_000),
            },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();
    assert!(updated.changed());
    assert!(updated.events.contains(&UnifiedEvent::MessageUpdated {
        message_id: MessageId(1),
        conversation_id: ConversationId(10),
    }));
    // An update is not a fresh creation, so no ExtractMemory followup.
    assert!(!updated
        .value
        .followups
        .iter()
        .any(|f| matches!(f, Followup::ExtractMemory { .. })));

    assert!(search_hits(&db, "alpha").is_empty());
    assert_eq!(search_hits(&db, "beta").len(), 1);
}

/// Test 4: `MessageDeleted` with purge: content is emptied, the message no longer
/// appears in `recent`, and it drops out of search.
#[test]
fn message_deleted_with_purge_clears_content_and_search() {
    let db = Database::open_in_memory().unwrap();
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            1_000,
            DiscordEvent::MessageCreated {
                message: msg(1, 10, 20, "secret content", 1_000),
            },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();

    let deleted = reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            2_000,
            DiscordEvent::MessageDeleted {
                message_id: MessageId(1),
                conversation_id: ConversationId(10),
            },
        ),
        &ReducerConfig {
            purge_deleted_messages: true,
        },
    )
    .unwrap();
    assert!(deleted.changed());

    let rec = db
        .read(|r| messages::get(r, MessageId(1)))
        .unwrap()
        .unwrap();
    assert!(rec.deleted);
    assert_eq!(rec.message.content.as_ref(), "");

    let recent = db
        .read(|r| messages::recent(r, ConversationId(10), 10, None))
        .unwrap();
    assert!(recent.is_empty());

    assert!(search_hits(&db, "secret").is_empty());
}

/// Test 5: `RelationshipsSnapshot` removes relationships absent from the new
/// snapshot and emits `RelationshipRemoved`.
#[test]
fn relationships_snapshot_removes_missing_entries() {
    let db = Database::open_in_memory().unwrap();
    let first = vec![
        (
            Relationship {
                user_id: UserId(1),
                discord: RelationshipKind::Friend,
                game: RelationshipKind::None,
                since: None,
            },
            None,
        ),
        (
            Relationship {
                user_id: UserId(2),
                discord: RelationshipKind::Friend,
                game: RelationshipKind::None,
                since: None,
            },
            None,
        ),
    ];
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            1_000,
            DiscordEvent::RelationshipsSnapshot { entries: first },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();

    let second = vec![(
        Relationship {
            user_id: UserId(1),
            discord: RelationshipKind::Friend,
            game: RelationshipKind::None,
            since: None,
        },
        None,
    )];
    let committed = reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            2_000,
            DiscordEvent::RelationshipsSnapshot { entries: second },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();

    assert!(committed.changed());
    assert!(committed
        .events
        .contains(&UnifiedEvent::RelationshipRemoved { user_id: UserId(2) }));
    assert!(db
        .read(|r| litecord_store::repos::relationships::get(r, UserId(2)))
        .unwrap()
        .is_none());
    assert!(db
        .read(|r| litecord_store::repos::relationships::get(r, UserId(1)))
        .unwrap()
        .is_some());
}

/// Test 6: `PresenceChanged` for an unknown user creates a stub and returns a
/// `Hydrate(User)` followup.
#[test]
fn presence_changed_for_unknown_user_creates_stub_and_follows_up() {
    let db = Database::open_in_memory().unwrap();
    let committed = reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            1_000,
            DiscordEvent::PresenceChanged {
                user_id: UserId(42),
                presence: Presence {
                    status: PresenceStatus::Online,
                    activity: None,
                },
            },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();

    assert!(committed
        .value
        .followups
        .contains(&Followup::Hydrate(HydrationKey::User {
            user_id: UserId(42)
        })));

    let rec = db.read(|r| users::get(r, UserId(42))).unwrap().unwrap();
    assert!(rec.is_stub);
    assert_eq!(rec.presence.status, PresenceStatus::Online);
}

/// Test 7: Stored `origin` reflects the source: "synthetic" for
/// `DiscordSource::Synthetic`, "discord_social_sdk" for
/// `DiscordSource::SocialSdk`.
#[test]
fn stored_origin_matches_source() {
    let db = Database::open_in_memory().unwrap();
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            1_000,
            DiscordEvent::MessageCreated {
                message: msg(1, 10, 20, "from synthetic", 1_000),
            },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::SocialSdk,
            1_000,
            DiscordEvent::MessageCreated {
                message: msg(2, 11, 21, "from social sdk", 1_000),
            },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();

    let synthetic = db
        .read(|r| messages::get(r, MessageId(1)))
        .unwrap()
        .unwrap();
    assert_eq!(synthetic.origin, Origin::Synthetic);
    assert_eq!(synthetic.origin.as_str(), "synthetic");

    let social = db
        .read(|r| messages::get(r, MessageId(2)))
        .unwrap()
        .unwrap();
    assert_eq!(social.origin, Origin::DiscordSocialSdk);
    assert_eq!(social.origin.as_str(), "discord_social_sdk");
}

/// Test 8: `pending_replies` surfaces a conversation whose latest message is
/// incoming, and excludes one whose latest message is from `me`.
#[test]
fn pending_replies_only_surfaces_incoming_conversations() {
    let db = Database::open_in_memory().unwrap();
    let me = UserId(5);

    // Conversation 1: latest message is from someone else -> pending.
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            1_000,
            DiscordEvent::MessageCreated {
                message: msg(1, 1, 2, "hi from them", 1_000),
            },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();

    // Conversation 2: latest message is from `me` -> not pending.
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            1_000,
            DiscordEvent::MessageCreated {
                message: msg(2, 2, me.get(), "hi from me", 1_000),
            },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();

    let pending = db
        .read(|r| messages::pending_replies(r, me, Timestamp::from_millis(0), 10))
        .unwrap();

    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].conversation_id, ConversationId(1));
    assert_eq!(pending[0].last_message.message.author_id, UserId(2));
}

/// Test 9: `conversations::upsert` never regresses `last_activity_at`, and a
/// visibility set through `set_visibility` survives further observation.
#[test]
fn conversation_upsert_keeps_visibility_and_never_regresses_activity() {
    let db = Database::open_in_memory().unwrap();
    let conv = |last_activity: i64| Conversation {
        id: ConversationId(1),
        kind: ConversationKind::DirectMessage,
        recipient_id: Some(UserId(9)),
        guild_id: None,
        lobby_id: None,
        title: None,
        last_message_id: Some(MessageId(100)),
        last_activity_at: Some(Timestamp::from_millis(last_activity)),
    };

    reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            1_000,
            DiscordEvent::ConversationUpserted {
                conversation: conv(1_000),
            },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();

    db.write(|tx| {
        conversations::set_visibility(tx, ConversationId(1), Some(AgentVisibility::Hidden))
    })
    .unwrap();

    // An "older" observation must not regress last_activity_at.
    let stale = reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            2_000,
            DiscordEvent::ConversationUpserted {
                conversation: conv(500),
            },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();
    assert!(!stale.changed());

    let rec = db
        .read(|r| conversations::get(r, ConversationId(1)))
        .unwrap()
        .unwrap();
    assert_eq!(
        rec.conversation.last_activity_at,
        Some(Timestamp::from_millis(1_000))
    );
    assert_eq!(rec.visibility, Some(AgentVisibility::Hidden));
}

/// Test 10: `sync_state` mark_dirty/mark_fresh/record_failure round-trip;
/// `hydration_jobs` round-trip.
#[test]
fn sync_state_and_hydration_jobs_round_trip() {
    let db = Database::open_in_memory().unwrap();
    let key = HydrationKey::Guilds;

    db.write(|tx| sync_state::mark_dirty(tx, &key, litecord_types::DurationMs::from_secs(60)))
        .unwrap();
    let rec = db.read(|r| sync_state::get(r, &key)).unwrap().unwrap();
    assert!(rec.dirty);

    db.write(|tx| {
        sync_state::mark_fresh(
            tx,
            &key,
            Timestamp::from_millis(42),
            litecord_types::DurationMs::from_secs(120),
        )
    })
    .unwrap();
    let rec = db.read(|r| sync_state::get(r, &key)).unwrap().unwrap();
    assert!(!rec.dirty);
    assert_eq!(rec.observed_at, Some(Timestamp::from_millis(42)));

    db.write(|tx| {
        sync_state::record_failure(
            tx,
            &key,
            "network down",
            litecord_types::DurationMs::from_secs(60),
        )
    })
    .unwrap();
    let rec = db.read(|r| sync_state::get(r, &key)).unwrap().unwrap();
    assert!(rec.dirty);
    assert_eq!(rec.failure_count, 1);
    assert_eq!(rec.last_error.as_deref(), Some("network down"));

    let job = hydration_jobs::PersistedJob {
        key,
        priority: 1,
        reason: "test".into(),
        requested_at: Timestamp::from_millis(1),
        attempts: 0,
        next_attempt_at: Timestamp::from_millis(1),
    };
    db.write(|tx| hydration_jobs::upsert(tx, &job)).unwrap();
    let jobs = db.read(|r| hydration_jobs::list(r)).unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].key, key);

    let removed = db.write(|tx| hydration_jobs::remove(tx, &key)).unwrap();
    assert!(removed.value);
    assert!(db.read(|r| hydration_jobs::list(r)).unwrap().is_empty());
}
