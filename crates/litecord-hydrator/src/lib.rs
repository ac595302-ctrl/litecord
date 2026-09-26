//! Litecord hydration.
//!
//! Hydration is how canonical Discord state gets *into* Litecord's store from
//! a [`litecord_core::ports::SocialBackend`]. It is deliberately **not**
//! polling: nothing in this crate periodically re-fetches everything. Instead
//! there are exactly three legitimate reasons to hydrate something:
//!
//! 1. **Event-driven targeted hydration** — a live event told us (or implied)
//!    that a specific, narrow piece of state changed
//!    ([`litecord_core::events::DiscordEvent::Invalidated`], a new guild
//!    appearing, a DM conversation getting a new message summary, ...). Only
//!    that [`scheduler::HydrationKey`]-scoped object is refetched.
//! 2. **Staleness-based reconciliation** — a background sweep
//!    ([`worker::Hydrator::reconcile`]) looks at recorded observation times
//!    and only requests objects that have actually gone stale (or were never
//!    observed, or were marked dirty by a resync). This is not "refresh
//!    everything on a timer"; each key has its own staleness policy
//!    ([`freshness::StalenessPolicy`]) and only the stale ones are requested,
//!    at [`scheduler::Priority::Background`].
//! 3. **Explicit user/agent refresh requests** — a person pulls to refresh, or
//!    an agent asks to read something before acting on it
//!    ([`worker::Hydrator::request_if_stale`]). These are the only paths that
//!    can force a fetch of something that isn't stale by the numbers.
//!
//! A full rebuild of all state after a single event is explicitly the wrong
//! design; the scheduler exists specifically so that one event never fans out
//! into hydrating everything the user has ever seen.
//!
//! ## Hydration results flow through the canonical ingest queue
//!
//! A hydration job never writes to the store directly. It fetches from the
//! backend, wraps the result in a
//! [`litecord_core::events::DiscordEvent`] snapshot variant (e.g.
//! `GuildsSnapshot`, `RelationshipsSnapshot`), stamps it with
//! [`litecord_core::events::SourceEnvelope`] and sends it into the *same*
//! bounded ingest queue
//! ([`litecord_core::bus::IngestSender`]) that live backend events use. The
//! canonical reducer downstream is the single write path for Discord state
//! either way — hydration cannot bypass it, race it, or maintain a second copy
//! of truth.
//!
//! ## Offline behavior
//!
//! When the backend reports it is offline or not connected, the hydrator
//! pauses (see [`scheduler::HydrationScheduler::pause`] /
//! [`worker::Hydrator::on_session_changed`]): it keeps whatever state it last
//! observed and stops issuing new fetches, rather than spinning through
//! retries against a backend that cannot answer. Work resumes, and state is
//! marked dirty for reconciliation, once the session is `Ready` again.
//!
//! ## Module map
//!
//! * [`scheduler`] — pure, synchronous, deterministic priority scheduling
//!   with dedup, upgrade, backoff and lazy deletion. No I/O; all time is
//!   passed in explicitly so it can be unit-tested without a runtime.
//! * [`freshness`] — the staleness model ([`freshness::Freshness`],
//!   [`freshness::StalenessPolicy`]) and the [`freshness::FreshnessStore`]
//!   port. [`freshness::InMemoryFreshnessStore`] is provided for tests and
//!   ephemeral use; a SQLite-backed store over `litecord-store`'s
//!   `sync_state` repository is expected to be added later by whoever wires
//!   this crate into the app (this crate does not depend on
//!   `litecord-store`).
//! * [`error`] — [`error::HydrationError`], convertible into
//!   [`litecord_core::Error`].
//! * [`worker`] — [`worker::Hydrator`], the async driver that pulls ready jobs
//!   off the scheduler, calls the backend, and emits envelopes.

pub mod error;
pub mod freshness;
pub mod scheduler;
pub mod worker;

pub use error::HydrationError;
pub use freshness::{Freshness, FreshnessStore, InMemoryFreshnessStore, StalenessPolicy};
pub use scheduler::{
    BackoffPolicy, FailOutcome, HydrationJob, HydrationReason, HydrationRequest,
    HydrationScheduler, Priority, RequestOutcome,
};
pub use worker::Hydrator;
