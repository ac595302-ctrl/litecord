//! Black-box integration tests for the memory, operational and action
//! repositories, exercised only through `litecord-store`'s public API against
//! `Database::open_in_memory()`.

#![allow(clippy::unwrap_used)]

use litecord_core::events::UnifiedEvent;
use litecord_types::actions::{
    ActionAuditEntry, ActionStatus, Actor, AgentAction, AuditEvent, CapabilityClass, MessageTarget,
};
use litecord_types::entity::{EntityId, LocalEntityKind};
use litecord_types::memory::{
    MemoryFingerprint, MemoryKind, MemoryStatus, NewMemory, RelationType,
};
use litecord_types::notes::{Bookmark, UserNote};
use litecord_types::provenance::DiscordIdentity;
use litecord_types::provenance::{Confidence, Origin, SourceRef};
use litecord_types::tasks::{
    Draft, DraftStatus, ReminderCondition, ReminderDraft, ReminderStatus, ReminderTrigger,
    TaskDraft, TaskStatus,
};
use litecord_types::{ConversationId, MessageId, Revision, Timestamp, UserId};

use litecord_store::repos::fts::FtsMode;
use litecord_store::repos::{
    actions, agent_runs, drafts, edges, embeddings, local_entities, memory, notes, reminders,
    settings, tasks,
};
use litecord_store::{Database, StoreError, WriteTx};

fn stub_conversation(tx: &WriteTx<'_>, id: ConversationId) {
    tx.execute(
        "INSERT INTO conversations (id, kind, origin, observed_at, revision) VALUES (?, 'dm', 'synthetic', 0, 0)",
        rusqlite::params![id.to_sql()],
    )
    .unwrap();
}

// 1. Memory provenance: an agent-derived memory cannot be inserted already
// `UserConfirmed`; `confirm()` moves it there while `origin` never changes.
#[test]
fn memory_provenance_rules() {
    let db = Database::open_in_memory().unwrap();

    let mut bad = NewMemory::new(MemoryKind::Fact, "the sky is green", Origin::AgentDerived);
    bad.status = MemoryStatus::UserConfirmed;
    let err = db.write(|tx| memory::insert(tx, &bad)).unwrap_err();
    assert!(matches!(err, StoreError::Invariant(_)));

    let good = NewMemory::new(MemoryKind::Fact, "the sky is blue", Origin::AgentDerived);
    let id = db.write(|tx| memory::insert(tx, &good)).unwrap().value;
    let item = db.read(|r| memory::get(r, id)).unwrap().unwrap();
    assert_eq!(item.status, MemoryStatus::Candidate);
    assert_eq!(item.origin, Origin::AgentDerived);

    let committed = db.write(|tx| memory::confirm(tx, id)).unwrap();
    assert!(committed.changed());
    let item = db.read(|r| memory::get(r, id)).unwrap().unwrap();
    assert_eq!(item.status, MemoryStatus::UserConfirmed);
    assert_eq!(item.origin, Origin::AgentDerived, "provenance is permanent");
}

