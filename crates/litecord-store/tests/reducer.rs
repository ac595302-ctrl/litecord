//! Integration tests for the canonical reducer (`litecord_store::reducer`),
//! exercised the way the ingest pipeline actually calls it: build a
//! `SourceEnvelope`, hand it to `reducer::apply`, then inspect the
//! `Committed` result and re-read canonical state through the public repo
//! API.

use litecord_core::events::{DiscordEvent, HydrationKey, SourceEnvelope, UnifiedEvent};
use litecord_types::provenance::{DiscordSource, Origin};
use litecord_types::social::{
    Channel, ChannelAccess, ChannelCapabilities, ChannelKind, Conversation, ConversationKind,
    Guild, Message, Presence, PresenceStatus, Relationship, RelationshipKind, User,
};
use litecord_types::trust::AgentVisibility;
use litecord_types::{ConversationId, MessageId, Revision, Timestamp, UserId};

use litecord_store::reducer::{self, Followup, ReducerConfig};
use litecord_store::repos::fts::FtsMode;
use litecord_store::repos::{channels, conversations, guilds, messages, sync_state, users};
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

fn guild(id: u64, name: &str) -> Guild {
    Guild {
        id: litecord_types::GuildId(id),
        name: name.into(),
        icon_url: None,
    }
}

fn channel(id: u64, guild_id: u64, name: &str) -> Channel {
    Channel {
        id: litecord_types::ChannelId(id),
        guild_id: litecord_types::GuildId(guild_id),
        name: name.into(),
        kind: ChannelKind::Text,
        position: 0,
        parent_id: None,
        access: ChannelAccess::Native,
        capabilities: ChannelCapabilities::READABLE,
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
    assert!(!committed
        .events
        .iter()
        .any(|event| matches!(event, UnifiedEvent::MessageImported { .. })));

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

// ---- MessagesPage (paged history backfill) ----

const DAY: i64 = 86_400_000;

fn page(conv: u64, msgs: Vec<Message>) -> DiscordEvent {
    DiscordEvent::MessagesPage {
        conversation_id: ConversationId(conv),
        messages: msgs,
    }
}

/// A history page upserts, never deletes messages it does not contain,
/// emits events only for what actually changed, and moves the cursor back.
#[test]
fn messages_page_upserts_never_deletes_and_advances_cursor() {
    use litecord_store::repos::history_sync;

    let db = Database::open_in_memory().unwrap();
    let cfg = ReducerConfig::default();
    let now = 100 * DAY;
    let recent: Vec<Message> = (100..105)
        .map(|i| msg(i, 10, 20, "recent", now - DAY + i as i64))
        .collect();
    let snapshot = reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            now,
            DiscordEvent::MessagesSnapshot {
                conversation_id: ConversationId(10),
                messages: recent.clone(),
            },
        ),
        &cfg,
    )
    .unwrap();
    assert_eq!(
        snapshot
            .events
            .iter()
            .filter(|event| matches!(event, UnifiedEvent::MessageImported { .. }))
            .count(),
        5
    );
    assert!(!snapshot
        .events
        .iter()
        .any(|event| matches!(event, UnifiedEvent::MessageCreated { .. })));
    db.write(|tx| history_sync::request(tx, ConversationId(10)))
        .unwrap();
    let rec = db
        .read(|r| history_sync::get(r, ConversationId(10)))
        .unwrap()
        .unwrap();
    assert_eq!(rec.oldest_message_id, Some(MessageId(100)));

    // An older page that overlaps one already-known message.
    let mut older: Vec<Message> = (90..100)
        .map(|i| msg(i, 10, 21, "old history", now - 50 * DAY + i as i64))
        .collect();
    older.push(recent[0].clone());
    let committed = reducer::apply(
        &db,
        &envelope(DiscordSource::Synthetic, now, page(10, older)),
        &cfg,
    )
    .unwrap();

    let imported = committed
        .events
        .iter()
        .filter(|e| matches!(e, UnifiedEvent::MessageImported { .. }))
        .count();
    assert_eq!(imported, 10, "only the new messages produce events");
    assert!(!committed
        .events
        .iter()
        .any(|event| matches!(event, UnifiedEvent::MessageCreated { .. })));
    assert!(!committed.events.iter().any(|e| matches!(
        e,
        UnifiedEvent::MessageDeleted { .. } | UnifiedEvent::MessageUpdated { .. }
    )));

    // Nothing outside the page was deleted.
    assert_eq!(
        db.read(|r| messages::count(r, Some(ConversationId(10))))
            .unwrap(),
        15
    );
    for id in 100..105 {
        let rec = db
            .read(|r| messages::get(r, MessageId(id)))
            .unwrap()
            .unwrap();
        assert!(!rec.deleted);
    }

    let rec = db
        .read(|r| history_sync::get(r, ConversationId(10)))
        .unwrap()
        .unwrap();
    assert_eq!(rec.oldest_message_id, Some(MessageId(90)));
    assert_eq!((rec.pages, rec.messages, rec.complete), (1, 11, false));

    // Re-applying the same page stores nothing new but still counts a page.
    let again: Vec<Message> = (90..100)
        .map(|i| msg(i, 10, 21, "old history", now - 50 * DAY + i as i64))
        .collect();
    let committed = reducer::apply(
        &db,
        &envelope(DiscordSource::Synthetic, now, page(10, again)),
        &cfg,
    )
    .unwrap();
    assert!(committed.events.is_empty());
    assert!(committed.value.followups.is_empty());
}

