# Litecord — Handoff / Progress Log

This file is the continuity record for whoever (human or model) picks up the
work next. **Update it at every commit batch.**

Source specs (not in repo): "V2 — Unified Memory and Agent Layer" (primary),
"Rust-Based Discord Social Client Hackathon Plan" (V1), UI mock PDF (visual
only). The build brief asked for a production-quality *foundation*. The
repository now has the backend foundation and an initial native UI checkpoint.
`docs/UI_INTEGRATION.md` describes the current renderer and its limits; this
log tracks the remaining work.

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
| 8 | layout model, panel registry, profile persistence APIs | done, tested |
| 9 | native UI stages 3–5 | implemented baseline; stage 6 verification in progress |

Earlier Stage 2 verification on Rust 1.98 covered the layout and app profile
tests, workspace tests, formatting, and all-feature Clippy. For this UI
checkpoint, full-workspace tests, formatting, all-feature Clippy and cargo-deny
pass locally on Windows. Six UI tests cover canonical bridging, failed sends,
private rendering, all destinations at two sizes, docking/cancel/apply with
draft preservation, and registered shortcuts independent of palette queries.
Native screenshot QA covers all nine destinations, a narrow window, and a
horizontal layout. Stage 6 remains partial: full mock fidelity, accessibility
review, and performance goals are not yet complete.
The release working-set sample was 137.82 MiB, above the proposed 50 MiB
target; see `UI_QA.md` for the sampling limits and reproducible checks.

## How to continue (prioritized)

### UI work — September 26, 2026

Stages 1 and 2 are complete: `UI_DESIGN.md` defines the visual contract,
`UI_WORKSPACE.md` defines layout/profile invariants, and `litecord-layout` plus
`litecord-app` provide the validated layout model and persisted profile APIs.
Stages 3–5 now have a working egui/eframe checkpoint:

* **Stage 3 — Messages and Friends:** canonical app snapshots flow through a
  bounded background bridge. The UI consumes app events and polls every two
  seconds for cross-process changes. Messages use variable-height virtualization
  and 200-message windows. Sending clears the draft only after success; edit
  and delete use confirmation dialogs, and message context actions go through
  app intents. Friends support canonical account/contact data, local notes, and
  per-conversation `AgentVisibility`. Privacy Mode masks presentation broadly;
  it is separate from agent visibility.
* **Stage 4 — workspace renderer:** the shell and workspace panels render from
  layout profiles. Profiles support create/rename/duplicate/delete/activate,
  with Edit/Apply/Cancel layout editing, drag-edge docking, and splitter saves.
  Sidebar and inspector panels are optional; the server strip supports top or
  bottom orientation. An always-available menu remains outside the editable
  panel tree. Ctrl/Cmd+Shift+L toggles editing and Escape cancels; the header
  exposes Apply/Cancel. `crates/litecord-ui/examples/layout_preview.rs`
  demonstrates a horizontal shell and server strip for visual QA.
* **Stage 5 — destination baseline:** Home, Messages, Friends, Servers, Voice,
  Inbox, Memory, Tasks, and Settings have initial screens with controls backed
  by current app data and supported actions. Demo mode is explicitly labelled
  `Demo · synthetic`.

The final polish batch adds capability-gated incoming-request Accept/Decline,
Remove/Block overflow actions, and Unblock on blocked rows. Relationship writes
use the typed `change_relationship` user-action service and the Action Engine;
accept/block/unblock are covered by a canonical backend-event integration test.
Registered command shortcuts run independently of palette search results.
The palette focuses its query when opened, displays shortcuts, and runs the
first available result with Enter. CI exposed a signed-out account test setup
race; the test now establishes its committed revision explicitly.

