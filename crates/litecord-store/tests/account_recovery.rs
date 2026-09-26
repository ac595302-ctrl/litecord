//! Account REST catch-up checkpoints commit atomically with imported messages
//! and are independent from live events and ordinary recent-history snapshots.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use litecord_core::events::{DiscordEvent, SourceEnvelope, UnifiedEvent};
use litecord_store::reducer::{self, ReducerConfig};
use litecord_store::repos::messages;
use litecord_store::{Database, StoreResult};
use litecord_types::provenance::{DiscordSource, Origin};
use litecord_types::social::Message;
use litecord_types::{ConversationId, MessageId, Timestamp, UserId};
use rusqlite::OptionalExtension;

fn message(id: u64, conversation_id: u64, author_id: u64, content: &str) -> Message {
    Message {
        id: MessageId(id),
        conversation_id: ConversationId(conversation_id),
        author_id: UserId(author_id),
        content: content.into(),
        sent_at: Timestamp::from_millis((id * 1_000) as i64),
        edited_at: None,
        reply_to: None,
        extras: Vec::new(),
    }
}

fn envelope(source: DiscordSource, at: i64, event: DiscordEvent) -> SourceEnvelope {
    SourceEnvelope::new(source, Timestamp::from_millis(at), event)
}

fn apply(
    db: &Database,
    source: DiscordSource,
    at: i64,
    event: DiscordEvent,
) -> litecord_store::Committed<reducer::ReduceOutcome> {
    reducer::apply(db, &envelope(source, at, event), &ReducerConfig::default()).unwrap()
}

/// Seed the tracked account conversation through an ordinary recent-history
/// snapshot. That snapshot establishes a baseline but is not a verified
/// forward catch-up page.
fn seed_cursor(db: &Database, conversation_id: u64, newest: u64) {
    apply(
        db,
        DiscordSource::UserSession,
        10_000,
        DiscordEvent::MessagesSnapshot {
            conversation_id: ConversationId(conversation_id),
            messages: vec![message(newest, conversation_id, 7, "baseline")],
        },
    );
}

