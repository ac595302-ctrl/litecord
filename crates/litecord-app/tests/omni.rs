#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Omni on a (scripted) harness: lazy sidecar, streaming, persisted
//! transcript, approval bridge by mode, remember-into-memory, idle stop.

use std::sync::Arc;
use std::time::Duration;

use discord_adapter::{fixtures, MockBackend};
use litecord_app::harness::{
    Decision, FakeDriver, FakeLauncher, FakeTurn, HarnessKind, ItemKind, LoginState, OmniMode,
    RequestKind, TranscriptItem,
};
use litecord_app::omni::in_quiet_hours;
use litecord_app::{HeartbeatOutcome, LitecordApp, OmniEvent};
use litecord_core::config::LitecordConfig;
use litecord_types::memory::MemoryStatus;
use litecord_types::provenance::Origin;
use litecord_types::Timestamp;

struct Harness {
    app: LitecordApp,
    launcher: FakeLauncher,
    _dir: tempfile::TempDir,
}

async fn start(driver: FakeDriver, idle_secs: u64) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = LitecordConfig {
        data_dir: dir.path().to_path_buf(),
        ..LitecordConfig::default()
    };
    cfg.omni.idle_shutdown_secs = idle_secs;
    // Deterministic regardless of the hour the test runs.
    cfg.omni.heartbeat.quiet_start_hour = 0;
    cfg.omni.heartbeat.quiet_end_hour = 0;
    cfg.omni.approval_timeout_secs = 1;
    let launcher = FakeLauncher::new(driver);
    let app = LitecordApp::builder(cfg)
        .backend(Arc::new(MockBackend::new(fixtures::generate(
            5,
            Timestamp::now(),
        ))))
        .omni_launcher(Arc::new(launcher.clone()))
        .omni_mcp_command("/usr/bin/litecord".into())
        .start()
        .await
        .unwrap();
    Harness {
        app,
        launcher,
        _dir: dir,
    }
}