#[test]
fn message_delete_before_history_page_keeps_a_tombstone() {
    let db = Database::open_in_memory().unwrap();
    let cfg = ReducerConfig::default();
    let deleted = reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            100,
            DiscordEvent::MessageDeleted {
                message_id: MessageId(77),
                conversation_id: ConversationId(10),
            },
        ),
        &cfg,
    )
    .unwrap();
    assert!(deleted.events.contains(&UnifiedEvent::MessageDeleted {
        message_id: MessageId(77),
        conversation_id: ConversationId(10),
    }));

    let page = reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            200,
            page(10, vec![msg(77, 10, 20, "late history", 50)]),
        ),
        &cfg,
    )
    .unwrap();
    assert!(!page.events.iter().any(|event| matches!(
        event,
        UnifiedEvent::MessageCreated {
            message_id: MessageId(77),
            ..
        } | UnifiedEvent::MessageImported {
            message_id: MessageId(77),
            ..
        } | UnifiedEvent::MessageUpdated {
            message_id: MessageId(77),
            ..
        }
    )));
    assert!(db
        .read(|r| messages::get(r, MessageId(77)))
        .unwrap()
        .is_none());
    let tombstones: i64 = db
        .read(|r| {
            Ok::<_, litecord_store::StoreError>(r.query_row(
                "SELECT COUNT(*) FROM message_tombstones WHERE message_id = ?1",
                [MessageId(77).to_sql()],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(tombstones, 1);
}

#[test]
fn stale_historical_edit_does_not_overwrite_newer_message() {
    let db = Database::open_in_memory().unwrap();
    let cfg = ReducerConfig::default();
    let mut live = msg(88, 10, 20, "current edit", 50);
    live.edited_at = Some(Timestamp::from_millis(300));
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            400,
            DiscordEvent::MessageUpdated {
                message: live.clone(),
            },
        ),
        &cfg,
    )
    .unwrap();

    let mut stale = msg(88, 10, 20, "older history", 50);
    stale.edited_at = Some(Timestamp::from_millis(200));
    let page = reducer::apply(
        &db,
        &envelope(DiscordSource::Synthetic, 500, page(10, vec![stale])),
        &cfg,
    )
    .unwrap();
    assert!(!page.events.iter().any(|event| matches!(
        event,
        UnifiedEvent::MessageUpdated {
            message_id: MessageId(88),
            ..
        }
    )));
    let record = db
        .read(|r| messages::get(r, MessageId(88)))
        .unwrap()
        .unwrap();
    assert_eq!(record.message.content.as_ref(), "current edit");
    assert_eq!(record.message.edited_at, live.edited_at);
}

#[test]
fn source_snapshots_only_retire_their_own_guilds_and_channels() {
    let db = Database::open_in_memory().unwrap();
    let cfg = ReducerConfig::default();

    reducer::apply(
        &db,
        &envelope(
            DiscordSource::UserSession,
            100,
            DiscordEvent::GuildsSnapshot {
                guilds: vec![guild(10, "shared"), guild(20, "user only")],
            },
        ),
        &cfg,
    )
    .unwrap();
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::UserSession,
            110,
            DiscordEvent::GuildChannelsSnapshot {
                guild_id: litecord_types::GuildId(10),
                channels: vec![channel(101, 10, "one"), channel(102, 10, "two")],
            },
        ),
        &cfg,
    )
    .unwrap();

    // The bot sees only the shared guild and one of its channels. Its
    // snapshots must not retire the user session's additional memberships.
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::BotGateway,
            120,
            DiscordEvent::GuildsSnapshot {
                guilds: vec![guild(10, "shared")],
            },
        ),
        &cfg,
    )
    .unwrap();
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::BotGateway,
            130,
            DiscordEvent::GuildChannelsSnapshot {
                guild_id: litecord_types::GuildId(10),
                channels: vec![channel(101, 10, "one")],
            },
        ),
        &cfg,
    )
    .unwrap();
    assert!(
        !db.read(|r| guilds::get(r, litecord_types::GuildId(20)))
            .unwrap()
            .unwrap()
            .departed
    );
    assert!(
        !db.read(|r| channels::get(r, litecord_types::ChannelId(102)))
            .unwrap()
            .unwrap()
            .removed
    );

    // The user session drops channel 101, but the bot still retains it.
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::UserSession,
            140,
            DiscordEvent::GuildChannelsSnapshot {
                guild_id: litecord_types::GuildId(10),
                channels: vec![channel(102, 10, "two")],
            },
        ),
        &cfg,
    )
    .unwrap();
    assert!(
        !db.read(|r| channels::get(r, litecord_types::ChannelId(101)))
            .unwrap()
            .unwrap()
            .removed
    );

    // A direct departure from the bot removes only its membership. The guild
    // remains active until the user session leaves as well.
    reducer::apply(
        &db,
        &envelope(
            DiscordSource::BotGateway,
            150,
            DiscordEvent::GuildRemoved {
                guild_id: litecord_types::GuildId(10),
            },
        ),
        &cfg,
    )
    .unwrap();
    assert!(
        !db.read(|r| guilds::get(r, litecord_types::GuildId(10)))
            .unwrap()
            .unwrap()
            .departed
    );
    assert!(
        db.read(|r| channels::get(r, litecord_types::ChannelId(101)))
            .unwrap()
            .unwrap()
            .removed
    );

    reducer::apply(
        &db,
        &envelope(
            DiscordSource::UserSession,
            160,
            DiscordEvent::GuildRemoved {
                guild_id: litecord_types::GuildId(10),
            },
        ),
        &cfg,
    )
    .unwrap();
    assert!(
        db.read(|r| guilds::get(r, litecord_types::GuildId(10)))
            .unwrap()
            .unwrap()
            .departed
    );
}