This is a functional checkpoint, not a claim of full visual fidelity to the
mock PDF. `LitecordApp::rooms_view()` supplies known lobby metadata and an
optional matching conversation ID; it is not a native channel service. Room
Messages opens that conversation in the separate Messages destination, while
Files needs backend history. Native channel messages, generative Omni replies
and transcription, device discovery, downloaded rich remote media, task
priorities/subtasks/comments, and a verified real Social SDK integration are
not implemented. Existing channel metadata can open in Discord.

**Stage 6 is ongoing:** headless GUI, screenshot, and dependency checks have
a passing baseline; full visual fidelity, accessibility and performance
review remain. The screenshot path is an optional,
hidden QA feature (`--features screenshots -- gui --screenshot PATH --screen
Friends --width 760`); graceful close and joined runtime shutdown have been
observed. The normal launch command is `cargo run -p litecord-desktop
--features gui -- gui`; headless `demo`, `mcp`, and `status` commands remain.

Local build preparation uses workspace-local Rust/LLVM-MinGW because this
host initially had no Rust compiler. No system PATH changes are required.

1. **Finish UI Stage 6** — complete the headless GUI/screenshot review and
   dependency checks; record visual or behavior gaps without overstating
   coverage.
2. **Real Social SDK backend** — vendor the SDK (`crates/discord-ffi/README.md`),
   verify/compile `native/discord_bridge.cpp`, implement
   `discord-adapter/src/social_sdk.rs` (OAuth2 PKCE, token storage via an OS
   keychain `SecretStore`, callback thread → `IngestSender::try_send`, a
   dedicated thread pumping `lc_bridge_run_callbacks`). The mock backend's
   behaviour and tests describe the expected contract.
3. **Bot adapter follow-ups** — implemented (see `docs/BOT.md`); remaining:
   verify `HttpTransport` against a real bot, sharding, interactions.
4. **LLM-backed extraction/summaries** (optional) — implement
   `CandidateExtractor` / `Summarizer` with a model; store output as
   `Origin::AgentDerived`.
5. Agent profiles (V2 §45), themes (V1 §18), media download cache.

### Backend additions for the UI — batch 10

New `LitecordApp` APIs (all tested; see `crates/litecord-app/tests/`):

| API | Purpose |
|---|---|
| `sign_in() -> AuthStep`, `complete_sign_in(redirect_url)`, `sign_out()`, `session_state()` | sign-in screen; PKCE + CSRF state in `discord-adapter::oauth`; mock supports `MockBackend::with_sign_in_required()` |
| `audio_devices()` | voice settings device pickers (empty list when unsupported) |
| `conversation_files_view(id, limit, before)` | Files tab (attachment metadata, paged, Discord links) |
| `command_palette_in / command_shortcuts_in / run_command_in(.., CommandScope)` | pass `selected_message`; enables `message.bookmark`, `message.copy_id`, `message.open_in_discord` |
| `task_detail_view(id)`, `create_task(draft)`, `set_task_priority`, `add_task_comment` | task priorities, one-level subtasks, comments (migration 0002) |

Also: unfinished hydration work persists across restarts; raw-message
retention (`retention.raw_messages_days`, bookmarks kept); weekly heuristic
conversation summaries refreshed by the maintenance task; dedicated
retriever test suite.

### Backend batch 11 — optional application-bot source

* `discord-adapter::bot::{gateway, translate, rest, time, transport, backend, http}`
* App: `AppBuilder::bot_backend`, config `backend.demo_bot`, second hydrator
  with follow-ups routed by source, `bot_session_state`, diagnostics `bot`.
* Actions: `propose_as(identity)`, identity-aware executor, bot limited to
  guild-channel messages; guild channels need a GuildMessages-capable identity.
* Views: `ConversationCapabilities.send_identity`, `PendingActionRow.identity`;
  services `send_message_as`; agent `propose_message.send_as`.

### Omni implemented + UI overhaul + Astra review (September 26, 2026)

- Omni backend: `litecord-harness` (Codex/OpenCode/fake drivers) and
  `OmniService` (see the status table at the top of `docs/AGENT_HARNESS.md`).
  The drivers have not been checked against real harness binaries.
