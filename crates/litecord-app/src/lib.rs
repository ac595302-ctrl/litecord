//! Litecord composition root.
//!
//! [`LitecordApp`] wires storage, the Discord backend, the event reactor,
//! hydration, memory, reminders and the Action Engine, and exposes
//! UI-framework-agnostic services and view models (see [`services`] and
//! [`view`]). A UI depends on this crate only.

mod account_recovery;
mod app;
pub mod automations;
pub mod freshness;
pub mod history;
pub mod omni;
pub mod people;
mod recovery;
pub mod rooms;
mod runtime;
pub mod services;
pub mod view;
pub mod workspace;

pub use app::{standalone_gateway, AppBuilder, LitecordApp};
pub use litecord_harness as harness;
pub use omni::{HeartbeatOutcome, OmniCheckin, OmniEvent, OmniService, OmniStatus, OmniViewModel};