fn newest_cursor(db: &Database, conversation_id: ConversationId) -> Option<Option<MessageId>> {
    db.read(|conn| -> StoreResult<_> {
        let newest: Option<Option<i64>> = conn
            .query_row(
                "SELECT newest_message_id FROM account_catchup WHERE conversation_id = ?1",
                [conversation_id.to_sql()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(newest.map(|id| id.map(MessageId::from_sql)))
    })
    .unwrap()
}

#[test]
fn catchup_messages_and_verified_cursor_commit_together_as_imported_history() {
    let db = Database::open_in_memory().unwrap();
    seed_cursor(&db, 20, 100);

    let committed = apply(
        &db,
        DiscordSource::UserSession,
        20_000,
        DiscordEvent::MessagesCatchupPage {
            conversation_id: ConversationId(20),
            messages: vec![
                message(101, 20, 7, "caught up one"),
                message(105, 20, 8, "caught up two"),
            ],
            has_more: false,
        },
    );

    assert_eq!(
        newest_cursor(&db, ConversationId(20)),
        Some(Some(MessageId(105)))
    );
    db.read(|conn| -> StoreResult<()> {
        assert!(messages::get(conn, MessageId(101))?.is_some());
        assert!(messages::get(conn, MessageId(105))?.is_some());
        Ok(())
    })
    .unwrap();
    assert!(committed.events.contains(&UnifiedEvent::MessageImported {
        message_id: MessageId(101),
        conversation_id: ConversationId(20),
    }));
    assert!(committed.events.contains(&UnifiedEvent::MessageImported {
        message_id: MessageId(105),
        conversation_id: ConversationId(20),
    }));
    assert!(!committed
        .events
        .iter()
        .any(|event| matches!(event, UnifiedEvent::MessageCreated { .. })));
}

#[test]
fn invalid_second_catchup_message_rolls_back_first_row_and_cursor_advance() {
    let db = Database::open_in_memory().unwrap();
    seed_cursor(&db, 30, 200);
    let revision_before = db.current_revision().unwrap();

    let result = reducer::apply(
        &db,
        &envelope(
            DiscordSource::UserSession,
            30_000,
            DiscordEvent::MessagesCatchupPage {
                conversation_id: ConversationId(30),
                messages: vec![
                    message(201, 30, 7, "would be first"),
                    message(202, 31, 8, "wrong conversation"),
                ],
                has_more: false,
            },
        ),
        &ReducerConfig::default(),
    );

    assert!(
        result.is_err(),
        "a page containing another conversation must fail"
    );
    assert_eq!(db.current_revision().unwrap(), revision_before);
    assert_eq!(
        newest_cursor(&db, ConversationId(30)),
        Some(Some(MessageId(200)))
    );
    assert_eq!(db.read(|conn| messages::count(conn, None)).unwrap(), 1);
    assert!(db
        .read(|conn| messages::get(conn, MessageId(201)))
        .unwrap()
        .is_none());
}

#[test]
fn live_message_with_a_higher_id_does_not_advance_verified_cursor() {
    let db = Database::open_in_memory().unwrap();
    seed_cursor(&db, 40, 300);

    let live = apply(
        &db,
        DiscordSource::UserSession,
        40_000,
        DiscordEvent::MessageCreated {
            message: message(999, 40, 9, "live gateway event"),
        },
    );

    assert!(live.events.contains(&UnifiedEvent::MessageCreated {
        message_id: MessageId(999),
        conversation_id: ConversationId(40),
    }));
    assert_eq!(
        newest_cursor(&db, ConversationId(40)),
        Some(Some(MessageId(300)))
    );
    assert!(db
        .read(|conn| messages::get(conn, MessageId(999)))
        .unwrap()
        .is_some());
}

#[test]
fn later_recent_snapshots_do_not_jump_verified_cursor() {
    let db = Database::open_in_memory().unwrap();
    seed_cursor(&db, 50, 400);

    apply(
        &db,
        DiscordSource::UserSession,
        50_000,
        DiscordEvent::MessagesSnapshot {
            conversation_id: ConversationId(50),
            messages: vec![
                message(700, 50, 7, "recent one"),
                message(750, 50, 8, "recent two"),
            ],
        },
    );

    assert_eq!(
        newest_cursor(&db, ConversationId(50)),
        Some(Some(MessageId(400)))
    );
    assert!(db
        .read(|conn| messages::get(conn, MessageId(750)))
        .unwrap()
        .is_some());
}

#[test]
fn bot_and_user_session_observations_share_one_canonical_message() {
    let db = Database::open_in_memory().unwrap();

    apply(
        &db,
        DiscordSource::BotGateway,
        60_000,
        DiscordEvent::MessageCreated {
            message: message(800, 60, 9, "seen by both sources"),
        },
    );
    apply(
        &db,
        DiscordSource::UserSession,
        61_000,
        DiscordEvent::MessagesCatchupPage {
            conversation_id: ConversationId(60),
            messages: vec![message(800, 60, 9, "seen by both sources")],
            has_more: false,
        },
    );

    db.read(|conn| -> StoreResult<()> {
        assert_eq!(messages::count(conn, Some(ConversationId(60)))?, 1);
        let canonical = messages::get(conn, MessageId(800))?.unwrap();
        assert_eq!(canonical.origin, Origin::DiscordBotGateway);

        let observations = messages::observations(conn, MessageId(800))?;
        assert_eq!(observations.len(), 2);
        assert!(observations
            .iter()
            .any(|observation| observation.origin == Origin::DiscordBotGateway));
        assert!(observations
            .iter()
            .any(|observation| observation.origin == Origin::DiscordUserSession));
        Ok(())
    })
    .unwrap();
    assert_eq!(
        newest_cursor(&db, ConversationId(60)),
        Some(Some(MessageId(800)))
    );
    let user_origin_rows: i64 = db
        .read(|conn| -> StoreResult<_> {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM message_observations WHERE message_id = ?1 AND origin = ?2",
                (MessageId(800).to_sql(), Origin::DiscordUserSession.as_str()),
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(user_origin_rows, 1);
}
