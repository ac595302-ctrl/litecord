//! Unified-memory services.
//!
//! * [`service::MemoryService`] — records candidates with fingerprint
//!   deduplication and explicit supersession (contradictions are never
//!   overwritten), confirmation, garbage collection and retention.
//! * [`extract`] — deterministic, LLM-free candidate extraction.
//! * [`tiers`] — hot/warm/cold classification and the bounded [`tiers::HotCache`].
//! * [`operational`] — reminders (incl. conditional) and task lifecycle.
//!
//! Provenance is permanent: derived memory keeps its `Origin` even after the
//! user confirms it, and nothing here writes canonical Discord tables.

pub mod error;
pub mod extract;
pub mod operational;
pub mod service;
pub mod tiers;

pub use error::MemoryError;
pub use operational::{ReminderEngine, ReminderFired, TaskService};
pub use service::{GcReport, MemoryService, RecordOutcome, RetentionReport};
pub use tiers::{classify, HotCache};