// 2. Supersession: two candidate memories, supersede old->new, history()
// returns both oldest-first, active listing excludes the old one, and
// re-superseding / cycles are rejected.
#[test]
fn supersession_chain_history_and_cycle_rejection() {
    let db = Database::open_in_memory().unwrap();
    let friday = db
        .write(|tx| {
            memory::insert(
                tx,
                &NewMemory::new(
                    MemoryKind::ImportantDate,
                    "Meeting Friday",
                    Origin::AgentDerived,
                ),
            )
        })
        .unwrap()
        .value;
    let saturday = db
        .write(|tx| {
            memory::insert(
                tx,
                &NewMemory::new(
                    MemoryKind::ImportantDate,
                    "Meeting Saturday",
                    Origin::AgentDerived,
                ),
            )
        })
        .unwrap()
        .value;

    db.write(|tx| memory::supersede(tx, friday, saturday))
        .unwrap();

    let old = db.read(|r| memory::get(r, friday)).unwrap().unwrap();
    assert_eq!(old.status, MemoryStatus::Superseded);
    assert_eq!(old.superseded_by, Some(saturday));

    let hist = db.read(|r| memory::history(r, saturday)).unwrap();
    assert_eq!(
        hist.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![friday, saturday]
    );

    let active = db
        .read(|r| {
            memory::list(
                r,
                &memory::MemoryFilter {
                    statuses: Some(vec![MemoryStatus::Candidate, MemoryStatus::Derived]),
                    ..Default::default()
                },
            )
        })
        .unwrap();
    assert!(active.iter().all(|m| m.id != friday));
    assert!(active.iter().any(|m| m.id == saturday));

    // Re-superseding an already-superseded item is rejected ("old" not active).
    let err = db
        .write(|tx| memory::supersede(tx, friday, saturday))
        .unwrap_err();
    assert!(matches!(err, StoreError::Invariant(_)));

    // A cycle back to `friday` through `saturday` is rejected.
    let err = db
        .write(|tx| memory::supersede(tx, saturday, friday))
        .unwrap_err();
    assert!(matches!(err, StoreError::Invariant(_)));
}

// 3. Fingerprint dedupe lookup, plus reinforce() merging sources and raising
// confidence.
#[test]
fn fingerprint_dedupe_and_reinforce_merges() {
    let db = Database::open_in_memory().unwrap();
    let entity = EntityId::User(UserId(42));
    let fp = MemoryFingerprint::compute(MemoryKind::Preference, &[entity], "forever");

    let mut m = NewMemory::new(
        MemoryKind::Preference,
        "prefers dark mode",
        Origin::AgentDerived,
    );
    m.fingerprint = Some(fp.clone());
    m.confidence = Confidence::new(0.4).unwrap();
    m.source_refs = vec![SourceRef::new(EntityId::Message(MessageId(1)))];
    let id = db.write(|tx| memory::insert(tx, &m)).unwrap().value;

    let found = db
        .read(|r| memory::find_active_by_fingerprint(r, &fp))
        .unwrap()
        .unwrap();
    assert_eq!(found.id, id);
    assert_eq!(found.source_refs.len(), 1);

    let more_sources = [
        SourceRef::new(EntityId::Message(MessageId(2))),
        SourceRef::new(EntityId::Message(MessageId(1))), // duplicate, ignored
    ];
    let changed = db
        .write(|tx| memory::reinforce(tx, id, &more_sources, Confidence::new(0.9).unwrap()))
        .unwrap()
        .value;
    assert!(changed);

    let item = db.read(|r| memory::get(r, id)).unwrap().unwrap();
    assert_eq!(
        item.source_refs.len(),
        2,
        "duplicate source must be ignored"
    );
    assert!((item.confidence.get() - 0.9).abs() < 1e-6);

    // Reinforcing with a lower confidence than already stored is a no-op for
    // confidence (but may still add sources); here nothing changes at all.
    let noop = db
        .write(|tx| memory::reinforce(tx, id, &[], Confidence::new(0.1).unwrap()))
        .unwrap();
    assert!(!noop.changed());
}

// 4. expire_due expires unpinned items only.
#[test]
fn expire_due_only_touches_unpinned_items() {
    let db = Database::open_in_memory().unwrap();
    let mut expiring = NewMemory::new(MemoryKind::Observation, "old news", Origin::AgentDerived);
    expiring.expires_at = Some(Timestamp::from_millis(1_000));
    let mut pinned = NewMemory::new(
        MemoryKind::Observation,
        "important pin",
        Origin::AgentDerived,
    );
    pinned.expires_at = Some(Timestamp::from_millis(1_000));
    pinned.pinned = true;

    let expiring_id = db.write(|tx| memory::insert(tx, &expiring)).unwrap().value;
    let pinned_id = db.write(|tx| memory::insert(tx, &pinned)).unwrap().value;

    let expired = db
        .write(|tx| memory::expire_due(tx, Timestamp::from_millis(2_000)))
        .unwrap()
        .value;
    assert_eq!(expired, vec![expiring_id]);
    assert_eq!(
        db.read(|r| memory::get(r, expiring_id))
            .unwrap()
            .unwrap()
            .status,
        MemoryStatus::Expired
    );
    assert_eq!(
        db.read(|r| memory::get(r, pinned_id))
            .unwrap()
            .unwrap()
            .status,
        MemoryStatus::Candidate
    );
}

