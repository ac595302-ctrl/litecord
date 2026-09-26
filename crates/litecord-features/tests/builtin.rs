//! End-to-end tests over the built-in feature registry and command palette.

use std::collections::BTreeMap;

use litecord_features::builtin::builtin_registry;
use litecord_features::command::{CommandContext, CommandRegistry};
use litecord_features::feature::{FeatureContext, RenderMessage};
use litecord_features::intent::{AppIntent, NavTarget};
use litecord_types::ids::{MessageId, UserId};
use litecord_types::Timestamp;

fn render_message() -> RenderMessage {
    RenderMessage {
        message_id: MessageId(1),
        author_id: UserId(1),
        author_display: "Alice".to_string(),
        content: "hello world".to_string(),
        timestamp: Timestamp(0),
        compact: false,
        highlighted: false,
        blurred: false,
        badges: vec![],
    }
}

#[test]
fn builtin_registry_installs_commands_with_no_conflicts() {
    // `install_commands` bails out on the first duplicate id/shortcut it
    // finds, so a single successful call across every built-in feature is
    // itself proof there are no conflicts among them.
    let registry = builtin_registry().unwrap();
    let mut commands = CommandRegistry::new();
    registry.install_commands(&mut commands).unwrap();
    assert!(commands.len() >= 9, "expected at least core's 9 commands");
}

#[test]
fn disabled_feature_transform_is_not_applied() {
    let mut registry = builtin_registry().unwrap();
    registry.set_enabled("privacy_mode", false).unwrap();

    let mut settings = BTreeMap::new();
    settings.insert("privacy.enabled".to_string(), serde_json::json!(true));
    let ctx = FeatureContext { settings };

    let mut msg = render_message();
    registry.render(&mut msg, &ctx);

    // privacy_mode is disabled, so even though the setting says "on", the
    // message must be untouched.
    assert!(!msg.blurred);
    assert_eq!(msg.author_display, "Alice");
    assert_eq!(msg.content, "hello world");
}

#[test]
fn enabled_feature_transform_is_applied() {
    let registry = builtin_registry().unwrap();
    let mut settings = BTreeMap::new();
    settings.insert("privacy.enabled".to_string(), serde_json::json!(true));
    let ctx = FeatureContext { settings };

    let mut msg = render_message();
    registry.render(&mut msg, &ctx);

    assert!(msg.blurred);
    assert_eq!(msg.author_display, "Hidden user");
}

#[test]
fn navigation_is_sorted_by_order_and_hidden_when_disabled() {
    let mut registry = builtin_registry().unwrap();
    let nav = registry.navigation();
    assert_eq!(nav.len(), 5);
    let orders: Vec<i32> = nav.iter().map(|c| c.order).collect();
    let mut sorted = orders.clone();
    sorted.sort_unstable();
    assert_eq!(orders, sorted);

    registry.set_enabled("core", false).unwrap();
    assert!(registry.navigation().is_empty());
}

#[test]
fn executing_a_builtin_command_produces_expected_intent() {
    let registry = builtin_registry().unwrap();
    let mut commands = CommandRegistry::new();
    registry.install_commands(&mut commands).unwrap();

    let intents = commands
        .execute("nav.friends", &CommandContext::default())
        .unwrap();
    assert_eq!(
        intents,
        vec![AppIntent::Navigate {
            target: NavTarget::Friends
        }]
    );
}

/// Feature hooks are pure: calling one twice with equivalent, freshly built
/// inputs (no shared mutable state anywhere) yields identical results. This
/// is the closest a black-box test gets to proving "never mutates
/// application state" — there is no app state reachable from a hook at all,
/// only the immutable `FeatureContext`/event/message-context inputs and a
/// fresh local value to edit.
#[test]
fn feature_hooks_are_pure_functions_of_their_inputs() {
    let registry = builtin_registry().unwrap();
    let mut settings = BTreeMap::new();
    settings.insert("appearance.compact".to_string(), serde_json::json!(true));
    let ctx = FeatureContext { settings };

    let mut first = render_message();
    registry.render(&mut first, &ctx);
    let mut second = render_message();
    registry.render(&mut second, &ctx);

    assert_eq!(first, second);
}
