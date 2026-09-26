#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Memory service behaviour against a real (in-memory) store.

use std::sync::Arc;

use litecord_core::clock::{Clock, ManualClock};
use litecord_core::events::{DiscordEvent, SourceEnvelope};
use litecord_memory::{MemoryService, RecordOutcome, ReminderEngine, TaskService};
use litecord_store::reducer::{self, ReducerConfig};
use litecord_store::{repos, Database};
use litecord_types::entity::EntityId;
use litecord_types::ids::*;
use litecord_types::memory::{MemoryKind, MemoryStatus, NewMemory};
use litecord_types::provenance::{DiscordSource, Origin};
use litecord_types::social::*;
use litecord_types::tasks::*;
use litecord_types::{DurationMs, Timestamp};

// Thursday 2026-09-24T15:00:00Z
const NOW: Timestamp = Timestamp(1_790_262_000_000);
const ME: UserId = UserId(1000);
const ADA: UserId = UserId(7);
const CONV: ConversationId = ConversationId(5001);

fn setup() -> (Database, MemoryService, ManualClock) {
    let clock = ManualClock::new(NOW);
    let db = Database::open_in_memory_with(Arc::new(clock.clone()), None).unwrap();
    let user = |id, name: &str| User {
        id,
        username: name.into(),
        global_name: None,
        avatar_url: None,
        is_bot: false,
        is_provisional: false,
    };
    apply(
        &db,
        DiscordEvent::CurrentUser {
            user: user(ME, "you"),
        },
    );
    apply(
        &db,
        DiscordEvent::UserUpserted {
            user: user(ADA, "ada"),
        },
    );
    apply(
        &db,
        DiscordEvent::ConversationUpserted {
            conversation: Conversation {
                id: CONV,
                kind: ConversationKind::DirectMessage,
                recipient_id: Some(ADA),
                guild_id: None,
                lobby_id: None,
                title: None,
                last_message_id: None,
                last_activity_at: None,
            },
        },
    );
    let svc = MemoryService::with_heuristics(db.clone());
    (db, svc, clock)
}

fn apply(db: &Database, event: DiscordEvent) {
    reducer::apply(
        db,
        &SourceEnvelope::new(DiscordSource::Synthetic, NOW, event),
        &ReducerConfig::default(),
    )
    .unwrap();
}

fn message(
    db: &Database,
    svc: &MemoryService,
    id: u64,
    author: UserId,
    at: Timestamp,
    text: &str,
) -> Vec<RecordOutcome> {
    apply(
        db,
        DiscordEvent::MessageCreated {
            message: Message {
                id: MessageId(id),
                conversation_id: CONV,
                author_id: author,
                content: text.into(),
                sent_at: at,
                edited_at: None,
                reply_to: None,
                extras: vec![],
            },
        },
    );
    svc.on_message_created(MessageId(id)).unwrap()
}

fn active(db: &Database, kind: MemoryKind) -> Vec<litecord_types::memory::MemoryItem> {
    db.read(|r| {
        repos::memory::list(
            r,
            &repos::memory::MemoryFilter {
                kinds: Some(vec![kind]),
                statuses: Some(vec![
                    MemoryStatus::Candidate,
                    MemoryStatus::Derived,
                    MemoryStatus::UserConfirmed,
                ]),
                limit: 50,
                ..Default::default()
            },
        )
    })
    .unwrap()
}

#[test]
fn rescheduled_meeting_supersedes_and_keeps_history() {
    let (db, svc, _) = setup();
    message(
        &db,
        &svc,
        1,
        ADA,
        NOW,
        "Meeting Friday to review the project",
    );
    let out = message(
        &db,
        &svc,
        2,
        ADA,
        NOW.saturating_add(DurationMs::from_hours(1)),
        "Update: meeting moved to Saturday",
    );
    let Some(RecordOutcome::Superseded { old, new }) = out
        .iter()
        .find(|o| matches!(o, RecordOutcome::Superseded { .. }))
        .cloned()
    else {
        panic!("expected supersession, got {out:?}");
    };
    let dates = active(&db, MemoryKind::ImportantDate);
    assert_eq!(dates.len(), 1);
    assert!(dates[0].content.contains("saturday"));
    let history = svc.history(new).unwrap();
    assert_eq!(
        history.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![old, new]
    );
    assert_eq!(history[0].status, MemoryStatus::Superseded);
}

