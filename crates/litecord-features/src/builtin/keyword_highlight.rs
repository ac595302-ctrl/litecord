//! Highlights messages containing user-configured keywords.

use crate::feature::{
    section, Feature, FeatureContext, FeatureMetadata, RenderMessage, SettingDescriptor,
    SettingKind,
};

pub const SETTING_TERMS: &str = "highlight.terms";
const BADGE: &str = "highlight";

#[derive(Debug, Default)]
pub struct KeywordHighlight;

impl Feature for KeywordHighlight {
    fn metadata(&self) -> FeatureMetadata {
        FeatureMetadata {
            id: "keyword_highlight",
            name: "Keyword Highlight",
            description: "Highlights messages that contain any of your watched terms.",
            default_enabled: true,
        }
    }

    fn settings(&self) -> Vec<SettingDescriptor> {
        vec![SettingDescriptor {
            key: SETTING_TERMS.to_string(),
            label: "Highlighted terms".to_string(),
            description:
                "Messages containing any of these words (case-insensitive) are highlighted."
                    .to_string(),
            section: section::NOTIFICATIONS.to_string(),
            kind: SettingKind::StringList { default: vec![] },
        }]
    }

    fn transform_message(&self, msg: &mut RenderMessage, ctx: &FeatureContext) {
        let terms = ctx.string_list(SETTING_TERMS);
        let content_lower = msg.content.to_lowercase();
        let matched = terms.iter().any(|term| {
            let term = term.trim().to_lowercase();
            !term.is_empty() && content_lower.contains(&term)
        });
        if matched {
            msg.highlighted = true;
            if !msg.badges.iter().any(|b| b == BADGE) {
                msg.badges.push(BADGE.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litecord_types::ids::{MessageId, UserId};
    use litecord_types::Timestamp;
    use std::collections::BTreeMap;

    fn msg(content: &str) -> RenderMessage {
        RenderMessage {
            message_id: MessageId(1),
            author_id: UserId(1),
            author_display: "Alice".into(),
            content: content.into(),
            timestamp: Timestamp(0),
            compact: false,
            highlighted: false,
            blurred: false,
            badges: vec![],
        }
    }

    fn ctx_with_terms(terms: &[&str]) -> FeatureContext {
        let mut settings = BTreeMap::new();
        settings.insert(SETTING_TERMS.to_string(), serde_json::json!(terms));
        FeatureContext { settings }
    }

    #[test]
    fn matches_case_insensitively() {
        let feature = KeywordHighlight;
        let mut m = msg("please REVIEW this PR");
        feature.transform_message(&mut m, &ctx_with_terms(&["review"]));
        assert!(m.highlighted);
        assert!(m.badges.contains(&"highlight".to_string()));
    }

    #[test]
    fn no_match_leaves_message_untouched() {
        let feature = KeywordHighlight;
        let mut m = msg("nothing interesting here");
        feature.transform_message(&mut m, &ctx_with_terms(&["urgent"]));
        assert!(!m.highlighted);
        assert!(m.badges.is_empty());
    }

    #[test]
    fn blank_terms_are_ignored() {
        let feature = KeywordHighlight;
        let mut m = msg("");
        feature.transform_message(&mut m, &ctx_with_terms(&["  ", ""]));
        assert!(!m.highlighted);
    }
}