async fn until<T>(mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..200 {
        if let Some(v) = f() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_streams_and_persists_completed_items_only() {
    let h = start(
        FakeDriver::new(|text| FakeTurn {
            reply: format!("You said: {text}"),
            items: vec![TranscriptItem {
                kind: ItemKind::ToolCall,
                text: "litecord.compile_context".into(),
            }],
            ..FakeTurn::default()
        }),
        600,
    )
    .await;
    let omni = h.app.omni();
    let status = omni.status();
    assert_eq!(status.selected, Some(HarnessKind::Fake));
    assert!(!status.running, "no sidecar before first use");
    assert_eq!(status.login, LoginState::Stopped);

    let mut events = omni.subscribe();
    let sid = omni.send(None, "who needs a reply?").await.unwrap();
    assert_eq!(h.launcher.launches(), 1);

    let mut streamed = String::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            OmniEvent::Delta { session_id, text } => {
                assert_eq!(session_id, sid);
                streamed.push_str(&text);
            }
            OmniEvent::Changed { .. } => {
                if !omni.view(Some(sid)).unwrap().active.unwrap().running
                    && streamed.len() == "You said: who needs a reply?".len()
                {
                    break;
                }
            }
        }
    }
    assert_eq!(streamed, "You said: who needs a reply?");

    let view = until(|| {
        let v = omni.view(Some(sid)).unwrap();
        (v.items.len() == 3 && v.active.as_ref().is_some_and(|a| a.turns == 1)).then_some(v)
    })
    .await;
    let kinds: Vec<_> = view
        .items
        .iter()
        .map(|i| (i.role.as_str(), i.kind.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("user", "message"),
            ("omni", "tool_call"),
            ("omni", "message")
        ]
    );
    assert_eq!(view.active.as_ref().unwrap().turns, 1);
    assert_eq!(view.active.as_ref().unwrap().title, "who needs a reply?");
    assert!(
        view.streaming.is_none(),
        "streaming text is never persisted"
    );
    assert!(view.status.running);

    // Second message reuses the attached session (no new harness session).
    omni.send(Some(sid), "thanks").await.unwrap();
    until(|| (omni.view(Some(sid)).unwrap().items.len() == 6).then_some(())).await;
    let sent = h.launcher.driver().sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].0, sent[1].0);
    h.app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn assistant_mode_declines_local_commands_without_asking() {
    let h = start(
        FakeDriver::new(|_| FakeTurn {
            reply: "done".into(),
            request: Some(RequestKind::Command {
                command: "cat ~/.litecord/litecord.db".into(),
                cwd: None,
            }),
            ..FakeTurn::default()
        }),
        600,
    )
    .await;
    let omni = h.app.omni();
    let sid = omni.send(None, "read my files").await.unwrap();
    let view = until(|| {
        let v = omni.view(Some(sid)).unwrap();
        v.items
            .iter()
            .any(|i| i.role == "omni" && i.kind == "message")
            .then_some(v)
    })
    .await;
    assert!(view.requests.is_empty(), "never shown to the user");
    let texts: Vec<_> = view.items.iter().map(|i| i.text.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("Assistant mode")),
        "{texts:?}"
    );
    assert!(texts.iter().any(|t| t.contains("(declined)")), "{texts:?}");
    h.app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_mode_asks_and_times_out_to_decline() {
    let h = start(
        FakeDriver::new(|_| FakeTurn {
            reply: "ok".into(),
            request: Some(RequestKind::Command {
                command: "ls".into(),
                cwd: None,
            }),
            ..FakeTurn::default()
        }),
        600,
    )
    .await;
    let omni = h.app.omni();

    // Answered by the user.
    let sid = omni.new_session(OmniMode::Workspace, "files").unwrap();
    omni.send(Some(sid), "list files").await.unwrap();
    let req = until(|| omni.view(Some(sid)).unwrap().requests.first().cloned()).await;
    assert_eq!(req.session_id, sid);
    omni.answer(&req.id, Decision::Accept).await.unwrap();
    until(|| {
        omni.view(Some(sid))
            .unwrap()
            .items
            .iter()
            .any(|i| i.text == "ls (ran)")
            .then_some(())
    })
    .await;
    assert!(
        omni.answer(&req.id, Decision::Accept).await.is_err(),
        "single use"
    );

    // Unanswered: declined after the timeout (1 s in this test).
    omni.send(Some(sid), "again").await.unwrap();
    until(|| omni.view(Some(sid)).unwrap().requests.first().cloned()).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let view = omni.view(Some(sid)).unwrap();
    assert!(view.requests.is_empty());
    assert!(view.items.iter().any(|i| i.text == "ls (declined)"));
    h.app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remember_creates_an_agent_derived_memory_candidate() {
    let h = start(
        FakeDriver::new(|_| FakeTurn {
            reply: "Ada's birthday is on the 12th".into(),
            ..FakeTurn::default()
        }),
        600,
    )
    .await;
    let omni = h.app.omni();
    let sid = omni.send(None, "when is Ada's birthday?").await.unwrap();
    let item = until(|| {
        omni.view(Some(sid))
            .unwrap()
            .items
            .into_iter()
            .find(|i| i.role == "omni" && i.kind == "message")
    })
    .await;
    let id = omni.remember(sid, item.seq).unwrap();
    let mem = h
        .app
        .database()
        .read(|r| litecord_store::repos::memory::get(r, id))
        .unwrap()
        .expect("memory recorded");
    assert_eq!(mem.origin, Origin::AgentDerived);
    assert_eq!(mem.status, MemoryStatus::Candidate);
    assert_eq!(&*mem.content, "Ada's birthday is on the 12th");
    assert_eq!(
        mem.source_refs[0].entity.to_string(),
        format!("omni_session:{sid}")
    );
    h.app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_sidecar_stops_and_sessions_resume() {
    let h = start(
        FakeDriver::new(|_| FakeTurn {
            reply: "hi".into(),
            ..FakeTurn::default()
        }),
        0,
    )
    .await;
    let omni = h.app.omni();
    let sid = omni.send(None, "hello").await.unwrap();
    until(|| (omni.view(Some(sid)).unwrap().items.len() == 2).then_some(())).await;
    omni.stop_if_idle().await;
    assert!(!omni.status().running);
    assert!(h.launcher.driver().is_stopped());

    omni.send(Some(sid), "back again").await.unwrap();
    assert_eq!(h.launcher.launches(), 2, "restarted on demand");
    until(|| (omni.view(Some(sid)).unwrap().items.len() == 4).then_some(())).await;
    let sent = h.launcher.driver().sent();
    assert_eq!(
        sent.last().unwrap().0,
        "fake-1",
        "resumed the same harness session"
    );
    h.app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sign_in_flow_and_unavailable_without_disk_database() {
    let h = start(FakeDriver::demo().signed_out(), 600).await;
    let omni = h.app.omni();
    assert_eq!(omni.refresh_login().await.unwrap(), LoginState::SignedOut);
    assert!(omni.send(None, "hi").await.is_err(), "signed out");
    let l = omni.sign_in().await.unwrap();
    assert!(matches!(l, LoginState::SigningIn { url: Some(_), .. }));
    h.launcher.driver().complete_login();
    until(|| omni.status().login.is_ready().then_some(())).await;
    h.app.shutdown().await;

    // In-memory databases cannot be shared with the harness's MCP server.
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(Arc::new(MockBackend::new(fixtures::generate(
            5,
            Timestamp::now(),
        ))))
        .omni_launcher(Arc::new(FakeLauncher::new(FakeDriver::demo())))
        .omni_mcp_command("/usr/bin/litecord".into())
        .in_memory()
        .start()
        .await
        .unwrap();
    assert!(app.omni().status().unavailable.is_some());
    assert!(app.omni().send(None, "hi").await.is_err());
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeats_are_opt_in_rate_limited_and_quiet_when_ok() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let turns = Arc::new(AtomicUsize::new(0));
    let t = turns.clone();
    let h = start(
        FakeDriver::new(move |_| FakeTurn {
            // First check-in: nothing to report. Later ones: something is.
            reply: if t.fetch_add(1, Ordering::SeqCst) == 0 {
                "HEARTBEAT_OK".into()
            } else {
                "- Ada is waiting for a reply".into()
            },
            ..FakeTurn::default()
        }),
        600,
    )
    .await;
    let omni = h.app.omni();
    // Let initial hydration produce events.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        omni.heartbeat(false).await.unwrap(),
        HeartbeatOutcome::Skipped("disabled")
    );
    omni.set_heartbeat_enabled(true).unwrap();
    let HeartbeatOutcome::Sent { session_id } = omni.heartbeat(false).await.unwrap() else {
        panic!("due check-in not sent");
    };
    let done = |n: u32| {
        let omni = omni.clone();
        move || {
            let v = omni.view(Some(session_id)).unwrap();
            let a = v.active.clone().unwrap();
            (a.turns == n && !a.running).then_some(v)
        }
    };
    let hb = until(done(1)).await;
    let prompt = &hb.items[0];
    assert_eq!(prompt.role, "system");
    assert!(prompt.text.contains("Scheduled check-in"));
    assert!(!prompt.text.contains("{{"));
    assert!(hb.checkins.is_empty(), "HEARTBEAT_OK is not surfaced");
    let active = hb.active.unwrap();
    assert_eq!(
        (active.kind.as_str(), active.mode),
        ("heartbeat", OmniMode::Assistant)
    );

    assert_eq!(
        omni.heartbeat(false).await.unwrap(),
        HeartbeatOutcome::Skipped("too soon")
    );
    // "Check now" bypasses the schedule; its reply needs attention.
    assert_eq!(
        omni.heartbeat(true).await.unwrap(),
        HeartbeatOutcome::Sent { session_id },
        "same rolling session"
    );
    let v = until(done(2)).await;
    assert_eq!(v.checkins.len(), 1);
    assert_eq!(v.checkins[0].text, "- Ada is waiting for a reply");
    omni.dismiss_checkins().unwrap();
    assert!(omni.view(None).unwrap().checkins.is_empty());
    h.app.shutdown().await;
}

#[test]
fn quiet_hours_wrap_midnight() {
    assert!(in_quiet_hours(23, 22, 7));
    assert!(in_quiet_hours(3, 22, 7));
    assert!(!in_quiet_hours(12, 22, 7));
    assert!(in_quiet_hours(13, 12, 14));
    assert!(!in_quiet_hours(14, 12, 14));
    assert!(!in_quiet_hours(5, 0, 0), "equal bounds disable quiet hours");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_sign_in_method_and_model_choice() {
    use litecord_app::harness::LoginKind;
    use litecord_core::secrets::Secret;
    let h = start(FakeDriver::demo().signed_out(), 600).await;
    let omni = h.app.omni();

    let options = omni.login_options().await.unwrap();
    assert!(options.iter().any(|o| o.kind == LoginKind::ApiKey));
    assert_eq!(omni.status().login_options, options);

    // Pasted-code flow.
    let l = omni.sign_in_with("code").await.unwrap();
    assert!(matches!(
        l,
        LoginState::SigningIn {
            needs_code: true,
            ..
        }
    ));
    assert!(
        omni.submit_login_code("  ").await.is_err(),
        "empty code rejected"
    );
    omni.submit_login_code("ABCD-1234").await.unwrap();
    until(|| omni.status().login.is_ready().then_some(())).await;

    // API key flow; the key never appears in status or errors.
    omni.sign_out().await.unwrap();
    let err = omni
        .sign_in_api_key("api_key", &Secret::new("short".into()))
        .await
        .unwrap_err();
    assert!(!err.to_string().contains("short"));
    omni.sign_in_api_key("api_key", &Secret::new("sk-demo-key-123".into()))
        .await
        .unwrap();
    until(|| omni.status().login.is_ready().then_some(())).await;
    assert!(!format!("{:?}", omni.status()).contains("sk-demo-key-123"));

    // Models and the saved preference.
    omni.set_model(Some("no-longer-offered")).unwrap();
    assert_eq!(omni.models().await.unwrap(), ["demo-large", "demo-small"]);
    assert_eq!(omni.status().model, None);
    assert!(omni
        .status()
        .last_error
        .as_deref()
        .unwrap()
        .contains("unavailable"));
    assert!(omni.set_model(Some("no-longer-offered")).is_err());
    omni.set_model(Some("demo-small")).unwrap();
    assert_eq!(omni.status().model.as_deref(), Some("demo-small"));
    omni.set_model(None).unwrap();
    assert_eq!(omni.model(), None);
    h.app.shutdown().await;
}

mod automations {
    use super::*;
    use litecord_app::automations::{
        AutomationDraft, AutomationOutcome, AutomationOutputKind, AutomationTrigger,
    };
    use litecord_core::events::{DiscordEvent, SourceEnvelope};
    use litecord_store::reducer::{self, ReducerConfig};
    use litecord_types::provenance::DiscordSource;
    use litecord_types::social::Message;
    use litecord_types::{ConversationId, MessageId, UserId};

    fn incoming(app: &LitecordApp, id: u64, conv: ConversationId, author: UserId, text: &str) {
        let message = Message {
            id: MessageId(id),
            conversation_id: conv,
            author_id: author,
            content: text.into(),
            sent_at: Timestamp::now(),
            edited_at: None,
            reply_to: None,
            extras: vec![],
        };
        reducer::apply(
            app.database(),
            &SourceEnvelope::new(
                DiscordSource::Synthetic,
                Timestamp::now(),
                DiscordEvent::MessageCreated { message },
            ),
            &ReducerConfig::default(),
        )
        .unwrap();
    }

    async fn dm(app: &LitecordApp) -> (ConversationId, UserId) {
        until(|| {
            app.conversations_view(50)
                .unwrap()
                .conversations
                .into_iter()
                .find_map(|c| c.recipient_id.map(|r| (c.conversation_id, r)))
        })
        .await
    }

    fn draft(name: &str, trigger: AutomationTrigger) -> AutomationDraft {
        AutomationDraft {
            name: name.into(),
            prompt: "Summarize what matters.".into(),
            trigger,
            output: AutomationOutputKind::Inbox,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scheduled_automation_runs_once_and_reports_to_inbox() {
        let h = start(
            FakeDriver::new(|p| FakeTurn {
                reply: if p.contains("Quiet one") {
                    "AUTOMATION_OK".into()
                } else {
                    "- Ada is waiting on the build".into()
                },
                ..FakeTurn::default()
            }),
            600,
        )
        .await;
        let omni = h.app.omni();
        let loud = omni
            .create_automation(&draft("Brief", AutomationTrigger::Every { hours: 1 }))
            .unwrap();
        omni.create_automation(&draft("Quiet one", AutomationTrigger::Every { hours: 1 }))
            .unwrap();
        let outcomes = omni.automations_tick().await.unwrap();
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| matches!(o, AutomationOutcome::Ran { .. }))
                .count(),
            2
        );
        let v = until(|| {
            let v = omni.view(None).unwrap();
            (v.automations.iter().all(|a| a.runs == 1)
                && v.checkins.iter().any(|c| c.source == "Brief"))
            .then_some(v)
        })
        .await;
        assert_eq!(v.checkins.len(), 1, "AUTOMATION_OK is not surfaced");
        assert_eq!(v.checkins[0].text, "- Ada is waiting on the build");
        let row = v.automations.iter().find(|a| a.id == loud).unwrap();
        assert_eq!(row.trigger_label, "every 1 h");
        let session = omni.view(row.session_id).unwrap();
        let active = session.active.unwrap();
        assert_eq!(
            (active.kind.as_str(), active.mode),
            ("automation", OmniMode::Assistant)
        );
        assert!(session.items[0].text.contains("Never send"));

        // Not due again within the hour.
        until(|| {
            omni.view(None)
                .unwrap()
                .sessions
                .iter()
                .all(|s| !s.running)
                .then_some(())
        })
        .await;
        assert!(omni
            .automations_tick()
            .await
            .unwrap()
            .iter()
            .all(|o| !matches!(o, AutomationOutcome::Ran { .. })));
        h.app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn event_triggers_match_new_messages_but_not_hidden_or_own() {
        let h = start(
            FakeDriver::new(|_| FakeTurn {
                reply: "- drafted".into(),
                ..FakeTurn::default()
            }),
            600,
        )
        .await;
        let app = &h.app;
        let omni = app.omni();
        let (conv, friend) = dm(app).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        let kw = omni
            .create_automation(&draft(
                "Launch watch",
                AutomationTrigger::Keyword {
                    keyword: "Launch".into(),
                },
            ))
            .unwrap();
        omni.create_automation(&draft(
            "From friend",
            AutomationTrigger::DirectMessageFrom { user_id: friend },
        ))
        .unwrap();

        // Nothing new: nothing runs.
        assert!(omni.automations_tick().await.unwrap().is_empty());

        incoming(app, 880_001, conv, friend, "the LAUNCH is tomorrow");
        let ran = omni.automations_tick().await.unwrap();
        assert_eq!(
            ran.iter()
                .filter(|o| matches!(o, AutomationOutcome::Ran { .. }))
                .count(),
            2,
            "{ran:?}"
        );
        let session = omni
            .automations()
            .unwrap()
            .into_iter()
            .find(|a| a.id == kw)
            .unwrap()
            .session_id;
        let first = omni.view(session).unwrap().items[0].text.clone();
        assert!(
            first.contains("880001"),
            "context names the message id: {first}"
        );
        assert!(
            !first.contains("LAUNCH is tomorrow"),
            "content is not pasted in"
        );

        // Hidden conversations never trigger automations.
        until(|| {
            omni.view(None)
                .unwrap()
                .sessions
                .iter()
                .all(|s| !s.running)
                .then_some(())
        })
        .await;
        app.set_conversation_visibility(conv, Some(litecord_types::trust::AgentVisibility::Hidden))
            .unwrap();
        incoming(app, 880_002, conv, friend, "launch again");
        assert!(omni.automations_tick().await.unwrap().is_empty());
        h.app.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hourly_budget_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = LitecordConfig {
            data_dir: dir.path().to_path_buf(),
            ..LitecordConfig::default()
        };
        cfg.omni.max_automation_runs_per_hour = 1;
        cfg.omni.heartbeat.quiet_start_hour = 0;
        cfg.omni.heartbeat.quiet_end_hour = 0;
        let app = LitecordApp::builder(cfg)
            .backend(Arc::new(MockBackend::new(fixtures::generate(
                5,
                Timestamp::now(),
            ))))
            .omni_launcher(Arc::new(FakeLauncher::new(FakeDriver::new(|_| FakeTurn {
                reply: "ok".into(),
                ..FakeTurn::default()
            }))))
            .omni_mcp_command("/usr/bin/litecord".into())
            .start()
            .await
            .unwrap();
        let omni = app.omni();
        assert!(omni
            .create_automation(&draft(" ", AutomationTrigger::Every { hours: 1 }))
            .is_err());
        assert!(omni
            .create_automation(&draft("x", AutomationTrigger::Every { hours: 0 }))
            .is_err());
        omni.create_automation(&draft("A", AutomationTrigger::Every { hours: 1 }))
            .unwrap();
        omni.create_automation(&draft("B", AutomationTrigger::Every { hours: 1 }))
            .unwrap();
        let outcomes = omni.automations_tick().await.unwrap();
        assert!(
            outcomes.contains(&AutomationOutcome::Skipped("hourly limit")),
            "{outcomes:?}"
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| matches!(o, AutomationOutcome::Ran { .. }))
                .count(),
            1
        );
        app.shutdown().await;
    }
}
