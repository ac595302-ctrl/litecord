#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Interactive sign-in flow against the mock backend.

use std::sync::Arc;
use std::time::Duration;

use discord_adapter::{fixtures, MockBackend};
use litecord_app::LitecordApp;
use litecord_core::config::LitecordConfig;
use litecord_types::capability::AuthStep;
use litecord_types::social::SessionState;
use litecord_types::Timestamp;

async fn wait_for(app: &LitecordApp, want: SessionState) {
    for _ in 0..200 {
        if app.session_state().unwrap() == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "session never reached {want:?}; now {:?}",
        app.session_state().unwrap()
    );
}

fn state_param(url: &str) -> String {
    url.split('&')
        .find_map(|p| p.strip_prefix("state="))
        .unwrap()
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sign_in_hydrates_and_sign_out_keeps_local_memory() {
    let backend =
        Arc::new(MockBackend::new(fixtures::generate(9, Timestamp::now())).with_sign_in_required());
    let app = LitecordApp::builder(LitecordConfig::default())
        .backend(backend.clone())
        .in_memory()
        .start()
        .await
        .unwrap();
    wait_for(&app, SessionState::LoggedOut).await;
    assert!(
        app.friends_view().unwrap().offline.is_empty()
            && app.friends_view().unwrap().online.is_empty()
    );

    let AuthStep::OpenBrowser { url, redirect_uri } = app.sign_in().await.unwrap() else {
        panic!("expected browser step");
    };
    assert!(url.contains("code_challenge_method=S256"));
    wait_for(&app, SessionState::Authorizing).await;

    // A forged redirect is refused and does not sign in.
    assert!(app
        .complete_sign_in(&format!("{redirect_uri}?code=x&state=forged"))
        .await
        .is_err());
    // The attempt is consumed; start again and complete properly.
    let AuthStep::OpenBrowser { url, redirect_uri } = app.sign_in().await.unwrap() else {
        panic!("expected browser step");
    };
    let state = state_param(&url);
    app.complete_sign_in(&format!("{redirect_uri}?code=good&state={state}"))
        .await
        .unwrap();
    wait_for(&app, SessionState::Ready).await;

    // Hydration resumes after sign-in.
    let mut friends = 0;
    for _ in 0..200 {
        let f = app.friends_view().unwrap();
        friends = f.online.len() + f.offline.len();
        if friends > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(friends > 0);
    assert_eq!(app.sign_in().await.unwrap(), AuthStep::AlreadySignedIn);

    app.sign_out().await.unwrap();
    wait_for(&app, SessionState::LoggedOut).await;
    let f = app.friends_view().unwrap();
    assert!(
        f.online.len() + f.offline.len() > 0,
        "local memory is kept after sign-out"
    );
    assert!(app
        .send_message(
            app.conversations_view(1).unwrap().conversations[0].conversation_id,
            "hi"
        )
        .await
        .is_err());
    app.shutdown().await;
}
