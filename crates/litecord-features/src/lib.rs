//! Litecord's compiled-in, Vencord-style feature system and the backend for
//! the command palette.
//!
//! This crate holds **first-party, compiled-in features only** — there is no
//! dynamic plugin loading, no scripting, no sandboxing concern. Its central
//! rule is that a feature hook never mutates application state: every hook
//! in [`feature::Feature`] either returns data (an [`intent::AppIntent`]
//! list, a settings/command/navigation description) or edits a value that is
//! itself just being rendered (a [`feature::RenderMessage`] draft). The
//! application layer is the only place that turns those into real effects —
//! moving the view model, writing settings, or handing an
//! [`litecord_types::actions::AgentAction`] to the Action Engine.
//!
//! * [`intent`] — [`intent::AppIntent`], the vocabulary of effects a command
//!   or feature hook can ask for.
//! * [`command`] — [`command::CommandRegistry`], the command palette backend:
//!   registration, availability checks against a [`command::CommandContext`],
//!   fuzzy search and execution.
//! * [`feature`] — the [`feature::Feature`] trait and
//!   [`feature::FeatureRegistry`] that hosts compiled-in features and fans
//!   hooks out to the enabled ones.
//! * [`builtin`] — the first-party features themselves, and
//!   [`builtin::builtin_registry`] to install all of them at once.

pub mod builtin;
pub mod command;
pub mod feature;
pub mod intent;
