//! Intents features and commands hand back to the application layer.
//!
//! **Feature hooks never mutate application state.** Every effect a command,
//! a feature's event hook, or a message action wants to have is expressed as
//! an [`AppIntent`] value. The application layer is the only place that
//! interprets these: navigation intents move the view model, setting intents
//! go through the settings store, [`AppIntent::ProposeAction`] hands an
//! [`AgentAction`] to the Action Engine (which alone may perform a Discord
//! write, and only after policy/approval), and so on. This crate has no
//! knowledge of *how* any of that happens.
//!
//! ## A note on the JSON shape
//!
//! [`NavTarget`], [`VoiceIntent`] and [`AppIntent`] are all internally tagged
//! (`#[serde(tag = "type")]`) as specified. serde cannot merge an internal tag
//! into a newtype variant whose payload does not itself serialize as a JSON
//! object (e.g. a bare id string), and it also cannot merge two internal tags
//! named `"type"` into a single object without a collision (the outer and
//! inner enum would both try to write a `"type"` key). Both situations occur
//! here: `NavTarget::Conversation`/`NavTarget::Guild` wrap a snowflake id, and
//! `AppIntent::Navigate`/`AppIntent::Voice` wrap another `tag = "type"` enum.
//! Rather than dropping the tag (which would make the wire format
//! inconsistent across variants) these are written as struct variants with a
//! single named field, e.g. `Navigate { target: NavTarget }` rather than
//! `Navigate(NavTarget)`. This keeps every variant's JSON shape as
//! `{"type": "...", ...fields}` with no ambiguity, matching how
//! `AppIntent::ProposeAction { action: AgentAction }` and
//! `AppIntent::OpenExternal { target: DiscordTarget }` already have to be
//! written for the same reason.

use serde::{Deserialize, Serialize};

use litecord_types::actions::AgentAction;
use litecord_types::capability::DiscordTarget;
use litecord_types::ids::{ConversationId, GuildId, MessageId};

/// A place in the application the UI can be navigated to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NavTarget {
    Friends,
    Conversation {
        conversation_id: ConversationId,
    },
    Guild {
        guild_id: GuildId,
    },
    AgentInbox,
    Memory,
    Tasks,
    Settings {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        section: Option<String>,
    },
    Voice,
    Diagnostics,
}

/// Requests against the local voice session. The application layer maps
/// these onto whatever the active backend's voice transport supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceIntent {
    ToggleMute,
    ToggleDeafen,
    Leave,
}

/// Something a feature hook or command wants the application layer to do.
///
/// This type is inert: constructing one has no side effect. Only code
/// outside this crate (the app layer) interprets it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppIntent {
    Navigate {
        target: NavTarget,
    },
    ToggleSetting {
        key: String,
    },
    SetSetting {
        key: String,
        value: serde_json::Value,
    },
    Voice {
        intent: VoiceIntent,
    },
    CopyToClipboard {
        text: String,
    },
    OpenExternal {
        target: DiscordTarget,
    },
    BookmarkMessage {
        message_id: MessageId,
    },
    /// Hand an action to the Action Engine. Policy decides whether this
    /// requires user approval before anything external happens.
    ProposeAction {
        action: AgentAction,
    },
    ShowNotice {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_target_serializes_tagged() {
        let v = serde_json::to_value(NavTarget::Conversation {
            conversation_id: ConversationId(7),
        })
        .unwrap();
        assert_eq!(v["type"], "conversation");
        assert_eq!(v["conversation_id"], "7");
    }

    #[test]
    fn intent_serializes_tagged_snake_case() {
        let v = serde_json::to_value(AppIntent::ToggleSetting {
            key: "appearance.compact".into(),
        })
        .unwrap();
        assert_eq!(v["type"], "toggle_setting");
        assert_eq!(v["key"], "appearance.compact");

        let v = serde_json::to_value(AppIntent::Voice {
            intent: VoiceIntent::ToggleMute,
        })
        .unwrap();
        assert_eq!(v["type"], "voice");
        assert_eq!(v["intent"]["type"], "toggle_mute");
    }

    #[test]
    fn navigate_intent_has_no_duplicate_tag_keys() {
        let v = serde_json::to_value(AppIntent::Navigate {
            target: NavTarget::Friends,
        })
        .unwrap();
        assert_eq!(v["type"], "navigate");
        assert_eq!(v["target"]["type"], "friends");
    }
}
