//! The [`Feature`] trait and [`FeatureRegistry`] that hosts compiled-in,
//! Vencord-style features.
//!
//! A feature is a bundle of optional hooks (settings, commands, an event
//! listener, a message transform, message actions, navigation entries).
//! **Every hook must be cheap and free of side effects.** None of them may
//! write to a database, call out to Discord, or otherwise mutate application
//! state directly — they read `FeatureContext`/event/message inputs and
//! return data ([`AppIntent`]s, or a mutated *local* copy of a render-only
//! value) for the application layer to act on.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;

use litecord_core::events::ApplicationEvent;
use litecord_types::ids::{ConversationId, GuildId, MessageId, UserId};
use litecord_types::Timestamp;

use crate::command::{CommandError, CommandRegistry};
use crate::intent::{AppIntent, NavTarget};

/// Settings sections, following V1's settings layout. Kept as plain string
/// constants (rather than an enum) since [`SettingDescriptor::section`] is a
/// free-form `String` that the UI groups by.
pub mod section {
    pub const ACCOUNT: &str = "Account";
    pub const APPEARANCE: &str = "Appearance";
    pub const VOICE: &str = "Voice";
    pub const NOTIFICATIONS: &str = "Notifications";
    pub const PERFORMANCE: &str = "Performance";
    pub const PRIVACY: &str = "Privacy";
    pub const PLUGINS: &str = "Plugins";
    pub const ADVANCED: &str = "Advanced";
}

/// Static description of a feature, independent of whether it is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureMetadata {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub default_enabled: bool,
}

/// The type and default of one user-configurable setting.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SettingKind {
    Bool { default: bool },
    Text { default: String },
    StringList { default: Vec<String> },
    Number { default: f64, min: f64, max: f64 },
}

/// One entry in the settings schema.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SettingDescriptor {
    pub key: String,
    pub label: String,
    pub description: String,
    pub section: String,
    pub kind: SettingKind,
}

/// Read-only settings snapshot passed into every feature hook.
#[derive(Debug, Clone, Default)]
pub struct FeatureContext {
    pub settings: BTreeMap<String, serde_json::Value>,
}

impl FeatureContext {
    pub fn bool(&self, key: &str, default: bool) -> bool {
        self.settings
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(default)
    }

    pub fn string_list(&self, key: &str) -> Vec<String> {
        self.settings
            .get(key)
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A presentation-neutral view of a message that features may transform
/// (e.g. blur it, highlight it, add a badge) before the UI renders it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RenderMessage {
    pub message_id: MessageId,
    pub author_id: UserId,
    pub author_display: String,
    pub content: String,
    pub timestamp: Timestamp,
    pub compact: bool,
    pub highlighted: bool,
    pub blurred: bool,
    pub badges: Vec<String>,
}

/// The read-only context a message action is offered against.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageActionContext {
    pub message_id: MessageId,
    pub conversation_id: ConversationId,
    pub author_id: UserId,
    pub content: String,
    pub guild_id: Option<GuildId>,
    pub authored_by_me: bool,
}

/// One entry a feature contributes to a message's context menu.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MessageAction {
    pub id: String,
    pub label: String,
    pub group: String,
    pub intents: Vec<AppIntent>,
}

/// One entry a feature contributes to the app's primary navigation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NavContribution {
    pub id: String,
    pub label: String,
    pub target: NavTarget,
    pub order: i32,
}

/// A compiled-in feature. All methods have inert defaults so a feature only
/// implements the hooks it needs.
///
/// Every hook here must be cheap (no I/O, no blocking) and side-effect free:
/// it may only read its inputs and return data. Wanting something to
/// *happen* means returning an [`AppIntent`] (or, for [`Feature::transform_message`],
/// mutating the local [`RenderMessage`] copy the caller handed in — that copy
/// is not application state, it is the value being rendered).
pub trait Feature: Send + Sync {
    fn metadata(&self) -> FeatureMetadata;

    fn settings(&self) -> Vec<SettingDescriptor> {
        vec![]
    }

    fn register_commands(&self, _reg: &mut CommandRegistry) -> Result<(), CommandError> {
        Ok(())
    }

    fn on_event(&self, _event: &ApplicationEvent, _ctx: &FeatureContext) -> Vec<AppIntent> {
        vec![]
    }