#[test]
fn identical_claims_reinforce_instead_of_duplicating() {
    let (_db, svc, _) = setup();
    let mut m = NewMemory::new(
        MemoryKind::Fact,
        "ada works on the prototype",
        Origin::AgentDerived,
    );
    m.fingerprint = Some(litecord_types::memory::MemoryFingerprint::compute(
        MemoryKind::Fact,
        &[EntityId::User(ADA)],
        "prototype",
    ));
    let first = svc.record(m.clone()).unwrap();
    let second = svc.record(m).unwrap();
    let RecordOutcome::Inserted(id) = first else {
        panic!()
    };
    assert_eq!(second, RecordOutcome::Reinforced(id));
}

#[test]
fn my_reply_expires_earlier_questions_only() {
    let (db, svc, _) = setup();
    let t = |h| NOW.saturating_add(DurationMs::from_hours(h));
    message(&db, &svc, 10, ADA, t(1), "Can you review the prototype?");
    assert_eq!(active(&db, MemoryKind::PendingReply).len(), 1);
    message(&db, &svc, 11, ME, t(2), "Yes, tonight.");
    assert!(active(&db, MemoryKind::PendingReply).is_empty());

    // A newer question, then a *backfilled older* reply of mine: the newer
    // question must stay open.
    message(&db, &svc, 12, ADA, t(5), "Did you get a chance to look?");
    message(&db, &svc, 9, ME, t(0), "older message from me");
    let open = active(&db, MemoryKind::PendingReply);
    assert_eq!(open.len(), 1);
    assert!(open[0].content.contains("chance to look"));
}

#[test]
fn backfilled_older_observation_does_not_supersede_newer() {
    let (db, svc, _) = setup();
    let t = |h| NOW.saturating_add(DurationMs::from_hours(h));
    message(&db, &svc, 20, ADA, t(5), "Can you send the zeppelin notes?");
    message(&db, &svc, 19, ADA, t(1), "Did the old build load ok?");
    let open = active(&db, MemoryKind::PendingReply);
    assert_eq!(open.len(), 1);
    assert!(open[0].content.contains("zeppelin"));
}

#[test]
fn my_commitment_creates_one_candidate_task_with_provenance() {
    let (db, svc, _) = setup();
    message(
        &db,
        &svc,
        30,
        ME,
        NOW,
        "Agreed, I'll send the design doc tomorrow.",
    );
    svc.on_message_created(MessageId(30)).unwrap();
    let tasks = TaskService::new(db.clone())
        .open_and_candidates(50)
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].status, TaskStatus::Candidate);
    assert_eq!(tasks[0].origin, Origin::LocalApplication);
    assert_eq!(
        tasks[0].source.as_ref().unwrap().entity,
        EntityId::Message(MessageId(30))
    );
    // Extracted memories are candidates with local-application provenance;
    // confirming keeps the origin.
    let c = &active(&db, MemoryKind::Commitment)[0];
    assert_eq!(
        (c.origin, c.status),
        (Origin::LocalApplication, MemoryStatus::Candidate)
    );
    svc.confirm(c.id).unwrap();
    let c = db.read(|r| repos::memory::get(r, c.id)).unwrap().unwrap();
    assert_eq!(
        (c.origin, c.status),
        (Origin::LocalApplication, MemoryStatus::UserConfirmed)
    );
}

