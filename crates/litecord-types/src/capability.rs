//! Explicit capability modelling.
//!
//! The Social SDK does not expose everything the Discord client can do.
//! Rather than faking unsupported features, every backend reports a
//! [`CapabilitySet`] and the UI/agents adapt. Unsupported content resolves to
//! [`ActionResult::OpenDiscord`] with a deep link.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::{ChannelId, ConversationId, GuildId, MessageId, UserId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum SupportLevel {
    Full,
    Partial { note: String },
    ExternalFallback,
    Unsupported,
}

impl SupportLevel {
    pub fn is_usable(&self) -> bool {
        matches!(self, SupportLevel::Full | SupportLevel::Partial { .. })
    }
}

/// Discord features a backend may or may not offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    CurrentUser,
    Friends,
    FriendRequests,
    Blocking,
    Presence,
    RichPresence,
    DmList,
    DmHistory,
    DmSend,
    DmEdit,
    DmDelete,
    GuildListing,
    GuildChannels,
    GuildMessages,
    /// Sending a message as a reply to another one.
    Replies,
    LinkedChannels,
    Lobbies,
    Voice,
    VoiceDevices,
}

/// How much history a backend can provide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryCapability {
    None,
    Recent {
        max_messages: u32,
        max_age_secs: Option<u64>,
    },
    Full,
}

/// The full capability report of a backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CapabilitySet {
    pub entries: BTreeMap<Capability, SupportLevel>,
    pub dm_history: Option<HistoryCapability>,
}

impl CapabilitySet {
    pub fn with(mut self, cap: Capability, level: SupportLevel) -> Self {
        self.entries.insert(cap, level);
        self
    }

    /// Unknown capabilities are reported as `Unsupported`, never assumed.
    pub fn support(&self, cap: Capability) -> SupportLevel {
        self.entries
            .get(&cap)
            .cloned()
            .unwrap_or(SupportLevel::Unsupported)
    }

    pub fn is_usable(&self, cap: Capability) -> bool {
        self.support(cap).is_usable()
    }
}

/// Allowed operations for an experimental account connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionAccessMode {
    #[default]
    ReadOnly,
    ReadWrite,
}

/// How the app is connected to Discord (V1 §26).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendMode {
    FullSocialSdk,
    UserSession,
    PresenceOnly,
    Demo,
    BotBridge,
}

#[cfg(test)]
mod backend_mode_tests {
    use super::BackendMode;

    #[test]
    fn user_session_backend_mode_serde_roundtrip() {
        assert_eq!(
            serde_json::to_string(&BackendMode::UserSession).unwrap(),
            "\"user_session\""
        );
        assert_eq!(
            serde_json::from_str::<BackendMode>("\"user_session\"").unwrap(),
            BackendMode::UserSession
        );
    }
}

/// Where to send the user when content is only available in Discord itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiscordTarget {
    User {
        user_id: UserId,
    },
    Conversation {
        conversation_id: ConversationId,
    },
    Channel {
        guild_id: GuildId,
        channel_id: ChannelId,
    },
    Message {
        guild_id: Option<GuildId>,
        channel_id: ChannelId,
        message_id: MessageId,
    },
}

impl DiscordTarget {
    /// Official web deep link. Opening it is always a user action.
    pub fn web_url(&self) -> String {
        match self {
            DiscordTarget::User { user_id } => format!("https://discord.com/users/{user_id}"),
            DiscordTarget::Conversation { conversation_id } => {
                format!("https://discord.com/channels/@me/{conversation_id}")
            }
            DiscordTarget::Channel {
                guild_id,
                channel_id,
            } => format!("https://discord.com/channels/{guild_id}/{channel_id}"),
            DiscordTarget::Message {
                guild_id,
                channel_id,
                message_id,
            } => match guild_id {
                Some(g) => format!("https://discord.com/channels/{g}/{channel_id}/{message_id}"),
                None => format!("https://discord.com/channels/@me/{channel_id}/{message_id}"),
            },
        }
    }
}

/// Result of an operation that may only be possible in the real client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ActionResult<T> {
    Completed {
        value: T,
    },
    OpenDiscord {
        reason: String,
        target: DiscordTarget,
    },
    Unsupported {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_capabilities_are_unsupported() {
        let caps = CapabilitySet::default().with(Capability::Friends, SupportLevel::Full);
        assert!(caps.is_usable(Capability::Friends));
        assert_eq!(caps.support(Capability::Voice), SupportLevel::Unsupported);
    }

    #[test]
    fn deep_links() {
        let t = DiscordTarget::Channel {
            guild_id: GuildId(1),
            channel_id: ChannelId(2),
        };
        assert_eq!(t.web_url(), "https://discord.com/channels/1/2");
    }
}

/// Next step of a sign-in flow, as returned by the backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum AuthStep {
    /// Credentials are already valid; nothing to do.
    AlreadySignedIn,
    /// A user-owned session credential is required (distinct from OAuth).
    SessionCredential { label: String },
    /// Open `url` in the system browser (always a user action), then pass
    /// the redirect URL Discord sends back to `complete_sign_in`.
    OpenBrowser { url: String, redirect_uri: String },
}

#[cfg(test)]
mod auth_step_tests {
    use super::AuthStep;

    #[test]
    fn session_credential_step_serde_roundtrip() {
        let step = AuthStep::SessionCredential {
            label: "Discord user session".into(),
        };
        let encoded = serde_json::to_string(&step).unwrap();
        assert_eq!(serde_json::from_str::<AuthStep>(&encoded).unwrap(), step);
        assert!(encoded.contains("session_credential"));
        assert!(encoded.contains("Discord user session"));
    }
}
