# Implementation Status

Legend: **Done** = implemented and covered by tests · **Partial** = working
base with named gaps · **Skeleton** = interface/boundary only, clearly marked
in code · **Deferred** = not started (design slot exists).

An interface existing is never counted as "implemented".

UI stage 1 (September 26, 2026): visual contract, framework direction, panel
and profile contract, and source-based screen/API inventory are documented in
`UI_DESIGN.md`, `UI_WORKSPACE.md`, and `UI_SCREEN_MATRIX.md`. This completes
design/planning only; a GUI and docking renderer remain unimplemented at this
checkpoint. Subsequent stages must update this statement as they land.

| Component | Status | Implemented | Tested | Deferred | Notes |
|---|---|---|---|---|---|
| Domain types (`litecord-types`) | Done | strong ids, Timestamp, Revision/MemorySnapshot, provenance, trust, social models, capabilities, memory/task/action types | serde round-trips, parsing, invariants | — | snowflakes serialize as strings |
| Errors & config (`litecord-core`) | Done | categorized `Error`, layered config (defaults→file→env→CLI) | precedence, validation, example-file equality | — | only place reading env vars |
| Event system & bus | Done | `DiscordEvent`→reducer→`UnifiedEvent` log→`ApplicationEvent` broadcast; bounded ingest with drop+resync overflow policy | overflow, closed channel, app e2e | — | |
| Revision model | Done | revision bump inside every write tx; no-op writes don't bump; consistent read snapshots | bump/rollback/no-op tests, pack `as_of_revision` | — | cross-process safe (stored in DB) |
| SQLite persistence & migrations | Done | WAL, FKs, normalized schema, FTS5 triggers, repository layer (all SQL in `repos`) | ~90 store tests | raw-message retention pruning | `migrations/0001_initial.sql` |
| Canonical reducer | Done | all `DiscordEvent`s incl. authoritative snapshots, stubs + follow-up hydration | 10 integration tests | — | single canonical write path |
| Discord boundary (`SocialBackend`) | Done | trait with explicit capability model, Unsupported defaults, external fallback links | via mock | — | |
| Mock/demo backend | Done | full read/write/voice/relationship behaviour, offline + failure injection, deterministic fixtures | 27 tests | — | data is `Origin::Synthetic` |
| Social SDK FFI (`discord-ffi`) | Skeleton | C ABI header, C++ bridge sketch, feature-gated linking, safe string conversion | conversions only | real SDK verification, OAuth2 PKCE, token refresh | bridge **not compiled or verified** against the SDK |
| Social SDK backend | Skeleton | returns NotConnected/empty capabilities; never fakes data | skeleton test | everything | needs SDK binaries + Discord app approval |
| Bot gateway adapter | Deferred | `DiscordSource::BotGateway`, `DiscordIdentity::ApplicationBot`, provenance plumbing | — | adapter | add as another `SocialBackend` |
| Hydration | Done | priority queue, dedup, lazy-deletion heap, bounded exponential backoff, offline pause, reconnect recovery, staleness reconciliation, SQLite freshness | 28 + app e2e | persisting the pending queue (`hydration_jobs` repo exists, unused) | |
| Unified memory service | Done | fingerprint dedup/reinforce, explicit supersession in observation order, confirm/reject, expiry + decay GC, retention for events/agent runs | 15 tests incl. backfill-ordering regressions | LLM summarization/extraction (by design later) | heuristic extractor, no AI |
| Entity graph | Partial | edges with provenance/confidence, local entities, neighbors | repo tests | topic/project extraction | only ParticipatesIn edges are auto-created |
| Hot/warm/cold tiers | Partial | tier classification, bounded `HotCache` with metrics gauge | unit tests | wiring a hot cache into views (views read SQLite directly; fast enough today) | |
| Tasks, reminders, drafts, notes, bookmarks | Done | candidate→confirmed tasks with source provenance, at + conditional reminders, drafts | repo + service tests | OS notifications (UI) | commitments stored as memory items |
| Summaries | Partial | table, repo with explicit supersession, FTS | repo tests | summary generation | needs a summarizer (LLM or heuristic) |
| Retrieval | Done | FTS5 bm25, browse mode, structured filters, visibility, explainable scores, temporal parsing | via compiler/MCP tests + unit tests | dedicated retriever test file | |
| Semantic search | Partial | `EmbeddingProvider`, deterministic `HashingEmbedder`, embeddings table | unit tests | real model provider, persisted/async embedding jobs | optional by design |
| Context compiler | Done | intent, entity resolution, relevance gate, budget, trust labels, stats, single snapshot | 8 deterministic tests | — | |
| Agent gateway & harness | Done | 24 tools + 10 resources, visibility, propose-only authority, agent-run recording, `ScriptedHarness` | via MCP tests | Codex/OpenCode in-process harnesses (use MCP instead) | |
| MCP server | Done | JSON-RPC 2.0 stdio, lifecycle, tools, resources, batching, version negotiation | 4 integration tests + binary smoke test | resource subscriptions, auth for non-stdio transports | |
| Action Engine | Done | central policy, proposals, payload-hash-bound expiring single-use MAC'd tokens, edit invalidation, revalidation, executor, audit | 7 integration + policy exhaustive tests | Administrative actions (none defined) | |
| Security & trust | Done (foundation) | trust labels, visibility, credential isolation (`Secret<T>`), prompt-injection labelling, FTS query sanitization | credential-exclusion + injection tests | OS keychain store, DB encryption at rest | see SECURITY.md |
| Feature & command system | Done | registry, 7 built-in features, fuzzy command palette backend, intents | 38 tests | themes (V1 §18), dynamic plugins (deliberately not planned) | |
| Application layer & view models | Done | runtime wiring, supervised tasks, 10 view models, services | app e2e test | agent profiles (V2 §45), Model Context Debugger view model (use `compile_context`) | |
| Desktop binary | Partial | `litecord demo`, `litecord mcp`, `litecord status` | smoke-tested | GUI (delegated to the UI model) | |
| Observability | Done | tracing spans (startup, reduce, hydrate, retrieval, context, tools, actions), metrics registry with real RSS | metrics tests | exporting metrics | message content never logged |
| CI | Done | fmt, clippy (all features), tests on Linux/macOS/Windows, cargo-deny | — | — | `.github/workflows/ci.yml` |
