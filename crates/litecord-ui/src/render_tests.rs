#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use crate::{
    bridge::{self, Selection},
    theme,
    workspace::Workspace,
};
use discord_adapter::{fixtures, MockBackend};
use eframe::egui::{self, Pos2, Rect, Vec2};
use litecord_app::LitecordApp;
use litecord_core::config::LitecordConfig;
use litecord_layout::{Destination, LayoutProfiles};
use litecord_types::{ConversationId, Timestamp, UserId};
use tokio::time::sleep;

async fn start_app(data: discord_adapter::fixtures::DemoData) -> LitecordApp {
    LitecordApp::builder(LitecordConfig::default())
        .backend(Arc::new(MockBackend::new(data)))
        .in_memory()
        .start()
        .await
        .unwrap()
}

async fn wait_for_conversation(app: &LitecordApp) -> (ConversationId, UserId) {
    for _ in 0..300 {
        if let Some(row) = app
            .conversations_view(200)
            .unwrap()
            .conversations
            .into_iter()
            .find(|row| row.recipient_id.is_some())
        {
            let conversation_id = row.conversation_id;
            let contact_id = row.recipient_id.unwrap();
            if app
                .conversation_view(conversation_id, 200, None)
                .is_ok_and(|view| !view.messages.is_empty())
            {
                return (conversation_id, contact_id);
            }
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("demo conversation did not hydrate in time");
}

async fn wait_for_seeded_private_content(
    app: &LitecordApp,
    conversation_id: ConversationId,
    contact_id: UserId,
    message_secret: &str,
    contact_secret: &str,
) {
    for _ in 0..300 {
        let message_ready = app
            .conversation_view(conversation_id, 200, None)
            .is_ok_and(|view| {
                view.messages
                    .iter()
                    .any(|message| message.render.content == message_secret)
            });
        let contact_ready = app
            .contact_view(contact_id)
            .ok()
            .flatten()
            .is_some_and(|contact| contact.display_name == contact_secret);
        if message_ready && contact_ready {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("the selected privacy-test conversation did not hydrate");
}

fn selection(
    generation: u64,
    destination: Destination,
    conversation_id: ConversationId,
    contact_id: UserId,
) -> Selection {
    Selection {
        generation,
        destination,
        conversation: Some(conversation_id),
        contact: Some(contact_id),
        before: None,
        palette_query: String::new(),
        task: None,
        omni_session: None,
    }
}

fn raw_input(width: f32, height: f32) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, height))),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_destination_renders_wide_and_narrow_without_changing_saved_layouts() {
    let app = start_app(fixtures::generate(31, Timestamp::now())).await;
    let (conversation_id, contact_id) = wait_for_conversation(&app).await;
    let saved_before: LayoutProfiles = app.layout_profiles_view().unwrap().profiles;
    let ctx = egui::Context::default();
    theme::apply(&ctx);

    let style = ctx.global_style();
    assert_eq!(style.visuals.panel_fill, theme::SHELL);
    assert_eq!(style.visuals.widgets.inactive.bg_fill, theme::RAISED);
    assert_eq!(style.visuals.hyperlink_color, theme::PRIMARY);

    let mut workspace = Workspace::new(app.clone(), tokio::runtime::Handle::current(), ctx.clone());
    workspace.profile = saved_before.active().unwrap().clone();

    let sizes = [(1586.0, 992.0), (520.0, 640.0)];
    for (index, destination) in Destination::ALL.into_iter().enumerate() {
        let selected = selection(
            100_000 + index as u64,
            destination,
            conversation_id,
            contact_id,
        );
        workspace.selection = selected.clone();
        workspace.snapshot = Some(Arc::new(bridge::snapshot(&app, selected, None).unwrap()));

        for (width, height) in sizes {
            let _ = ctx.run_ui(raw_input(width, height), |ctx| workspace.draw(ctx));
            assert_eq!(workspace.profile, saved_before.active().unwrap().clone());
        }
    }

    assert_eq!(
        app.layout_profiles_view().unwrap().profiles,
        saved_before,
        "rendering and per-destination projection must not persist layout edits"
    );
    drop(workspace);
    app.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn privacy_mode_keeps_contact_message_and_draft_secrets_out_of_rendered_text() {
    const CONTACT_SECRET: &str = "privacy_contact_sentinel_e729";
    const MESSAGE_SECRET: &str = "privacy_message_sentinel_81d7";
    const DRAFT_SECRET: &str = "privacy_draft_sentinel_512a";
    const EDIT_SECRET: &str = "privacy_edit_sentinel_332b";

    let mut data = fixtures::generate(37, Timestamp::now());
    let conversation_id = data.conversations.first().unwrap().id;
    let contact_id = data.conversations.first().unwrap().recipient_id.unwrap();
    let contact = data
        .users
        .iter_mut()
        .find(|user| user.id == contact_id)
        .unwrap();
    contact.username = Arc::from(CONTACT_SECRET);
    contact.global_name = Some(Arc::from(CONTACT_SECRET));
    for message in data.messages.iter_mut().filter(|message| {
        message.conversation_id == conversation_id && message.author_id == contact_id
    }) {
        message.content = Arc::from(MESSAGE_SECRET);
    }

    let app = start_app(data).await;
    wait_for_seeded_private_content(
        &app,
        conversation_id,
        contact_id,
        MESSAGE_SECRET,
        CONTACT_SECRET,
    )
    .await;
    app.set_setting("privacy.enabled", serde_json::Value::Bool(true))
        .unwrap();

    let ctx = egui::Context::default();
    theme::apply(&ctx);
    let mut workspace = Workspace::new(app.clone(), tokio::runtime::Handle::current(), ctx.clone());
    workspace.profile = app
        .layout_profiles_view()
        .unwrap()
        .profiles
        .active()
        .unwrap()
        .clone();
    let selected = selection(200_000, Destination::Messages, conversation_id, contact_id);
    let snapshot = bridge::snapshot(&app, selected.clone(), None).unwrap();
    let message_id = snapshot
        .chat
        .as_ref()
        .unwrap()
        .messages
        .first()
        .unwrap()
        .message_id;
    workspace.selection = selected;
    workspace.snapshot = Some(Arc::new(snapshot));
    workspace
        .drafts
        .insert(conversation_id, DRAFT_SECRET.to_owned());
    workspace.editing_message = Some((message_id, EDIT_SECRET.to_owned()));

    assert!(workspace.private());
    let output = ctx.run_ui(raw_input(1586.0, 992.0), |ctx| workspace.draw(ctx));
    let rendered = rendered_text(&output);
    for secret in [CONTACT_SECRET, MESSAGE_SECRET, DRAFT_SECRET, EDIT_SECRET] {
        assert!(
            !rendered.contains(secret),
            "private rendering exposed {secret:?}"
        );
    }
    assert!(rendered.contains("Hidden in privacy mode"));

    drop(workspace);
    app.shutdown().await;
}

fn rendered_text(output: &egui::FullOutput) -> String {
    let mut text = String::new();
    for clipped in &output.shapes {
        collect_shape_text(&clipped.shape, &mut text);
    }
    text
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dragging_navigation_to_top_is_a_cancelable_draft() {
    let app = start_app(fixtures::generate(41, Timestamp::now())).await;
    let (conversation_id, contact_id) = wait_for_conversation(&app).await;
    let saved = app.layout_profiles_view().unwrap();
    let ctx = egui::Context::default();
    theme::apply(&ctx);
    let mut workspace = Workspace::new(app.clone(), tokio::runtime::Handle::current(), ctx.clone());
    let selected = selection(300_000, Destination::Messages, conversation_id, contact_id);
    workspace.snapshot = Some(Arc::new(
        bridge::snapshot(&app, selected.clone(), None).unwrap(),
    ));
    workspace.selection = selected;
    workspace.profile = saved.profiles.active().unwrap().clone();
    workspace.edit_original = Some(workspace.profile.clone());
    workspace
        .drafts
        .insert(conversation_id, "unsent draft".into());

    let start = egui::pos2(30.0, 65.0);
    let end = egui::pos2(700.0, 60.0);
    let events = [
        vec![egui::Event::PointerMoved(start)],
        vec![egui::Event::PointerButton {
            pos: start,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }],
        vec![egui::Event::PointerMoved(end)],
        vec![egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    ];
    for events in events {
        let mut input = raw_input(1586.0, 992.0);
        input.events = events;
        let _ = ctx.run_ui(input, |ctx| workspace.draw(ctx));
    }
    assert_ne!(
        workspace.profile,
        saved.profiles.active().unwrap().clone(),
        "pointer drag must move the panel"
    );
    let nav = workspace.profile.shell.find("nav").unwrap();
    assert!(matches!(
        nav,
        litecord_layout::LayoutNode::Panel {
            placement: litecord_layout::Placement::Top,
            ..
        }
    ));
    assert_eq!(app.layout_profiles_view().unwrap().profiles, saved.profiles);
    assert_eq!(
        workspace.drafts.get(&conversation_id).unwrap(),
        "unsent draft"
    );
    workspace.cancel_edit();
    assert_eq!(workspace.profile, saved.profiles.active().unwrap().clone());
    let edited = {
        let mut draft = workspace.profile.clone();
        draft
            .shell
            .dock("nav", "outlet", litecord_layout::Placement::Top)
            .unwrap();
        draft
    };
    workspace.edit_original = Some(workspace.profile.clone());
    workspace.profile = edited.clone();
    workspace.layout_token = saved.storage_token;
    workspace.apply_edit();
    for _ in 0..300 {
        let _ = ctx.run_ui(raw_input(1586.0, 992.0), |ui| workspace.draw(ui));
        if !workspace.busy {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert!(!workspace.busy, "layout apply must complete");
    assert!(workspace.edit_original.is_none());
    assert_eq!(
        app.layout_profiles_view()
            .unwrap()
            .profiles
            .active()
            .unwrap(),
        &edited
    );
    assert_eq!(
        workspace.drafts.get(&conversation_id).unwrap(),
        "unsent draft"
    );
    drop(workspace);
    app.shutdown().await;
}

fn collect_shape_text(shape: &egui::Shape, text: &mut String) {
    match shape {
        egui::Shape::Text(text_shape) => {
            text.push_str(&text_shape.galley.job.text);
            text.push('\n');
        }
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_shape_text(shape, text);
            }
        }
        _ => {}
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registered_shortcut_navigates_even_when_palette_query_has_no_matches() {
    let app = start_app(fixtures::generate(43, Timestamp::now())).await;
    let (conversation_id, contact_id) = wait_for_conversation(&app).await;
    let ctx = egui::Context::default();
    theme::apply(&ctx);
    let mut workspace = Workspace::new(app.clone(), tokio::runtime::Handle::current(), ctx.clone());
    let mut selected = selection(400_000, Destination::Messages, conversation_id, contact_id);
    selected.palette_query = "no_command_matches_this_sentinel".into();
    workspace.selection = selected.clone();
    workspace.snapshot = Some(Arc::new(bridge::snapshot(&app, selected, None).unwrap()));
    assert!(workspace
        .snapshot
        .as_ref()
        .unwrap()
        .palette
        .matches
        .is_empty());
    let modifiers = egui::Modifiers {
        ctrl: true,
        command: true,
        ..egui::Modifiers::NONE
    };
    let mut input = raw_input(1586.0, 992.0);
    input.modifiers = modifiers;
    input.events = vec![egui::Event::Key {
        key: egui::Key::Comma,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }];
    let _ = ctx.run_ui(input, |ui| workspace.draw(ui));
    for _ in 0..300 {
        let _ = ctx.run_ui(raw_input(1586.0, 992.0), |ui| workspace.draw(ui));
        if workspace.selection.destination == Destination::Settings {
            break;
        }
        sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(workspace.selection.destination, Destination::Settings);
    drop(workspace);
    app.shutdown().await;
}
