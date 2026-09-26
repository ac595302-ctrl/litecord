//! Core navigation and voice commands.
//!
//! This feature's id is `"core"`. It is registered like any other feature
//! (nothing in [`crate::feature::FeatureRegistry`] special-cases it and
//! [`FeatureRegistry::set_enabled`](crate::feature::FeatureRegistry::set_enabled)
//! will happily disable it), but disabling it is not meaningful: it carries
//! the app's baseline navigation and voice controls, and hiding those from
//! the command palette would leave no way back to most of the UI. The
//! application layer should not expose a way to turn it off.

use litecord_types::capability::Capability;

use crate::command::{Command, CommandError, CommandRegistry, Requirement};
use crate::feature::{Feature, FeatureMetadata, NavContribution};
use crate::intent::{AppIntent, NavTarget, VoiceIntent};

#[derive(Debug, Default)]
pub struct CoreCommands;

fn navigate(target: NavTarget) -> Vec<AppIntent> {
    vec![AppIntent::Navigate { target }]
}

impl Feature for CoreCommands {
    fn metadata(&self) -> FeatureMetadata {
        FeatureMetadata {
            id: "core",
            name: "Core Commands",
            description: "Baseline navigation and voice commands. Not meant to be disabled.",
            default_enabled: true,
        }
    }

    fn register_commands(&self, reg: &mut CommandRegistry) -> Result<(), CommandError> {
        reg.register(
            Command::new("nav.friends", "Go to Friends", |_| {
                navigate(NavTarget::Friends)
            })
            .description("Open the Friends list.")
            .category("Navigation")
            .keywords(["friends", "dms"]),
        )?;
        reg.register(
            Command::new("nav.agent_inbox", "Open Agent Inbox", |_| {
                navigate(NavTarget::AgentInbox)
            })
            .description("Review actions proposed by agents.")
            .category("Navigation")
            .keywords(["agent", "inbox", "approvals"]),
        )?;
        reg.register(
            Command::new("nav.settings", "Go to Settings", |_| {
                navigate(NavTarget::Settings { section: None })
            })
            .description("Open application settings.")
            .category("Navigation")
            .keywords(["settings", "preferences"])
            .shortcut("Ctrl+,"),
        )?;
        reg.register(
            Command::new("nav.tasks", "Go to Tasks", |_| navigate(NavTarget::Tasks))
                .description("Open your task list.")
                .category("Navigation")
                .keywords(["tasks", "todo"]),
        )?;
        reg.register(
            Command::new("nav.memory", "Go to Memory", |_| {
                navigate(NavTarget::Memory)
            })
            .description("Open unified memory.")
            .category("Navigation")
            .keywords(["memory", "notes"]),
        )?;
        reg.register(
            Command::new("nav.diagnostics", "Go to Diagnostics", |_| {
                navigate(NavTarget::Diagnostics)
            })
            .description("Open diagnostics and connection status.")
            .category("Navigation")
            .keywords(["diagnostics", "debug", "status"]),
        )?;
        reg.register(
            Command::new("voice.toggle_mute", "Toggle mute", |_| {
                vec![AppIntent::Voice {
                    intent: VoiceIntent::ToggleMute,
                }]
            })
            .description("Mute or unmute your microphone.")
            .category("Voice")
            .keywords(["mute", "microphone"])
            .shortcut("Ctrl+Shift+M")
            .requires(Requirement::Capability(Capability::Voice)),
        )?;
        reg.register(
            Command::new("voice.toggle_deafen", "Toggle deafen", |_| {
                vec![AppIntent::Voice {
                    intent: VoiceIntent::ToggleDeafen,
                }]
            })
            .description("Deafen or undeafen yourself.")
            .category("Voice")
            .keywords(["deafen", "audio"])
            .shortcut("Ctrl+Shift+D")
            .requires(Requirement::Capability(Capability::Voice)),
        )?;
        reg.register(
            Command::new("voice.leave", "Leave voice", |_| {
                vec![AppIntent::Voice {
                    intent: VoiceIntent::Leave,
                }]
            })
            .description("Disconnect from the current voice session.")
            .category("Voice")
            .keywords(["leave", "disconnect", "hang up"])
            .requires(Requirement::Capability(Capability::Voice))
            .requires(Requirement::Online),
        )?;
        // Message-scoped commands: available only when the UI passes a
        // selected message in the command scope.
        reg.register(
            Command::new("message.bookmark", "Bookmark selected message", |ctx| {
                ctx.selected_message
                    .map(|message_id| vec![AppIntent::BookmarkMessage { message_id }])
                    .unwrap_or_default()
            })
            .description("Save the selected message to your local bookmarks.")
            .category("Message")
            .keywords(["bookmark", "save", "pin"])
            .requires(Requirement::SelectedMessage),
        )?;
        reg.register(
            Command::new("message.copy_id", "Copy selected message ID", |ctx| {
                ctx.selected_message
                    .map(|id| {
                        vec![AppIntent::CopyToClipboard {
                            text: id.to_string(),
                        }]
                    })
                    .unwrap_or_default()
            })
            .description("Copy the selected message's ID.")
            .category("Message")
            .keywords(["copy", "id"])
            .requires(Requirement::SelectedMessage),
        )?;
        reg.register(
            Command::new(
                "message.open_in_discord",
                "Open selected message in Discord",
                |ctx| match (ctx.active_conversation, ctx.selected_message) {
                    (Some(c), Some(message_id)) => vec![AppIntent::OpenExternal {
                        target: litecord_types::capability::DiscordTarget::Message {
                            guild_id: None,
                            channel_id: litecord_types::ids::ChannelId(c.get()),
                            message_id,
                        },
                    }],
                    _ => Vec::new(),
                },
            )
            .description("Open the selected message in the Discord client.")
            .category("Message")
            .keywords(["discord", "external", "link"])
            .requires(Requirement::ActiveConversation)
            .requires(Requirement::SelectedMessage),
        )?;
        Ok(())
    }

