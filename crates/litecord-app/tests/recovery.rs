#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Unfinished hydration work survives a restart.

use std::sync::Arc;
use std::time::Duration;

use discord_adapter::{fixtures, MockBackend};
use litecord_app::LitecordApp;
use litecord_core::config::LitecordConfig;
use litecord_core::ports::BackendError;
use litecord_store::{repos, Database};
use litecord_types::Timestamp;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unfinished_hydration_is_persisted_and_restored() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = LitecordConfig {
        data_dir: dir.path().to_path_buf(),
        ..LitecordConfig::default()
    };

    // First run: every read fails (retryable), so startup work stays queued
    // in backoff when we shut down.
    let failing = Arc::new(MockBackend::new(fixtures::generate(3, Timestamp::now())));
    failing.fail_next(10_000, BackendError::Sdk("flaky".into()));
    let app = LitecordApp::builder(cfg.clone())
        .backend(failing)
        .start()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    app.shutdown().await;

    let db = Database::open(cfg.database_path(), &cfg.database).unwrap();
    let saved = db.read(|r| repos::hydration_jobs::list(r)).unwrap();
    assert!(
        saved
            .iter()
            .any(|j| j.key == litecord_core::events::HydrationKey::Relationships),
        "startup jobs persisted: {saved:?}"
    );
    drop(db);

    // Second run with a healthy backend: the queue is restored and cleared,
    // and hydration completes.
    let healthy = Arc::new(MockBackend::new(fixtures::generate(3, Timestamp::now())));
    let app = LitecordApp::builder(cfg.clone())
        .backend(healthy)
        .start()
        .await
        .unwrap();
    let remaining = app
        .database()
        .read(|r| repos::hydration_jobs::list(r))
        .unwrap();
    assert!(
        remaining.is_empty(),
        "restored jobs are cleared from the table"
    );
    let mut hydrated = false;
    for _ in 0..200 {
        if app.friends_view().unwrap().offline.len() + app.friends_view().unwrap().online.len() > 0
        {
            hydrated = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(hydrated);
    app.shutdown().await;
}
