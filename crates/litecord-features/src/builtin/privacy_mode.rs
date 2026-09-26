//! Blurs message content and hides author names for streaming/screen-share.

use crate::command::{Command, CommandError, CommandRegistry};
use crate::feature::{
    section, Feature, FeatureContext, FeatureMetadata, RenderMessage, SettingDescriptor,
    SettingKind,
};
use crate::intent::AppIntent;

pub const SETTING_ENABLED: &str = "privacy.enabled";
const HIDDEN_AUTHOR: &str = "Hidden user";
const HIDDEN_CONTENT: &str = "\u{2022}\u{2022}\u{2022}"; // "•••"

#[derive(Debug, Default)]
pub struct PrivacyMode;

impl Feature for PrivacyMode {
    fn metadata(&self) -> FeatureMetadata {
        FeatureMetadata {
            id: "privacy_mode",
            name: "Privacy Mode",
            description: "Blurs message content and hides author names, e.g. while screen-sharing.",
            default_enabled: true,
        }
    }

    fn settings(&self) -> Vec<SettingDescriptor> {
        vec![SettingDescriptor {
            key: SETTING_ENABLED.to_string(),
            label: "Privacy mode".to_string(),
            description: "Hide message content and author names in the UI.".to_string(),
            section: section::PRIVACY.to_string(),
            kind: SettingKind::Bool { default: false },
        }]
    }

    fn register_commands(&self, reg: &mut CommandRegistry) -> Result<(), CommandError> {
        reg.register(
            Command::new("privacy.toggle", "Toggle privacy mode", |_ctx| {
                vec![AppIntent::ToggleSetting {
                    key: SETTING_ENABLED.to_string(),
                }]
            })
            .description("Blur message content and hide author names.")
            .category("Privacy")
            .keywords(["blur", "hide", "stream", "screen share"])
            .shortcut("Ctrl+Shift+P"),
        )
    }

    fn transform_message(&self, msg: &mut RenderMessage, ctx: &FeatureContext) {
        if ctx.bool(SETTING_ENABLED, false) {
            msg.blurred = true;
            msg.author_display = HIDDEN_AUTHOR.to_string();
            msg.content = HIDDEN_CONTENT.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litecord_types::ids::{MessageId, UserId};
    use litecord_types::Timestamp;
    use std::collections::BTreeMap;

    fn msg() -> RenderMessage {
        RenderMessage {
            message_id: MessageId(1),
            author_id: UserId(1),
            author_display: "Alice".into(),
            content: "super secret".into(),
            timestamp: Timestamp(0),
            compact: false,
            highlighted: false,
            blurred: false,
            badges: vec![],
        }
    }

    #[test]
    fn hides_author_and_content_when_enabled() {
        let feature = PrivacyMode;
        let mut m = msg();
        let mut settings = BTreeMap::new();
        settings.insert(SETTING_ENABLED.to_string(), serde_json::json!(true));
        feature.transform_message(&mut m, &FeatureContext { settings });
        assert!(m.blurred);
        assert_eq!(m.author_display, "Hidden user");
        assert_eq!(m.content, "\u{2022}\u{2022}\u{2022}");
    }

    #[test]
    fn leaves_message_alone_when_disabled() {
        let feature = PrivacyMode;
        let mut m = msg();
        feature.transform_message(&mut m, &FeatureContext::default());
        assert!(!m.blurred);
        assert_eq!(m.author_display, "Alice");
        assert_eq!(m.content, "super secret");
    }
}