#[test]
fn conditional_reminders_fire_only_without_a_reply() {
    let (db, svc, clock) = setup();
    let engine = ReminderEngine::new(db.clone());
    let make = |title: &str| {
        db.write(|tx| {
            repos::reminders::create(
                tx,
                &ReminderDraft {
                    title: title.into(),
                    note: None,
                    trigger: ReminderTrigger::Conditional {
                        condition: ReminderCondition::NoReplyFrom {
                            user_id: ADA,
                            conversation_id: CONV,
                            since: NOW,
                        },
                        check_at: NOW.saturating_add(DurationMs::from_hours(24)),
                    },
                    conversation_id: Some(CONV),
                    task_id: None,
                    source: None,
                },
                Origin::UserProvided,
            )
        })
        .unwrap()
        .value
    };
    let r1 = make("follow up with ada");
    assert_eq!(
        engine.next_due().unwrap(),
        Some(NOW.saturating_add(DurationMs::from_hours(24)))
    );
    assert!(engine.tick(NOW).unwrap().is_empty(), "not due yet");

    clock.advance(DurationMs::from_hours(25));
    let fired = engine.tick(clock.now()).unwrap();
    assert_eq!(fired.len(), 1, "no reply ⇒ fires");
    assert_eq!(fired[0].reminder_id, r1);

    let r2 = make("second follow up");
    message(
        &db,
        &svc,
        40,
        ADA,
        NOW.saturating_add(DurationMs::from_hours(2)),
        "here you go",
    );
    assert!(
        engine.tick(clock.now()).unwrap().is_empty(),
        "replied ⇒ satisfied"
    );
    let r2 = db.read(|r| repos::reminders::get(r, r2)).unwrap().unwrap();
    assert_eq!(r2.status, ReminderStatus::Satisfied);
}

#[test]
fn gc_expires_due_items_but_keeps_pinned() {
    let (db, svc, _) = setup();
    let mut a = NewMemory::new(MemoryKind::Fact, "short lived", Origin::AgentDerived);
    a.expires_at = Some(NOW);
    let mut b = a.clone();
    b.content = "pinned".into();
    b.pinned = true;
    let RecordOutcome::Inserted(a) = svc.record(a).unwrap() else {
        panic!()
    };
    let RecordOutcome::Inserted(b) = svc.record(b).unwrap() else {
        panic!()
    };
    let report = svc
        .gc(NOW.saturating_add(DurationMs::from_secs(1)))
        .unwrap();
    assert_eq!(report.expired, 1);
    let get = |id| {
        db.read(|r| repos::memory::get(r, id))
            .unwrap()
            .unwrap()
            .status
    };
    assert_eq!(get(a), MemoryStatus::Expired);
    assert_eq!(get(b), MemoryStatus::Candidate);
}

#[test]
fn task_service_priority_subtasks_and_comments() {
    let (db, _svc, _) = setup();
    let tasks = TaskService::new(db.clone());
    let draft = |title: &str| TaskDraft {
        title: title.into(),
        description: None,
        priority: TaskPriority::Normal,
        due_at: None,
        related_users: Vec::new(),
        conversation_id: None,
        parent_id: None,
        source: None,
    };
    let parent = db
        .write(|tx| {
            repos::tasks::create(
                tx,
                &draft("plan launch"),
                TaskStatus::Open,
                Origin::UserProvided,
            )
        })
        .unwrap()
        .value;

    let changed = tasks
        .set_priority(parent, TaskPriority::Urgent, Origin::UserProvided)
        .unwrap();
    assert!(changed);
    let t = db.read(|r| repos::tasks::get(r, parent)).unwrap().unwrap();
    assert_eq!(t.priority, TaskPriority::Urgent);

    let mut sub_draft = draft("write invite list");
    sub_draft.parent_id = Some(parent);
    let sub = db
        .write(|tx| repos::tasks::create(tx, &sub_draft, TaskStatus::Open, Origin::UserProvided))
        .unwrap()
        .value;
    let subs = tasks.subtasks(parent).unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].id, sub);

    let comment_id = tasks
        .add_comment(parent, "kicking this off", Origin::UserProvided)
        .unwrap();
    assert!(comment_id > 0);
    let comments = tasks.comments(parent).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].body, "kicking this off");
    assert_eq!(comments[0].origin, Origin::UserProvided);
}
