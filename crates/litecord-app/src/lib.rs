//! Litecord composition root.
//!
//! [`LitecordApp`] wires storage, the Discord backend, the event reactor,
//! hydration, memory, reminders and the Action Engine, and exposes
//! UI-framework-agnostic services and view models (see [`services`] and
//! [`view`]). A UI depends on this crate only.

mod app;
pub mod freshness;
pub mod people;
mod runtime;
pub mod services;
pub mod view;
pub mod workspace;

pub use app::{standalone_gateway, AppBuilder, LitecordApp};