// 5. Memory search finds by content and respects a status filter.
#[test]
fn memory_search_by_content_and_status() {
    let db = Database::open_in_memory().unwrap();
    let mut confirmed = NewMemory::new(
        MemoryKind::Fact,
        "the rocket launch is Tuesday",
        Origin::UserProvided,
    );
    confirmed.status = MemoryStatus::UserConfirmed;
    db.write(|tx| memory::insert(tx, &confirmed)).unwrap();
    db.write(|tx| {
        memory::insert(
            tx,
            &NewMemory::new(MemoryKind::Fact, "unrelated fact", Origin::AgentDerived),
        )
    })
    .unwrap();

    let hits = db
        .read(|r| {
            memory::search(
                r,
                &memory::MemorySearch {
                    query: "rocket launch",
                    statuses: None,
                    kinds: None,
                    entity: None,
                    since: None,
                    mode: FtsMode::All,
                    limit: 10,
                },
            )
        })
        .unwrap();
    assert_eq!(hits.len(), 1);

    let filtered_out = db
        .read(|r| {
            memory::search(
                r,
                &memory::MemorySearch {
                    query: "rocket",
                    statuses: Some(&[MemoryStatus::Candidate]),
                    kinds: None,
                    entity: None,
                    since: None,
                    mode: FtsMode::Any,
                    limit: 10,
                },
            )
        })
        .unwrap();
    assert!(filtered_out.is_empty());
}

// 6. Edge upsert is idempotent (no revision churn on an identical repeat) and
// neighbors() sees both directions.
#[test]
fn edge_upsert_idempotent_and_neighbors() {
    let db = Database::open_in_memory().unwrap();
    let alice = EntityId::User(UserId(1));
    let bob = EntityId::User(UserId(2));

    let first = db
        .write(|tx| {
            edges::upsert(
                tx,
                &alice,
                RelationType::FriendOf,
                &bob,
                Origin::UserProvided,
                Confidence::new(0.8).unwrap(),
            )
        })
        .unwrap();
    assert!(first.changed());
    let rev_after_first = first.revision;

    let repeat = db
        .write(|tx| {
            edges::upsert(
                tx,
                &alice,
                RelationType::FriendOf,
                &bob,
                Origin::UserProvided,
                Confidence::new(0.8).unwrap(),
            )
        })
        .unwrap();
    assert!(!repeat.changed());
    assert_eq!(
        repeat.revision, rev_after_first,
        "identical upsert must not bump the revision"
    );

    let neighbors_alice = db.read(|r| edges::neighbors(r, &alice, 0)).unwrap();
    let neighbors_bob = db.read(|r| edges::neighbors(r, &bob, 0)).unwrap();
    assert_eq!(neighbors_alice.len(), 1);
    assert_eq!(neighbors_bob.len(), 1);
}

