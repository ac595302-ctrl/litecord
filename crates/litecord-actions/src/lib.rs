//! The Action Engine.
//!
//! ```text
//! agent/user ─► propose ─► validate ─► policy ─┬─► Execute ─────────────┐
//!                                              ├─► RequireApproval       │
//!                                              │     └─► user approves/  │
//!                                              │         edits/rejects   │
//!                                              │           └─► token ────┤
//!                                              └─► Deny                  │
//!                           revalidate against current state ◄──────────┘
//!                                 └─► executor (local repos / SocialBackend)
//!                                        └─► audit (action_history)
//! ```
//!
//! This crate is the **only** agent-controlled path toward external
//! mutation. Nothing else in Litecord calls `SocialBackend` write methods.

pub mod engine;
pub mod error;
pub mod executor;
pub mod hash;
pub mod policy;
pub mod token;

pub use engine::{ActionEngine, ActionProposer, ProposeOutcome};
pub use error::ActionError;
pub use executor::{ActionExecutor, DefaultExecutor, ExecutionOutcome};
pub use policy::{ActionPolicy, PolicyDecision};
pub use token::{ApprovalToken, TokenIssuer};
