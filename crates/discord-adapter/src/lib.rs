//! Discord backend implementations of
//! `litecord_core::ports::SocialBackend`.
//!
//! ## The adapter boundary
//!
//! ```text
//! Discord Social SDK (native, proprietary)
//!        │  C ABI (ids + plain structs only)
//!        ▼
//! discord-ffi            — the only crate allowed unsafe/native symbols
//!        │  discord_ffi::LcUser / LcMessage / LcEvent (still not Litecord types)
//!        ▼
//! discord-adapter (this crate) — converts (convert.rs) into Litecord types,
//!        │                       implements SocialBackend for real
//!        ▼
//! litecord_core::ports::SocialBackend — the trait; nothing above this line
//!        │                              knows an SDK type exists
//!        ▼
//! hydrator / reducer / rest of Litecord
//! ```
//!
//! No Discord Social SDK type ever escapes `discord-ffi`, and no
//! `discord-ffi` type ever escapes `discord-adapter`: everything crossing
//! into the rest of the workspace is a `litecord_types` type. This crate
//! provides two [`litecord_core::ports::SocialBackend`] implementations:
//!
//! * [`mock::MockBackend`] — a fully working, deterministic, in-memory
//!   backend over synthetic data ([`fixtures`]). Always available (no
//!   feature flag), and what demo mode / most tests use.
//! * [`social_sdk::SocialSdkBackend`] — a skeleton over the real SDK, behind
//!   the `discord-social-sdk` feature. It never fakes data; see its module
//!   docs for current status.
//!
//! ## Another backend, another identity
//!
//! A future bot-gateway integration (acting as an authorized application bot
//! rather than the signed-in user) is simply another
//! [`litecord_core::ports::SocialBackend`] implementation, stamping
//! `litecord_types::provenance::DiscordSource::BotGateway` on everything it
//! produces instead of `SocialSdk`/`Synthetic`. Nothing in `litecord-core` or
//! above needs to change to add it — that is the point of the port.

pub mod bot_gateway;
pub mod convert;
pub mod fixtures;
pub mod mock;
pub mod oauth;

#[cfg(feature = "discord-social-sdk")]
pub mod social_sdk;

pub use fixtures::DemoData;
pub use mock::MockBackend;

#[cfg(feature = "discord-social-sdk")]
pub use social_sdk::SocialSdkBackend;