// 7. Tasks: create as candidate, move to open, Done stamps completed_at;
// search finds by title.
#[test]
fn task_lifecycle_and_search() {
    let db = Database::open_in_memory().unwrap();
    let draft = TaskDraft {
        title: "Ship the release notes".to_string(),
        description: Some("Summarize v2 changes".to_string()),
        due_at: None,
        related_users: vec![UserId(7)],
        conversation_id: None,
        source: None,
    };
    let id = db
        .write(|tx| tasks::create(tx, &draft, TaskStatus::Candidate, Origin::AgentDerived))
        .unwrap()
        .value;

    let opened = db
        .write(|tx| tasks::set_status(tx, id, TaskStatus::Open, Origin::UserProvided))
        .unwrap()
        .value;
    assert!(opened);

    let done = db
        .write(|tx| tasks::set_status(tx, id, TaskStatus::Done, Origin::UserProvided))
        .unwrap()
        .value;
    assert!(done);
    let t = db.read(|r| tasks::get(r, id)).unwrap().unwrap();
    assert!(t.completed_at.is_some());
    assert_eq!(t.related_users, vec![UserId(7)]);

    let hits = db
        .read(|r| tasks::search(r, "release notes", None, 10))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0.id, id);
}

// 8. Reminders: `At` and `Conditional` round-trip; due() only returns
// pending, due items; `Fired` emits `ReminderFired`.
#[test]
fn reminder_round_trip_due_and_fired_event() {
    let db = Database::open_in_memory().unwrap();

    let at_draft = ReminderDraft {
        title: "ping the team".to_string(),
        note: None,
        trigger: ReminderTrigger::At {
            at: Timestamp::from_millis(1_000),
        },
        conversation_id: None,
        task_id: None,
        source: None,
    };
    let cond_draft = ReminderDraft {
        title: "check for a reply".to_string(),
        note: None,
        trigger: ReminderTrigger::Conditional {
            condition: ReminderCondition::NoReplyFrom {
                user_id: UserId(1),
                conversation_id: ConversationId(1),
                since: Timestamp::from_millis(0),
            },
            check_at: Timestamp::from_millis(50_000),
        },
        conversation_id: None,
        task_id: None,
        source: None,
    };

    let at_id = db
        .write(|tx| reminders::create(tx, &at_draft, Origin::UserProvided))
        .unwrap()
        .value;
    let cond_id = db
        .write(|tx| reminders::create(tx, &cond_draft, Origin::AgentDerived))
        .unwrap()
        .value;

    let loaded_cond = db.read(|r| reminders::get(r, cond_id)).unwrap().unwrap();
    assert_eq!(loaded_cond.trigger, cond_draft.trigger);

    let due_soon = db
        .read(|r| reminders::due(r, Timestamp::from_millis(2_000), 0))
        .unwrap();
    assert_eq!(
        due_soon.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![at_id]
    );

    let committed = db
        .write(|tx| {
            reminders::set_status(tx, at_id, ReminderStatus::Fired, Origin::LocalApplication)
        })
        .unwrap();
    assert!(committed.changed());
    assert!(committed.events.iter().any(
        |e| matches!(e, UnifiedEvent::ReminderFired { reminder_id } if *reminder_id == at_id)
    ));

    let due_after_fired = db
        .read(|r| reminders::due(r, Timestamp::from_millis(2_000), 0))
        .unwrap();
    assert!(
        due_after_fired.is_empty(),
        "fired reminders are no longer pending/due"
    );
}