/// The page and its cursor share one transaction: if the transaction fails
/// after reducing, neither the messages nor the cursor move.
#[test]
fn messages_page_cursor_rolls_back_with_the_page() {
    use litecord_store::repos::history_sync;
    use litecord_store::StoreError;

    let db = Database::open_in_memory().unwrap();
    db.write(|tx| history_sync::request(tx, ConversationId(10)))
        .unwrap();
    let before = db
        .read(|r| history_sync::get(r, ConversationId(10)))
        .unwrap()
        .unwrap();

    let env = envelope(
        DiscordSource::Synthetic,
        10 * DAY,
        page(10, vec![msg(5, 10, 20, "lost", DAY)]),
    );
    let result: Result<litecord_store::Committed<()>, StoreError> = db.write(|tx| {
        reducer::reduce(tx, &env, &ReducerConfig::default())?;
        Err(StoreError::Invariant("simulated failure".into()))
    });
    assert!(result.is_err());

    let after = db
        .read(|r| history_sync::get(r, ConversationId(10)))
        .unwrap()
        .unwrap();
    assert_eq!(after, before);
    assert!(db
        .read(|r| messages::get(r, MessageId(5)))
        .unwrap()
        .is_none());
}

/// An empty page means the beginning of history: the row completes and is
/// no longer pending.
#[test]
fn empty_messages_page_marks_history_sync_complete() {
    use litecord_store::repos::history_sync;

    let db = Database::open_in_memory().unwrap();
    db.write(|tx| history_sync::request(tx, ConversationId(10)))
        .unwrap();
    assert!(db
        .read(|r| history_sync::next_pending(r))
        .unwrap()
        .is_some());

    let committed = reducer::apply(
        &db,
        &envelope(DiscordSource::Synthetic, DAY, page(10, Vec::new())),
        &ReducerConfig::default(),
    )
    .unwrap();
    // No unified events, but the cursor row changed: a new revision.
    assert!(!committed.changed());
    assert_eq!(committed.revision, Revision(2));

    let rec = db
        .read(|r| history_sync::get(r, ConversationId(10)))
        .unwrap()
        .unwrap();
    assert!(rec.complete);
    assert_eq!((rec.pages, rec.messages), (1, 0));
    assert!(db
        .read(|r| history_sync::next_pending(r))
        .unwrap()
        .is_none());

    // Without a history_sync row an empty page is a pure no-op.
    let committed = reducer::apply(
        &db,
        &envelope(DiscordSource::Synthetic, DAY, page(11, Vec::new())),
        &ReducerConfig::default(),
    )
    .unwrap();
    assert_eq!(committed.revision, Revision(2));
    assert_eq!(db.current_revision().unwrap(), Revision(2));
}