    fn navigation(&self) -> Vec<NavContribution> {
        vec![
            NavContribution {
                id: "nav.friends".to_string(),
                label: "Friends".to_string(),
                target: NavTarget::Friends,
                order: 0,
            },
            NavContribution {
                id: "nav.agent_inbox".to_string(),
                label: "Agent Inbox".to_string(),
                target: NavTarget::AgentInbox,
                order: 10,
            },
            NavContribution {
                id: "nav.tasks".to_string(),
                label: "Tasks".to_string(),
                target: NavTarget::Tasks,
                order: 20,
            },
            NavContribution {
                id: "nav.memory".to_string(),
                label: "Memory".to_string(),
                target: NavTarget::Memory,
                order: 30,
            },
            NavContribution {
                id: "nav.settings".to_string(),
                label: "Settings".to_string(),
                target: NavTarget::Settings { section: None },
                order: 40,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandContext;
    use litecord_types::capability::{CapabilitySet, SupportLevel};

    #[test]
    fn registers_all_commands_without_conflict() {
        let feature = CoreCommands;
        let mut reg = CommandRegistry::new();
        feature.register_commands(&mut reg).unwrap();
        assert_eq!(reg.len(), 12);
    }

    #[test]
    fn voice_commands_require_voice_capability() {
        let feature = CoreCommands;
        let mut reg = CommandRegistry::new();
        feature.register_commands(&mut reg).unwrap();
        let ctx = CommandContext::default();
        assert!(reg.execute("voice.toggle_mute", &ctx).is_err());

        let ctx = CommandContext {
            capabilities: CapabilitySet::default().with(Capability::Voice, SupportLevel::Full),
            online: true,
            ..Default::default()
        };
        assert!(reg.execute("voice.toggle_mute", &ctx).is_ok());
        assert!(reg.execute("voice.leave", &ctx).is_ok());
    }

    #[test]
    fn navigation_entries_cover_five_targets() {
        let feature = CoreCommands;
        assert_eq!(feature.navigation().len(), 5);
    }
}
