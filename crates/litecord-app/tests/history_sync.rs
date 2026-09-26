#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Stage C: user-selected, resumable history sync.

use std::sync::Arc;
use std::time::Duration;

use discord_adapter::{fixtures, MockBackend};
use litecord_app::LitecordApp;
use litecord_core::config::LitecordConfig;
use litecord_store::repos;
use litecord_types::{ConversationId, Timestamp};

const DEEP: ConversationId = ConversationId(5001);

fn cfg(interval_ms: u64, idle_ms: u64) -> LitecordConfig {
    let mut cfg = LitecordConfig::default();
    cfg.hydration.history_page_interval_ms = interval_ms;
    cfg.hydration.history_idle_after_ms = idle_ms;
    // Test processes run many apps at once; don't pause on RSS.
    cfg.runtime.memory_soft_limit_mb = 1 << 20;
    cfg
}

async fn start(cfg: LitecordConfig, in_memory: bool, now: Timestamp) -> LitecordApp {
    let backend = Arc::new(MockBackend::new(fixtures::generate(5, now)));
    let mut b = LitecordApp::builder(cfg).backend(backend);
    if in_memory {
        b = b.in_memory();
    }
    let app = b.start().await.unwrap();
    for _ in 0..300 {
        if app
            .database()
            .read(|r| repos::conversations::get(r, DEEP))
            .unwrap()
            .is_some()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    app
}

fn stored(app: &LitecordApp) -> i64 {
    app.database()
        .read(|r| repos::messages::count(r, Some(DEEP)))
        .unwrap()
}

async fn wait_complete(app: &LitecordApp) {
    for _ in 0..500 {
        let view = app.history_sync_view().unwrap();
        if view.row(DEEP).is_some_and(|r| r.complete) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "history sync did not complete: {:?}",
        app.history_sync_view()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_pages_the_whole_history_and_marks_it_complete() {
    let app = start(cfg(10, 0), true, Timestamp::now()).await;
    let before = stored(&app);
    app.request_history_sync(DEEP).unwrap();
    wait_complete(&app).await;
    let after = stored(&app);
    assert!(after > before, "backfill stored older messages");
    assert!(after >= 260, "all deep history is stored ({after})");
    let row = app.history_sync_view().unwrap().row(DEEP).cloned().unwrap();
    assert!(row.pages >= 3);
    assert!(row.last_error.is_none());
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_resumes_from_its_checkpoint_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let now = Timestamp::now();
    let mut slow = cfg(600_000, 0);
    slow.data_dir = dir.path().to_path_buf();
    let app = start(slow, false, now).await;
    app.request_history_sync(DEEP).unwrap();
    // One page, then a long wait: stop mid-way.
    for _ in 0..300 {
        if app
            .history_sync_view()
            .unwrap()
            .row(DEEP)
            .is_some_and(|r| r.pages >= 1)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let first = app.history_sync_view().unwrap().row(DEEP).cloned().unwrap();
    assert_eq!(first.pages, 1);
    assert!(!first.complete);
    app.shutdown().await;

    let mut fast = cfg(10, 0);
    fast.data_dir = dir.path().to_path_buf();
    let app = start(fast, false, now).await;
    wait_complete(&app).await;
    let row = app.history_sync_view().unwrap().row(DEEP).cloned().unwrap();
    assert!(row.pages > first.pages, "resumed rather than restarted");
    assert!(stored(&app) >= 260);
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_waits_while_the_ui_is_in_use_and_can_be_stopped() {
    let app = start(cfg(10, 600_000), true, Timestamp::now()).await;
    app.note_user_activity();
    app.request_history_sync(DEEP).unwrap();
    let mut paused = None;
    for _ in 0..300 {
        paused = app
            .history_sync_view()
            .unwrap()
            .row(DEEP)
            .and_then(|r| r.paused_reason.clone());
        if paused.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(paused.as_deref(), Some("waiting while you use the app"));
    let row = app.history_sync_view().unwrap().row(DEEP).cloned().unwrap();
    assert_eq!(row.pages, 0);
    app.stop_history_sync(DEEP).unwrap();
    assert!(!app.history_sync_view().unwrap().row(DEEP).unwrap().enabled);
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_is_refused_without_a_database_quota_or_conversation() {
    let mut c = cfg(10, 0);
    c.retention.max_database_mb = 0;
    let app = start(c, true, Timestamp::now()).await;
    assert!(app.request_history_sync(DEEP).is_err());
    app.shutdown().await;

    let app = start(cfg(10, 0), true, Timestamp::now()).await;
    assert!(app.request_history_sync(ConversationId(424242)).is_err());
    let view = app.history_sync_view().unwrap();
    assert!(view.quota_bytes > 0 && view.db_bytes > 0);
    app.shutdown().await;
}