- UI overhaul: see "September 26, 2026 update" in `docs/UI_INTEGRATION.md`.
- A GPT model ("Astra") reviewed local changes that are not in this
  repository. Each item was re-implemented on main:
  - projected resize (`resize_projected`) and the UI wiring;
  - FFI string views with lifetimes. Found in review: the native bridge
    reuses scratch buffers per query, so the queries now take `&mut self`.
  - docking preview, menu-based docking, shared cancel;
  - theme foundations;
  - Messages improvements, using the app's Files API;
  - status-aware navigation;
  - header connection status;
  - bot identity in the composer and in approvals.
- Backend: after a conversation list arrives, the 8 most recent
  conversations' history is fetched in the background if stale, so lists
  show real previews.
- Disk: `target/debug/incremental` grows past 10 GB quickly. Clear it when
  space runs low.

### Omni on Codex/OpenCode — design (September 26, 2026)

Design in [`docs/AGENT_HARNESS.md`](AGENT_HARNESS.md): the user's own
Codex/OpenCode (signed in with their own login) powers Omni as a lazily
started sidecar; tools reach Litecord through the existing MCP server;
Litecord owns heartbeats, approvals and memory. Step 1 is shipped:
`litecord_agent::prompts` (`omni.md`, `heartbeat.md`,
`mcp_instructions.md`, the last one returned from MCP `initialize`). Next is
step 2 of its build plan (driver trait + fake driver + `OmniService`).

### Audit fixes — September 26, 2026

- **Agent visibility leak (fixed):** commitment tasks and reminders carry
  message-derived text plus a `conversation_id`, but `list_tasks`,
  `discord://tasks`/`reminders` and the context compiler listed them without
  checking that conversation's visibility. They now follow
  `VisibilityPolicy::allows_content` like everything else (the retriever
  already did). Tests: `litecord-context/tests/compiler.rs`,
  `litecord-app/tests/bot.rs`.
- **Capability honesty:** the bot backends advertised `Dm*` capabilities
  only because the engine checked `DmSend`/`DmEdit`/`DmDelete` for every
  message write. The engine now requires `GuildMessages` for writes in guild
  channels and `Dm*` for DMs (`required_capability` in `engine.rs`); the
  bot no longer claims DM support, and the conversation view's
  `can_edit`/`can_delete` use the same rule.
- **Bot gateway:** resume now connects with `?v=10&encoding=json` appended
  (`rest::gateway_connect_url`); every reconnect pauses for a backoff that
  doubles until READY/RESUMED, so a flapping gateway is never hammered; the
  `Authorization` header is marked sensitive so it is redacted from Debug
  output; the User-Agent URL comes from the workspace `repository`.

### Memory footprint note (for UI performance work)

`litecord demo --in-memory` (release, headless: full runtime, SQLite, demo
hydration, memory extraction, context compile) reports **~9.5 MiB RSS** on
Linux. The ~138 MiB measured for the GUI build is therefore almost entirely
the GUI stack (egui/glow context, font atlases, window buffers) — optimize
there first (e.g. renderer choice, font subsetting, texture sizes).

## Gotchas learned the hard way

* Supersession and pending-reply expiry must follow **observation time**,
  not processing order: hydration backfills older messages after newer live
  ones (`litecord-memory/tests/service.rs` has the regressions).
* Code that emits events from "callback-like" contexts must use
  `IngestSender::try_send`; awaiting capacity can deadlock tests and SDK
  threads.
* `clippy.toml` allows unwrap only inside `#[test]` fns; integration test
  files start with `#![allow(clippy::unwrap_used, clippy::expect_used)]`.
* Worktree subagents each build their own `target/` (GBs). Delete merged
  worktrees (`git worktree remove`) or the disk fills and linking fails.
* Internally tagged serde enums cannot wrap bare strings or nest another
  internally tagged enum in a tuple variant — use struct variants.
