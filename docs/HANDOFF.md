# Litecord — Handoff / Progress Log

This file is the continuity record for whoever (human or model) picks up the
work next. **Update it at every commit batch.**

Source specs (not in repo): "V2 — Unified Memory and Agent Layer" (primary),
"Rust-Based Discord Social Client Hackathon Plan" (V1), UI mock PDF (visual
only). The build brief asked for a production-quality *foundation*; UI is
explicitly delegated to a later model (see `docs/UI_INTEGRATION.md` once
written).

## Architecture decisions (settled — do not relitigate without reason)

* Crate prefix `litecord-*` for domain crates; `discord-ffi` / `discord-adapter`
  for the Discord boundary. Dependency direction:
  `types → core → store → {memory, hydrator, retrieval} → context →
  {actions, agent} → mcp → app → desktop(bin)`; `discord-ffi → discord-adapter
  → core::ports::SocialBackend` (dependency inversion: the domain never imports
  the adapter).
* `SocialBackend` trait lives in `litecord-core::ports`. Optional ops default to
  `BackendError::Unsupported`; capabilities are explicit (`CapabilitySet`).
* Events: `DiscordEvent` (normalized payloads, in `SourceEnvelope` with
  `DiscordSource` provenance) → bounded ingest mpsc → reducer (one SQLite tx =
  one revision) → `UnifiedEvent`s (ids only, persisted in `events`) →
  `ApplicationEvent` broadcast. Hydration results flow through the *same*
  ingest queue, so the reducer is the single canonical write path.
* Ingest overflow policy: `try_send` from SDK callback threads drops + flags
  overflow → reactor broadcasts `ResyncRequired` and schedules reconciliation.
* Revisions: `Database::write` bumps `revision_counter` inside the tx; no-op
  writes (no row changes, no events) roll back and do not bump. Reads run in a
  deferred tx (consistent WAL snapshot) and expose `ReadTx::revision()`.
* Canonical Discord tables can only be written by the reducer from a
  `DiscordSource` (SocialSdk | BotGateway | Synthetic). Demo data is
  `Origin::Synthetic`.
* All SQL lives in `litecord-store::repos`. FTS5 external-content tables are
  kept in sync by triggers (see `migrations/0001_initial.sql`). Untrusted query
  text goes through `repos::fts::match_expr`.
* Secrets: `litecord_core::secrets::Secret<T>` (no Serialize, redacted Debug).
  Only the Discord adapter may hold a `SecretStore`.
* MCP: hand-rolled JSON-RPC 2.0 over stdio (no SDK dependency); runs as a
  separate process on the same SQLite file; can only *propose* Discord writes.
  The desktop app executes approved proposals.
* Local ids are SQLite `INTEGER` autoincrement; Discord snowflakes are `u64`
  stored bit-cast as `i64`, serialized to JSON as strings.

## Status by batch

| Batch | Scope | State |
|---|---|---|
| 1 | workspace, `litecord-types` | done, tested |
| 2 | `litecord-core` | done, tested |
| 3 | store db core, schema, migrations, FTS | done, tested |
| 4 | store repositories + reducer; discord-ffi + discord-adapter; features/commands; hydrator | done, tested |
| 5 | memory service, retrieval, Action Engine | done, tested |
| 6 | context compiler, agent gateway, MCP server | done, tested |
| 7 | app wiring, view models, `litecord` binary, docs, CI | done, tested |

Verification at the last commit: `cargo fmt --all --check` clean;
`cargo clippy --workspace --all-targets --all-features -- -D warnings` clean;
`cargo test --workspace` → 270 passed, 0 failed.

## How to continue (prioritized)

1. **UI** — follow `docs/UI_INTEGRATION.md`. Add `crates/litecord-ui`,
   depend on `litecord-app` only.
2. **Real Social SDK backend** — vendor the SDK (`crates/discord-ffi/README.md`),
   verify/compile `native/discord_bridge.cpp`, implement
   `discord-adapter/src/social_sdk.rs` (OAuth2 PKCE, token storage via an OS
   keychain `SecretStore`, callback thread → `IngestSender::try_send`, a
   dedicated thread pumping `lc_bridge_run_callbacks`). The mock backend's
   behaviour and tests describe the expected contract.
3. **Summaries** — `repos::summaries` is ready; add a summarizer behind a
   trait in `litecord-memory` (non-LLM segment stats first, LLM optional).
4. **Hydration queue persistence** — `repos::hydration_jobs` exists; persist
   `HydrationScheduler::pending_snapshot()` on shutdown and restore on start.
5. **Raw-message retention** — add `messages::prune_before` (FTS triggers
   already handle deletes) and call it from `MemoryService::apply_retention`.
6. **Bot gateway adapter** — another `SocialBackend` with
   `DiscordSource::BotGateway`; proposals already record `DiscordIdentity`.
7. Dedicated retriever tests (`crates/litecord-retrieval/tests/`), agent
   profiles (V2 §45), themes (V1 §18).

## Gotchas learned the hard way

* Supersession and pending-reply expiry must follow **observation time**,
  not processing order: hydration backfills older messages after newer live
  ones (`litecord-memory/tests/service.rs` has the regressions).
* Code that emits events from "callback-like" contexts must use
  `IngestSender::try_send`; awaiting capacity can deadlock tests and SDK
  threads.
* `clippy.toml` allows unwrap only inside `#[test]` fns; integration test
  files start with `#![allow(clippy::unwrap_used, clippy::expect_used)]`.
* Internally tagged serde enums cannot wrap bare strings or nest another
  internally tagged enum in a tuple variant — use struct variants.