/// Backfilled history only requests memory extraction for messages sent in
/// the last 7 days (relative to `observed_at`), so old history cannot flood
/// memory with stale pending replies.
#[test]
fn messages_page_extracts_memory_only_for_recent_messages() {
    let db = Database::open_in_memory().unwrap();
    let now = 100 * DAY;
    let list = vec![
        msg(1, 10, 20, "ancient", now - 60 * DAY),
        msg(2, 10, 20, "eight days", now - 8 * DAY),
        msg(3, 10, 20, "six days", now - 6 * DAY),
        msg(4, 10, 20, "today", now - 1_000),
    ];
    let committed = reducer::apply(
        &db,
        &envelope(DiscordSource::Synthetic, now, page(10, list)),
        &ReducerConfig::default(),
    )
    .unwrap();

    let extracted: Vec<MessageId> = committed
        .value
        .followups
        .iter()
        .filter_map(|f| match f {
            Followup::ExtractMemory { message_id, .. } => Some(*message_id),
            _ => None,
        })
        .collect();
    assert_eq!(extracted, vec![MessageId(3), MessageId(4)]);
    // All four are stored and announced as history imports.
    let imported = committed
        .events
        .iter()
        .filter(|e| matches!(e, UnifiedEvent::MessageImported { .. }))
        .count();
    assert_eq!(imported, 4);
    assert!(!committed
        .events
        .iter()
        .any(|event| matches!(event, UnifiedEvent::MessageCreated { .. })));
    assert!(committed
        .value
        .followups
        .contains(&Followup::Hydrate(HydrationKey::DmSummaries)));
}

/// A DM whose recipient has no profile yet keeps a placeholder and asks for
/// the profile, so the conversation can show a name instead of an ID. A known
/// recipient asks for nothing.
#[test]
fn dm_with_unknown_recipient_asks_for_the_profile_once_known_stops() {
    let db = Database::open_in_memory().unwrap();
    let dm = |id: u64, recipient: u64| Conversation {
        id: ConversationId(id),
        kind: ConversationKind::DirectMessage,
        recipient_id: Some(UserId(recipient)),
        guild_id: None,
        lobby_id: None,
        title: None,
        last_message_id: None,
        last_activity_at: None,
    };
    let wants = |f: &[Followup], user: u64| {
        f.contains(&Followup::Hydrate(HydrationKey::User {
            user_id: UserId(user),
        }))
    };
    let apply = |event: DiscordEvent, at: i64| {
        reducer::apply(
            &db,
            &envelope(DiscordSource::Synthetic, at, event),
            &ReducerConfig::default(),
        )
        .unwrap()
        .value
        .followups
    };

    let f = apply(
        DiscordEvent::ConversationUpserted {
            conversation: dm(1, 7),
        },
        1_000,
    );
    assert!(wants(&f, 7));
    assert!(
        db.read(|r| users::get(r, UserId(7)))
            .unwrap()
            .unwrap()
            .is_stub
    );

    // A snapshot that still lacks the profile asks again (e.g. after a
    // failed fetch), capped per snapshot.
    let f = apply(
        DiscordEvent::ConversationsSnapshot {
            conversations: vec![dm(1, 7)],
        },
        2_000,
    );
    assert!(wants(&f, 7));

    apply(
        DiscordEvent::UserUpserted {
            user: User {
                id: UserId(7),
                username: "grace".into(),
                global_name: Some("Grace".into()),
                avatar_url: None,
                is_bot: false,
                is_provisional: false,
            },
        },
        3_000,
    );
    let f = apply(
        DiscordEvent::ConversationUpserted {
            conversation: dm(1, 7),
        },
        4_000,
    );
    assert!(!wants(&f, 7));
    let f = apply(
        DiscordEvent::ConversationsSnapshot {
            conversations: vec![dm(1, 7)],
        },
        5_000,
    );
    assert!(!wants(&f, 7));
}

#[test]
fn relationship_snapshot_without_profiles_asks_for_them() {
    let db = Database::open_in_memory().unwrap();
    let committed = reducer::apply(
        &db,
        &envelope(
            DiscordSource::Synthetic,
            1_000,
            DiscordEvent::RelationshipsSnapshot {
                entries: vec![(
                    Relationship {
                        user_id: UserId(8),
                        discord: RelationshipKind::Friend,
                        game: RelationshipKind::None,
                        since: None,
                    },
                    None,
                )],
            },
        ),
        &ReducerConfig::default(),
    )
    .unwrap();
    assert!(committed
        .value
        .followups
        .contains(&Followup::Hydrate(HydrationKey::User {
            user_id: UserId(8)
        })));
}