// 9. Settings no-op on an identical value; note append; bookmark idempotency.
#[test]
fn settings_notes_and_bookmarks() {
    let db = Database::open_in_memory().unwrap();

    let v = serde_json::json!({"theme": "dark"});
    let first = db
        .write(|tx| settings::set_json(tx, "ui.theme", &v))
        .unwrap();
    assert!(first.changed());
    let repeat = db
        .write(|tx| settings::set_json(tx, "ui.theme", &v))
        .unwrap();
    assert!(!repeat.changed());

    let user = UserId(9);
    db.write(|tx| notes::append_note(tx, user, "met at conference", Origin::UserProvided))
        .unwrap();
    db.write(|tx| notes::append_note(tx, user, "follow up next week", Origin::UserProvided))
        .unwrap();
    let note = db.read(|r| notes::get_note(r, user)).unwrap().unwrap();
    assert_eq!(
        note.note.as_deref(),
        Some("met at conference\nfollow up next week")
    );

    let bookmark = Bookmark {
        message_id: MessageId(123),
        conversation_id: ConversationId(1),
        note: Some("funny".to_string()),
        created_at: Timestamp::from_millis(0),
    };
    let added = db
        .write(|tx| notes::add_bookmark(tx, &bookmark, Origin::UserProvided))
        .unwrap();
    assert!(added.changed());
    let added_again = db
        .write(|tx| notes::add_bookmark(tx, &bookmark, Origin::UserProvided))
        .unwrap();
    assert!(
        !added_again.changed(),
        "identical bookmark re-add is a no-op"
    );
    assert!(db
        .read(|r| notes::is_bookmarked(r, bookmark.message_id))
        .unwrap());

    // Also exercise upsert_note directly (whole-record replace) since
    // append_note only covers the free-text field.
    let full_note = UserNote {
        user_id: user,
        alias: Some("Sam".to_string()),
        note: note.note.clone(),
        favorite: true,
        updated_at: Timestamp::from_millis(0),
    };
    db.write(|tx| notes::upsert_note(tx, &full_note)).unwrap();
    let favorites = db.read(|r| notes::favorites(r)).unwrap();
    assert_eq!(favorites.len(), 1);
}

// 10. Actions: proposal insert/get round trip; approval consume is
// single-use; revoke marks unconsumed approvals consumed; audit trail order.
#[test]
fn action_proposal_approval_and_audit() {
    let db = Database::open_in_memory().unwrap();
    let action = AgentAction::SendMessage {
        target: MessageTarget::User { user_id: UserId(5) },
        content: "hello!".to_string(),
    };
    let actor = Actor::Agent {
        harness: "cli".to_string(),
        run_id: None,
    };
    let payload_json = serde_json::to_string(&action).unwrap();

    let action_id = db
        .write(|tx| {
            actions::insert_proposal(
                tx,
                &actions::NewProposal {
                    action: &action,
                    actor: &actor,
                    class: CapabilityClass::DiscordWrite,
                    identity: DiscordIdentity::UserSocialSdk,
                    payload_json: &payload_json,
                    payload_hash: "deadbeef",
                    status: ActionStatus::PendingApproval,
                    based_on_revision: Revision(1),
                    rationale: Some("user asked to reply"),
                },
            )
        })
        .unwrap()
        .value;

    let loaded = db.read(|r| actions::get(r, action_id)).unwrap().unwrap();
    assert_eq!(loaded.action, action);
    assert_eq!(loaded.class, CapabilityClass::DiscordWrite);

    db.write(|tx| {
        actions::insert_approval(
            tx,
            action_id,
            "deadbeef",
            "nonce-a",
            Timestamp::from_millis(0),
            Timestamp::from_millis(60_000),
            false,
        )
    })
    .unwrap();

    let consumed_once = db
        .write(|tx| actions::consume_approval(tx, "nonce-a", Timestamp::from_millis(1)))
        .unwrap()
        .value;
    assert!(consumed_once);
    let consumed_twice = db
        .write(|tx| actions::consume_approval(tx, "nonce-a", Timestamp::from_millis(2)))
        .unwrap()
        .value;
    assert!(!consumed_twice, "approval must be single-use");

    db.write(|tx| {
        actions::insert_approval(
            tx,
            action_id,
            "deadbeef",
            "nonce-b",
            Timestamp::from_millis(0),
            Timestamp::from_millis(60_000),
            false,
        )
    })
    .unwrap();
    let revoked = db
        .write(|tx| actions::revoke_approvals(tx, action_id, Timestamp::from_millis(3)))
        .unwrap()
        .value;
    assert_eq!(revoked, 1);
    assert!(db
        .read(|r| actions::get_approval(r, "nonce-b"))
        .unwrap()
        .unwrap()
        .consumed_at
        .is_some());

    db.write(|tx| actions::set_status(tx, action_id, ActionStatus::Approved, Origin::UserProvided))
        .unwrap();
    db.write(|tx| {
        actions::append_audit(
            tx,
            &ActionAuditEntry {
                action_id,
                at: Timestamp::from_millis(1),
                actor: actor.clone(),
                event: AuditEvent::Proposed,
                revision: Revision(1),
            },
        )
    })
    .unwrap();
    db.write(|tx| {
        actions::append_audit(
            tx,
            &ActionAuditEntry {
                action_id,
                at: Timestamp::from_millis(2),
                actor: Actor::User,
                event: AuditEvent::Approved { edited: false },
                revision: Revision(2),
            },
        )
    })
    .unwrap();

    let trail = db.read(|r| actions::audit_for(r, action_id)).unwrap();
    assert_eq!(trail.len(), 2);
    assert_eq!(trail[0].event, AuditEvent::Proposed);
    assert_eq!(trail[1].event, AuditEvent::Approved { edited: false });
}

