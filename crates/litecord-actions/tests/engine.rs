#![allow(clippy::unwrap_used, clippy::expect_used)]

//! End-to-end Action Engine behavior against the mock Discord backend:
//! propose → approve → payload-bound token → revalidate → execute → audit.

use std::sync::Arc;

use discord_adapter::fixtures::{self, DemoData};
use discord_adapter::mock::MockBackend;
use litecord_actions::{ActionEngine, ActionError, DefaultExecutor, ProposeOutcome};
use litecord_core::bus::ingest_channel;
use litecord_core::clock::ManualClock;
use litecord_core::config::AgentConfig;
use litecord_core::events::{DiscordEvent, SourceEnvelope};
use litecord_core::ports::SocialBackend;
use litecord_store::reducer::{self, ReducerConfig};
use litecord_store::{repos, Database};
use litecord_types::actions::*;
use litecord_types::entity::EntityId;
use litecord_types::ids::*;
use litecord_types::provenance::{DiscordSource, Origin};
use litecord_types::social::{Relationship, RelationshipKind};
use litecord_types::tasks::{TaskDraft, TaskStatus};
use litecord_types::trust::AgentVisibility;
use litecord_types::{DurationMs, Revision, Timestamp};

const NOW: Timestamp = Timestamp(1_790_262_000_000);

struct Harness {
    db: Database,
    backend: Arc<MockBackend>,
    engine: ActionEngine,
    data: DemoData,
    clock: ManualClock,
}

fn apply(db: &Database, event: DiscordEvent) {
    reducer::apply(
        db,
        &SourceEnvelope::new(DiscordSource::Synthetic, NOW, event),
        &ReducerConfig::default(),
    )
    .unwrap();
}

async fn setup(cfg: AgentConfig) -> Harness {
    let clock = ManualClock::new(NOW);
    let data = fixtures::generate(42, NOW);
    let db = Database::open_in_memory_with(Arc::new(clock.clone()), None).unwrap();
    apply(
        &db,
        DiscordEvent::CurrentUser {
            user: data.current_user.clone(),
        },
    );
    let entries = data
        .relationships
        .iter()
        .map(|r| {
            let u = data.users.iter().find(|u| u.id == r.user_id).cloned();
            (r.clone(), u)
        })
        .collect();
    apply(&db, DiscordEvent::RelationshipsSnapshot { entries });
    apply(
        &db,
        DiscordEvent::ConversationsSnapshot {
            conversations: data.conversations.clone(),
        },
    );
    for c in &data.conversations {
        let messages = data
            .messages
            .iter()
            .filter(|m| m.conversation_id == c.id)
            .cloned()
            .collect();
        apply(
            &db,
            DiscordEvent::MessagesSnapshot {
                conversation_id: c.id,
                messages,
            },
        );
    }
    let backend = Arc::new(MockBackend::with_clock(
        data.clone(),
        Arc::new(clock.clone()),
    ));
    let (sink, _rx) = ingest_channel(1024);
    backend.connect(sink).await.unwrap();
    let executor = Arc::new(DefaultExecutor::new(db.clone(), Some(backend.clone())));
    let engine = ActionEngine::new(db.clone(), &cfg, executor).unwrap();
    Harness {
        db,
        backend,
        engine,
        data,
        clock,
    }
}

fn agent() -> Actor {
    Actor::Agent {
        harness: "test".into(),
        run_id: None,
    }
}

fn first_friend(data: &DemoData) -> UserId {
    data.relationships
        .iter()
        .find(|r| r.discord == RelationshipKind::Friend)
        .unwrap()
        .user_id
}

fn send_to_conversation(data: &DemoData, content: &str) -> AgentAction {
    AgentAction::SendMessage {
        target: MessageTarget::Conversation {
            conversation_id: data.conversations[0].id,
        },
        content: content.into(),
    }
}

fn pending_id(outcome: ProposeOutcome) -> ActionId {
    match outcome {
        ProposeOutcome::PendingApproval { action_id } => action_id,
        other => panic!("expected pending approval, got {other:?}"),
    }
}

