//! Litecord application core.
//!
//! This crate owns the cross-cutting contracts that every other layer builds
//! on, without knowing about SQLite, the Discord SDK, MCP or any UI:
//!
//! * [`error`] — the categorized, secret-free error type used across crate
//!   boundaries.
//! * [`config`] — centralized, layered configuration (defaults → file → env →
//!   CLI). No other crate reads environment variables.
//! * [`events`] — the three event vocabularies: [`events::DiscordEvent`]
//!   (normalized source events), [`events::UnifiedEvent`] (persisted, id-only)
//!   and [`events::ApplicationEvent`] (broadcast to UI/agents).
//! * [`bus`] — bounded ingest channel with a defined overflow policy, and the
//!   application broadcast bus.
//! * [`ports`] — the [`ports::SocialBackend`] trait. Discord adapters implement
//!   it; hydration and the Action Engine consume it. This is the dependency
//!   inversion point that keeps the SDK out of the domain.
//! * [`secrets`] — [`secrets::Secret`] and [`secrets::SecretStore`]. Secrets are
//!   not `Serialize` and redact themselves in `Debug`/`Display`.
//! * [`metrics`], [`clock`], [`runtime`] — observability, injectable time and
//!   owned long-lived task supervision.

pub mod bus;
pub mod clock;
pub mod config;
pub mod error;
pub mod events;
pub mod metrics;
pub mod ports;
pub mod runtime;
pub mod secrets;

pub use error::{Error, ErrorKind, Result};
