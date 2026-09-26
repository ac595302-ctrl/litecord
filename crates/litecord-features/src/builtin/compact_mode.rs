//! Toggles a denser message layout.

use crate::command::{Command, CommandError, CommandRegistry};
use crate::feature::{
    section, Feature, FeatureContext, FeatureMetadata, RenderMessage, SettingDescriptor,
    SettingKind,
};
use crate::intent::AppIntent;

pub const SETTING_COMPACT: &str = "appearance.compact";

#[derive(Debug, Default)]
pub struct CompactMode;

impl Feature for CompactMode {
    fn metadata(&self) -> FeatureMetadata {
        FeatureMetadata {
            id: "compact_mode",
            name: "Compact Mode",
            description: "Denser message layout with less vertical padding.",
            default_enabled: true,
        }
    }

    fn settings(&self) -> Vec<SettingDescriptor> {
        vec![SettingDescriptor {
            key: SETTING_COMPACT.to_string(),
            label: "Compact mode".to_string(),
            description: "Show messages with less spacing between them.".to_string(),
            section: section::APPEARANCE.to_string(),
            kind: SettingKind::Bool { default: false },
        }]
    }

    fn register_commands(&self, reg: &mut CommandRegistry) -> Result<(), CommandError> {
        reg.register(
            Command::new("appearance.toggle_compact", "Toggle compact mode", |_ctx| {
                vec![AppIntent::ToggleSetting {
                    key: SETTING_COMPACT.to_string(),
                }]
            })
            .description("Switch between compact and comfortable message spacing.")
            .category("Appearance")
            .keywords(["density", "compact"]),
        )
    }

    fn transform_message(&self, msg: &mut RenderMessage, ctx: &FeatureContext) {
        msg.compact = ctx.bool(SETTING_COMPACT, false);
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
            content: "hi".into(),
            timestamp: Timestamp(0),
            compact: false,
            highlighted: false,
            blurred: false,
            badges: vec![],
        }
    }

    #[test]
    fn transform_reads_setting() {
        let feature = CompactMode;
        let mut m = msg();
        let mut settings = BTreeMap::new();
        settings.insert(SETTING_COMPACT.to_string(), serde_json::json!(true));
        feature.transform_message(&mut m, &FeatureContext { settings });
        assert!(m.compact);
    }

    #[test]
    fn command_toggles_setting() {
        let feature = CompactMode;
        let mut reg = CommandRegistry::new();
        feature.register_commands(&mut reg).unwrap();
        let intents = reg
            .execute(
                "appearance.toggle_compact",
                &crate::command::CommandContext::default(),
            )
            .unwrap();
        assert_eq!(
            intents,
            vec![AppIntent::ToggleSetting {
                key: SETTING_COMPACT.to_string()
            }]
        );
    }
}
