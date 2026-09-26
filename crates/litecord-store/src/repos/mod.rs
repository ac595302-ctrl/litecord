//! Repositories: all SQL lives here, grouped by aggregate.
//!
//! Conventions:
//! * Read functions take `&Connection` (works with both [`crate::ReadTx`] and
//!   [`crate::WriteTx`] via `Deref`).
//! * Write functions take `&WriteTx` so they run inside a revisioned
//!   transaction, stamp `tx.revision()` on rows and `tx.emit(..)` the
//!   corresponding `UnifiedEvent` **only when a row actually changed**.
//! * Upserts use `ON CONFLICT .. DO UPDATE .. WHERE <something differs>` so
//!   identical re-observations are no-ops (no revision churn).
