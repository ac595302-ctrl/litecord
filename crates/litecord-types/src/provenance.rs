//! Provenance: where a piece of knowledge came from.
//!
//! The central rule: **a model-generated conclusion must never silently become
//! a Discord fact.** Canonical social state (users, messages, guilds, ...) can
//! only be written from a [`DiscordSource`]; there is intentionally no
//! `From<Origin> for DiscordSource`. Derived memory records its
//! [`Origin`] forever, even after a user confirms it.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::entity::EntityId;
use crate::ValidationError;

/// The origin of any unified-memory object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Observed through the official Discord Social SDK (user identity).
    DiscordSocialSdk,
    /// Observed through an authorized application bot (bot identity).
    DiscordBotGateway,
    /// Typed or confirmed by the local user.
    UserProvided,
    /// Produced by deterministic local logic (heuristics, reminders, ...).
    LocalApplication,
    /// Produced by a model/agent. Never a Discord fact.
    AgentDerived,
    /// Imported from a file or another tool.
    Imported,
    /// Synthetic/demo data. Must always be labelled as such in the UI.
    Synthetic,
}

impl Origin {
    pub const ALL: [Origin; 7] = [
        Origin::DiscordSocialSdk,
        Origin::DiscordBotGateway,
        Origin::UserProvided,
        Origin::LocalApplication,
        Origin::AgentDerived,
        Origin::Imported,
        Origin::Synthetic,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Origin::DiscordSocialSdk => "discord_social_sdk",
            Origin::DiscordBotGateway => "discord_bot_gateway",
            Origin::UserProvided => "user_provided",
            Origin::LocalApplication => "local_application",
            Origin::AgentDerived => "agent_derived",
            Origin::Imported => "imported",
            Origin::Synthetic => "synthetic",
        }
    }

    pub fn parse(s: &str) -> Result<Self, ValidationError> {
        Self::ALL
            .into_iter()
            .find(|o| o.as_str() == s)
            .ok_or_else(|| ValidationError::Parse {
                what: "Origin",
                input: s.to_owned(),
            })
    }

    /// Whether this origin represents directly observed Discord data.
    pub const fn is_observed_discord(self) -> bool {
        matches!(self, Origin::DiscordSocialSdk | Origin::DiscordBotGateway)
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The sources allowed to write canonical Discord state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscordSource {
    SocialSdk,
    BotGateway,
    /// Mock/demo backend. Persisted with `Origin::Synthetic` so it can never be
    /// mistaken for real Discord content.
    Synthetic,
    /// Mock/demo *bot* source: synthetic data seen through the application
    /// bot identity (for developing the bot path without a real bot).
    SyntheticBot,
}

impl DiscordSource {
    pub const fn origin(self) -> Origin {
        match self {
            DiscordSource::SocialSdk => Origin::DiscordSocialSdk,
            DiscordSource::BotGateway => Origin::DiscordBotGateway,
            DiscordSource::Synthetic | DiscordSource::SyntheticBot => Origin::Synthetic,
        }
    }

    /// The Discord identity whose view this source represents.
    pub const fn identity(self) -> DiscordIdentity {
        match self {
            DiscordSource::SocialSdk | DiscordSource::Synthetic => DiscordIdentity::UserSocialSdk,
            DiscordSource::BotGateway | DiscordSource::SyntheticBot => {
                DiscordIdentity::ApplicationBot
            }
        }
    }
}

/// Which Discord identity observed or performs something. Bot and user
/// identities are never interchangeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DiscordIdentity {
    #[default]
    UserSocialSdk,
    ApplicationBot,
}

impl DiscordIdentity {
    pub const fn as_str(self) -> &'static str {
        match self {
            DiscordIdentity::UserSocialSdk => "user_social_sdk",
            DiscordIdentity::ApplicationBot => "application_bot",
        }
    }
}

/// A reference from derived knowledge back to what it was derived from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceRef {
    pub entity: EntityId,
    /// Optional short note ("quoted line 2", "summary segment 3").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl SourceRef {
    pub fn new(entity: EntityId) -> Self {
        Self { entity, note: None }
    }
}

/// Confidence in `[0.0, 1.0]`. Observed Discord facts are `1.0`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Confidence(f32);

impl Confidence {
    pub const CERTAIN: Confidence = Confidence(1.0);

    pub fn new(v: f32) -> Result<Self, ValidationError> {
        if v.is_finite() && (0.0..=1.0).contains(&v) {
            Ok(Confidence(v))
        } else {
            Err(ValidationError::OutOfRange {
                field: "confidence",
            })
        }
    }

    /// Clamp an arbitrary float into range (NaN becomes 0).
    pub fn clamped(v: f32) -> Self {
        if v.is_nan() {
            Confidence(0.0)
        } else {
            Confidence(v.clamp(0.0, 1.0))
        }
    }

    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Default for Confidence {
    fn default() -> Self {
        Confidence(0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_roundtrip() {
        for o in Origin::ALL {
            assert_eq!(Origin::parse(o.as_str()).unwrap(), o);
        }
        assert!(!Origin::AgentDerived.is_observed_discord());
    }

    #[test]
    fn synthetic_source_is_labelled() {
        assert_eq!(DiscordSource::Synthetic.origin(), Origin::Synthetic);
        assert_eq!(
            DiscordSource::BotGateway.identity(),
            DiscordIdentity::ApplicationBot
        );
    }

    #[test]
    fn confidence_rejects_out_of_range() {
        assert!(Confidence::new(1.5).is_err());
        assert!(Confidence::new(f32::NAN).is_err());
        assert_eq!(Confidence::clamped(2.0).get(), 1.0);
    }
}