// 11. Embeddings round-trip and corrupt/invalid rejection.
#[test]
fn embeddings_round_trip_and_validation() {
    let db = Database::open_in_memory().unwrap();
    let owner = EntityId::Memory(litecord_types::MemoryId(1));

    assert!(db
        .write(|tx| embeddings::upsert(tx, &owner, "m1", &[]))
        .is_err());
    assert!(db
        .write(|tx| embeddings::upsert(tx, &owner, "m1", &[1.0, f32::INFINITY]))
        .is_err());

    db.write(|tx| embeddings::upsert(tx, &owner, "m1", &[0.1, 0.2, 0.3]))
        .unwrap();
    let v = db
        .read(|r| embeddings::get(r, &owner, "m1"))
        .unwrap()
        .unwrap();
    assert_eq!(v.len(), 3);

    let n = db
        .write(|tx| embeddings::delete_for_owner(tx, &owner))
        .unwrap()
        .value;
    assert_eq!(n, 1);
    assert!(db
        .read(|r| embeddings::get(r, &owner, "m1"))
        .unwrap()
        .is_none());
}

// Extra: local entities + drafts + agent runs, tying a few more repos
// together to make sure they compose (e.g. a local entity used as a memory
// source, a draft lifecycle, and an agent run wrapping a batch of writes).
#[test]
fn local_entities_drafts_and_agent_runs_compose() {
    let db = Database::open_in_memory().unwrap();
    let conv = ConversationId(10);
    db.write(|tx| {
        stub_conversation(tx, conv);
        Ok::<_, StoreError>(())
    })
    .unwrap();

    let topic = db
        .write(|tx| {
            local_entities::get_or_create(
                tx,
                LocalEntityKind::Topic,
                "Q3 Roadmap",
                Origin::AgentDerived,
            )
        })
        .unwrap()
        .value;

    let run_id = db
        .write(|tx| agent_runs::start(tx, "cli", "compile context for Q3 roadmap", Revision(0)))
        .unwrap()
        .value;

    let mut m = NewMemory::new(
        MemoryKind::Observation,
        "the team discussed the Q3 roadmap",
        Origin::AgentDerived,
    );
    m.entities = vec![topic];
    let mem_id = db.write(|tx| memory::insert(tx, &m)).unwrap().value;
    let stored = db.read(|r| memory::get(r, mem_id)).unwrap().unwrap();
    assert_eq!(stored.entities, vec![topic]);

    let draft_id = db
        .write(|tx| drafts::create(tx, conv, "Draft: here's the Q3 plan", Origin::AgentDerived))
        .unwrap()
        .value;
    db.write(|tx| drafts::set_status(tx, draft_id, DraftStatus::Proposed))
        .unwrap();
    let d: Draft = db.read(|r| drafts::get(r, draft_id)).unwrap().unwrap();
    assert_eq!(d.status, DraftStatus::Proposed);

    db.write(|tx| agent_runs::finish(tx, run_id, true, 2, 500, None))
        .unwrap();
    let run = db.read(|r| agent_runs::get(r, run_id)).unwrap().unwrap();
    assert_eq!(run.status, "succeeded");
}
