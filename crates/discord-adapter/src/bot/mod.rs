//! Protocol layer for an optional Discord **bot** adapter.
//!
//! This is a second, independent [`litecord_core::ports::SocialBackend`]
//! source (`litecord_types::provenance::DiscordSource::BotGateway`, identity
//! `DiscordIdentity::ApplicationBot`) for guilds where the user has installed
//! their *own* application bot, alongside (never instead of) the Social SDK
//! user identity. Bot and user identities are never merged: everything this
//! backend produces is stamped `BotGateway` and every write it could perform
//! would be attributed to the bot, not the signed-in user.
//!
//! Everything in this module — and its submodules [`time`], [`translate`]
//! and [`rest`] — is pure and deterministic: parsing timestamps, translating
//! gateway DISPATCH JSON into [`litecord_core::events::DiscordEvent`]s, and
//! building/parsing REST requests. No networking happens here, and the bot
//! token never appears in this layer at all — it belongs only to the
//! transport (the gateway websocket client and HTTP client) that will be
//! added separately to drive these pure functions.

pub mod rest;
pub mod time;
pub mod translate;

pub use rest::{
    channel_messages, create_message, current_user, current_user_guilds, delete_message,
    edit_message, error_for_status, gateway_bot, guild_channels, parse_channels, parse_gateway_url,
    parse_guilds, parse_messages, Method, RestRequest, API_BASE,
};
pub use time::parse_iso8601;
pub use translate::{channel, channel_conversation, dispatch, message, user, TranslateError};