fn audit_kinds(h: &Harness, id: ActionId) -> Vec<String> {
    h.engine
        .audit(id)
        .unwrap()
        .into_iter()
        .map(|e| {
            serde_json::to_value(&e.event).unwrap()["event"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[tokio::test]
async fn agent_send_requires_approval_then_executes_with_audit_trail() {
    let h = setup(AgentConfig::default()).await;
    let action = send_to_conversation(&h.data, "Thanks, I'll look tonight.");
    let id = pending_id(
        h.engine
            .propose(
                action,
                agent(),
                Revision(1),
                Some("reply to question".into()),
            )
            .await
            .unwrap(),
    );
    assert_eq!(
        h.backend.calls("send_message"),
        0,
        "nothing sent before approval"
    );

    let token = h.engine.approve(id, None).unwrap();
    let outcome = h.engine.execute(&token).await.unwrap();
    assert_eq!(outcome.summary, "message sent");
    assert_eq!(h.backend.calls("send_message"), 1);
    assert_eq!(
        h.engine.get(id).unwrap().unwrap().status,
        ActionStatus::Executed
    );
    assert_eq!(
        audit_kinds(&h, id),
        ["proposed", "approved", "execution_started", "executed"]
    );

    // Tokens are single-use.
    assert!(h.engine.execute(&token).await.is_err());
    assert_eq!(h.backend.calls("send_message"), 1);
}

#[tokio::test]
async fn editing_after_approval_invalidates_the_old_token() {
    let h = setup(AgentConfig::default()).await;
    let id = pending_id(
        h.engine
            .propose(
                send_to_conversation(&h.data, "original"),
                agent(),
                Revision(1),
                None,
            )
            .await
            .unwrap(),
    );
    let old = h.engine.approve(id, None).unwrap();
    let edited = send_to_conversation(&h.data, "edited by user");
    let new = h.engine.approve(id, Some(edited)).unwrap();
    assert_ne!(old.payload_hash(), new.payload_hash());

    assert!(matches!(
        h.engine.execute(&old).await,
        Err(ActionError::PayloadMismatch)
    ));
    h.engine.execute(&new).await.unwrap();
    let conv = h.data.conversations[0].id;
    let last = h.backend.messages(conv, 1).await.unwrap().pop().unwrap();
    assert_eq!(&*last.content, "edited by user");

    // Changing the operation kind in an edit is refused.
    let id2 = pending_id(
        h.engine
            .propose(
                send_to_conversation(&h.data, "x"),
                agent(),
                Revision(1),
                None,
            )
            .await
            .unwrap(),
    );
    let other_kind = AgentAction::DraftMessage {
        conversation_id: conv,
        content: "x".into(),
    };
    assert!(matches!(
        h.engine.approve(id2, Some(other_kind)),
        Err(ActionError::Invalid(_))
    ));
}

#[tokio::test]
async fn approval_tokens_expire() {
    let cfg = AgentConfig {
        approval_ttl_secs: 1,
        ..AgentConfig::default()
    };
    let h = setup(cfg).await;
    let id = pending_id(
        h.engine
            .propose(
                send_to_conversation(&h.data, "hi"),
                agent(),
                Revision(1),
                None,
            )
            .await
            .unwrap(),
    );
    let token = h.engine.approve(id, None).unwrap();
    h.clock.advance(DurationMs::from_secs(2));
    assert!(matches!(
        h.engine.execute(&token).await,
        Err(ActionError::TokenExpired)
    ));
    assert_eq!(
        h.engine.get(id).unwrap().unwrap().status,
        ActionStatus::Expired
    );
    assert_eq!(h.backend.calls("send_message"), 0);
}

#[tokio::test]
async fn state_change_after_approval_is_caught_by_revalidation() {
    let h = setup(AgentConfig::default()).await;
    let friend = first_friend(&h.data);
    let id = pending_id(
        h.engine
            .propose(
                AgentAction::SendMessage {
                    target: MessageTarget::User { user_id: friend },
                    content: "hello".into(),
                },
                agent(),
                Revision(1),
                None,
            )
            .await
            .unwrap(),
    );
    let token = h.engine.approve(id, None).unwrap();

    // The user blocks the recipient before execution.
    apply(
        &h.db,
        DiscordEvent::RelationshipUpserted {
            relationship: Relationship {
                user_id: friend,
                discord: RelationshipKind::Blocked,
                game: RelationshipKind::None,
                since: None,
            },
            user: None,
        },
    );
    assert!(matches!(
        h.engine.execute(&token).await,
        Err(ActionError::RevalidationFailed(_))
    ));
    assert_eq!(
        h.engine.get(id).unwrap().unwrap().status,
        ActionStatus::Invalidated
    );
    assert_eq!(h.backend.calls("send_message"), 0);
    assert!(audit_kinds(&h, id).contains(&"invalidated".to_string()));
}

#[tokio::test]
async fn agent_local_writes_auto_execute_as_candidates() {
    let h = setup(AgentConfig::default()).await;
    let out = h
        .engine
        .propose(
            AgentAction::CreateTask {
                task: TaskDraft {
                    title: "Send revised design doc".into(),
                    description: None,
                    due_at: None,
                    related_users: vec![first_friend(&h.data)],
                    conversation_id: None,
                    source: None,
                },
            },
            agent(),
            Revision(1),
            None,
        )
        .await
        .unwrap();
    let ProposeOutcome::Executed { action_id, result } = out else {
        panic!("expected execution");
    };
    let Some(EntityId::Task(task_id)) = result.entity else {
        panic!("expected task entity");
    };
    let task =
        h.db.read(|r| repos::tasks::get(r, task_id))
            .unwrap()
            .unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Candidate,
        "agent tasks need confirmation"
    );
    assert_eq!(task.origin, Origin::AgentDerived);
    assert_eq!(
        audit_kinds(&h, action_id),
        ["proposed", "auto_approved", "execution_started", "executed"]
    );
}

#[tokio::test]
async fn policy_denials_are_audited_and_hidden_conversations_refused() {
    let h = setup(AgentConfig::default()).await;
    let err = h
        .engine
        .propose(
            AgentAction::RelationshipChange {
                user_id: first_friend(&h.data),
                action: RelationshipAction::Block,
            },
            agent(),
            Revision(1),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ActionError::PolicyDenied(_)));
    let denied =
        h.db.read(|r| repos::actions::list(r, Some(&[ActionStatus::Rejected]), 10))
            .unwrap();
    assert_eq!(denied.len(), 1);
    assert_eq!(audit_kinds(&h, denied[0].id), ["proposed", "denied"]);

    let conv = h.data.conversations[0].id;
    h.db.write(|tx| repos::conversations::set_visibility(tx, conv, Some(AgentVisibility::Hidden)))
        .unwrap();
    assert!(matches!(
        h.engine
            .propose(
                send_to_conversation(&h.data, "hi"),
                agent(),
                Revision(1),
                None
            )
            .await,
        Err(ActionError::Invalid(_))
    ));
}

#[tokio::test]
async fn user_sends_execute_immediately_and_only_own_messages_are_editable() {
    let h = setup(AgentConfig::default()).await;
    let out = h
        .engine
        .propose(
            send_to_conversation(&h.data, "from the composer"),
            Actor::User,
            Revision(1),
            None,
        )
        .await
        .unwrap();
    assert!(matches!(out, ProposeOutcome::Executed { .. }));

    let someone_elses = h
        .data
        .messages
        .iter()
        .find(|m| m.author_id != h.data.current_user.id)
        .unwrap()
        .id;
    assert!(matches!(
        h.engine
            .propose(
                AgentAction::DeleteMessage {
                    message_id: someone_elses
                },
                Actor::User,
                Revision(1),
                None,
            )
            .await,
        Err(ActionError::Invalid(_))
    ));
}
