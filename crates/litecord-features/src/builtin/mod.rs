//! First-party, compiled-in features.

mod compact_mode;
mod core_commands;
mod keyword_highlight;
mod local_bookmarks;
mod open_in_discord;
mod privacy_mode;
mod quick_copy;

pub use compact_mode::CompactMode;
pub use core_commands::CoreCommands;
pub use keyword_highlight::KeywordHighlight;
pub use local_bookmarks::LocalBookmarks;
pub use open_in_discord::OpenInDiscord;
pub use privacy_mode::PrivacyMode;
pub use quick_copy::QuickCopy;

use crate::feature::{FeatureError, FeatureRegistry};

/// Builds the [`FeatureRegistry`] with every first-party feature installed,
/// `"core"` first.
pub fn builtin_registry() -> Result<FeatureRegistry, FeatureError> {
    let mut registry = FeatureRegistry::new();
    registry.register(Box::new(CoreCommands))?;
    registry.register(Box::new(CompactMode))?;
    registry.register(Box::new(KeywordHighlight))?;
    registry.register(Box::new(PrivacyMode))?;
    registry.register(Box::new(QuickCopy))?;
    registry.register(Box::new(LocalBookmarks))?;
    registry.register(Box::new(OpenInDiscord))?;
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandRegistry;

    #[test]
    fn builtin_registry_installs_commands_without_conflicts() {
        let registry = builtin_registry().unwrap();
        let mut commands = CommandRegistry::new();
        registry.install_commands(&mut commands).unwrap();
        // 12 from core + 1 (compact) + 1 (privacy) = 14.
        assert_eq!(commands.len(), 14);
    }

    #[test]
    fn builtin_registry_has_seven_features_core_first() {
        let registry = builtin_registry().unwrap();
        let ids: Vec<&str> = registry.metadata().iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), 7);
        assert_eq!(ids[0], "core");
    }
}