    fn transform_message(&self, _msg: &mut RenderMessage, _ctx: &FeatureContext) {}

    fn message_actions(
        &self,
        _msg: &MessageActionContext,
        _ctx: &FeatureContext,
        _out: &mut Vec<MessageAction>,
    ) {
    }

    fn navigation(&self) -> Vec<NavContribution> {
        vec![]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FeatureError {
    #[error("feature \"{0}\" is already registered")]
    DuplicateId(String),
    #[error("feature \"{0}\" is not registered")]
    NotFound(String),
}

/// A feature's static metadata plus its current enabled state, for listing
/// in a settings UI.
#[derive(Debug, Clone, Serialize)]
pub struct FeatureInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub default_enabled: bool,
    pub enabled: bool,
}

/// Hosts every compiled-in feature and fans hooks out to the enabled ones.
///
/// Design choice: a disabled feature is *fully* inert. It contributes no
/// commands, no render transform, no message actions, no navigation entries
/// and no event handling. This keeps "disabled" behaving the way a user
/// expects (as if the feature were not compiled in) and keeps every fan-out
/// method's behavior consistent; only [`FeatureRegistry::settings_schema`]
/// looks at *all* registered features, since a disabled feature's settings
/// (including its own enabled toggle) must still be visible in the Settings
/// UI so it can be turned back on.
#[derive(Default)]
pub struct FeatureRegistry {
    features: Vec<Box<dyn Feature>>,
    enabled: BTreeMap<&'static str, bool>,
}

impl fmt::Debug for FeatureRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FeatureRegistry")
            .field(
                "features",
                &self
                    .features
                    .iter()
                    .map(|feat| feat.metadata().id)
                    .collect::<Vec<_>>(),
            )
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl FeatureRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, feature: Box<dyn Feature>) -> Result<(), FeatureError> {
        let meta = feature.metadata();
        if self.enabled.contains_key(meta.id) {
            return Err(FeatureError::DuplicateId(meta.id.to_string()));
        }
        self.enabled.insert(meta.id, meta.default_enabled);
        self.features.push(feature);
        Ok(())
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), FeatureError> {
        match self.enabled.get_mut(id) {
            Some(slot) => {
                *slot = enabled;
                Ok(())
            }
            None => Err(FeatureError::NotFound(id.to_string())),
        }
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.enabled.get(id).copied().unwrap_or(false)
    }

    /// Ids of currently enabled features, in registration order.
    pub fn enabled_ids(&self) -> Vec<&'static str> {
        self.features
            .iter()
            .map(|f| f.metadata().id)
            .filter(|id| self.is_enabled(id))
            .collect()
    }

    /// All registered features' metadata plus their current enabled state.
    pub fn metadata(&self) -> Vec<FeatureInfo> {
        self.features
            .iter()
            .map(|f| {
                let m = f.metadata();
                FeatureInfo {
                    id: m.id,
                    name: m.name,
                    description: m.description,
                    default_enabled: m.default_enabled,
                    enabled: self.is_enabled(m.id),
                }
            })
            .collect()
    }

    /// Applies `features.<id>.enabled` settings on top of each feature's
    /// `default_enabled`. A missing or non-boolean key leaves the current
    /// enabled state untouched.
    pub fn enabled_state_from_settings(&mut self, ctx: &FeatureContext) {
        let ids: Vec<&'static str> = self.features.iter().map(|f| f.metadata().id).collect();
        for id in ids {
            let key = format!("features.{id}.enabled");
            if let Some(value) = ctx.settings.get(&key) {
                if let Some(b) = value.as_bool() {
                    self.enabled.insert(id, b);
                }
            }
        }
    }

    /// Registers commands for every *enabled* feature, in registration
    /// order. See the struct-level doc for why disabled features are
    /// skipped entirely rather than registered-but-unavailable.
    pub fn install_commands(&self, reg: &mut CommandRegistry) -> Result<(), CommandError> {
        for feature in &self.features {
            if self.is_enabled(feature.metadata().id) {
                feature.register_commands(reg)?;
            }
        }
        Ok(())
    }

    /// Runs enabled features' [`Feature::transform_message`] in registration
    /// order.
    pub fn render(&self, msg: &mut RenderMessage, ctx: &FeatureContext) {
        for feature in &self.features {
            if self.is_enabled(feature.metadata().id) {
                feature.transform_message(msg, ctx);
            }
        }
    }

    /// Collects message actions from every enabled feature, in registration
    /// order.
    pub fn message_actions(
        &self,
        msg: &MessageActionContext,
        ctx: &FeatureContext,
    ) -> Vec<MessageAction> {
        let mut out = Vec::new();
        for feature in &self.features {
            if self.is_enabled(feature.metadata().id) {
                feature.message_actions(msg, ctx, &mut out);
            }
        }
        out
    }

    /// Dispatches an [`ApplicationEvent`] to every enabled feature and
    /// collects the intents they return.
    pub fn dispatch_event(&self, event: &ApplicationEvent, ctx: &FeatureContext) -> Vec<AppIntent> {
        let mut out = Vec::new();
        for feature in &self.features {
            if self.is_enabled(feature.metadata().id) {
                out.extend(feature.on_event(event, ctx));
            }
        }
        out
    }

    /// The full settings schema: every registered feature's own settings,
    /// plus an auto-generated `features.<id>.enabled` toggle per feature in
    /// the "Plugins" section. Unlike the other fan-out methods, this
    /// includes *disabled* features too, so their settings (including the
    /// toggle to re-enable them) stay visible.
    pub fn settings_schema(&self) -> Vec<SettingDescriptor> {
        let mut out = Vec::new();
        for feature in &self.features {
            out.extend(feature.settings());
        }
        for feature in &self.features {
            let m = feature.metadata();
            out.push(SettingDescriptor {
                key: format!("features.{}.enabled", m.id),
                label: format!("Enable {}", m.name),
                description: m.description.to_string(),
                section: section::PLUGINS.to_string(),
                kind: SettingKind::Bool {
                    default: m.default_enabled,
                },
            });
        }
        out
    }

    /// Navigation entries from enabled features, sorted by `order`
    /// ascending (ties keep registration order, since `sort_by_key` is
    /// stable).
    pub fn navigation(&self) -> Vec<NavContribution> {
        let mut out = Vec::new();
        for feature in &self.features {
            if self.is_enabled(feature.metadata().id) {
                out.extend(feature.navigation());
            }
        }
        out.sort_by_key(|c| c.order);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop(&'static str, bool);
    impl Feature for Noop {
        fn metadata(&self) -> FeatureMetadata {
            FeatureMetadata {
                id: self.0,
                name: self.0,
                description: "",
                default_enabled: self.1,
            }
        }
    }

    #[test]
    fn duplicate_feature_id_rejected() {
        let mut reg = FeatureRegistry::new();
        reg.register(Box::new(Noop("a", true))).unwrap();
        assert!(matches!(
            reg.register(Box::new(Noop("a", true))),
            Err(FeatureError::DuplicateId(_))
        ));
    }

    #[test]
    fn enabled_state_from_settings_overrides_default() {
        let mut reg = FeatureRegistry::new();
        reg.register(Box::new(Noop("a", true))).unwrap();
        assert!(reg.is_enabled("a"));
        let mut settings = BTreeMap::new();
        settings.insert("features.a.enabled".to_string(), serde_json::json!(false));
        reg.enabled_state_from_settings(&FeatureContext { settings });
        assert!(!reg.is_enabled("a"));
    }

    #[test]
    fn settings_schema_includes_auto_toggle_for_every_feature() {
        let mut reg = FeatureRegistry::new();
        reg.register(Box::new(Noop("a", true))).unwrap();
        reg.register(Box::new(Noop("b", false))).unwrap();
        let schema = reg.settings_schema();
        assert!(schema.iter().any(|s| s.key == "features.a.enabled"));
        assert!(schema.iter().any(|s| s.key == "features.b.enabled"));
    }

    #[test]
    fn feature_context_helpers_read_settings() {
        let mut settings = BTreeMap::new();
        settings.insert("a.flag".to_string(), serde_json::json!(true));
        settings.insert(
            "a.list".to_string(),
            serde_json::json!(["x", "y", "not-a-string-elem-becomes-skipped-if-any"]),
        );
        let ctx = FeatureContext { settings };
        assert!(ctx.bool("a.flag", false));
        assert!(!ctx.bool("missing", false));
        assert_eq!(
            ctx.string_list("a.list"),
            vec!["x", "y", "not-a-string-elem-becomes-skipped-if-any"]
        );
        assert!(ctx.string_list("missing").is_empty());
    }
}
