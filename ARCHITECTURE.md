# Litecord Architecture

Litecord is a native, memory-efficient Discord social client built on
Discord's **official** Social SDK, with a first-class **unified memory and
agent layer** (V2). This document explains the crate layout, dependency
direction, data flow and the invariants that keep the system safe and
lightweight.

## The one rule

> **Discord is the source of social state. Unified Memory is the source of
> application knowledge. The model is the reasoning layer. The Action Engine is
> the only controlled path from model reasoning back to Discord.**

Consequences that the code enforces:

| Never | Enforced by |
|---|---|
| the model becomes the database | agents only see `ContextPack`s / MCP tool results compiled from SQLite |
| the UI becomes the database | UI consumes view models from `litecord-app`; state lives in `litecord-store` |
| the SDK becomes global state | SDK symbols exist only in `discord-ffi`; everything else sees `SocialBackend` |
| a UI component calls the SDK | only `litecord-actions` (writes) and `litecord-hydrator` (reads) hold a `SocialBackend` |
| an agent receives credentials | `Secret<T>` is not `Serialize`; model-facing crates never depend on a `SecretStore` |
| a model mutates Discord directly | `ActionPolicy` makes every agent Discord write `RequireApproval` or `Deny`, for every config |
| derived memory becomes a Discord fact | canonical tables are written only by the reducer from a `DiscordSource`; memory carries `Origin` forever |

## Crates and dependency direction

```text
                 litecord-types            (domain types; no I/O)
                       │
                 litecord-core             (errors, config, events, bus, ports, secrets, metrics)
          ┌────────────┼──────────────────────────────┐
          │            │                              │
   litecord-store  litecord-features            discord-ffi   (only crate with unsafe/native)
  (SQLite, repos,  (feature registry,                 │
   reducer, FTS)    commands; intents only)     discord-adapter (SocialBackend impls:
          │                                           │          mock/demo, Social SDK)
   ┌──────┼──────────────┬─────────────┐              │
   │      │              │             │              │
memory  retrieval    hydrator       actions ◄─────────┘ (via core::ports::SocialBackend)
   │      │              │             │
   └──┬───┘              │             │
   context               │             │
      │                  │             │
    agent ───────────────┼─────────────┘
      │                  │
     mcp                 │
      │                  │
      └─────── litecord-app (runtime wiring, services, view models) ──── UI (future)
                         │
                 litecord-desktop (binary: `litecord demo | mcp | status`)
```

Rules:

* Arrows point from dependent to dependency; there are no cycles.
* `discord-adapter` sits **beside** the domain: it implements
  `litecord_core::ports::SocialBackend`. Only `litecord-app` (composition
  root) and tests name it.
* `litecord-mcp` depends on `litecord-agent`, never on `discord-*`.
* `litecord-memory`/`retrieval`/`context` never depend on UI, MCP or
  model vendors.

| Crate | Responsibility |
|---|---|
| `litecord-types` | Strong ids (snowflakes vs local ids), `Timestamp`, `Revision`/`MemorySnapshot`, `Origin`/`DiscordSource`/`SourceRef`, `TrustLevel`/`AgentVisibility`, social models, capabilities, memory/task/action data types. |
| `litecord-core` | Categorized `Error`; layered config; `DiscordEvent`/`UnifiedEvent`/`ApplicationEvent`; bounded ingest queue + broadcast bus; `SocialBackend` port; `Secret`/`SecretStore`; metrics; injectable clock; `TaskSupervisor`. |
| `litecord-store` | SQLite (WAL) with committed migrations; revisioned write transactions; all SQL in `repos`; FTS5 indexes; the canonical **reducer**. |
| `litecord-memory` | Unified-memory service: dedup by fingerprint, explicit supersession, confirmation, expiry/GC, heuristic (non-LLM) candidate extraction, entity-graph edges, hot/warm/cold tiers, tasks/reminders services. |
| `litecord-hydrator` | Event-driven + staleness-based hydration: priority queue, dedup, bounded exponential backoff, offline pause, reconnect recovery. |
| `litecord-retrieval` | FTS5 lexical search + structured filters + temporal parsing + optional embeddings, with explainable scores. |
| `litecord-context` | Context compiler: request → intent/entities → retrieval → ranking → token budget → `ContextPack` (with revision and trust labels). |
| `litecord-actions` | Action Engine: centralized policy, proposals, payload-hash-bound expiring single-use approval tokens, revalidation, executor, audit. |
| `litecord-agent` | Harness-neutral agent gateway: logical tools/resources over memory and actions; `AgentHarness` trait; agent-run recording. |
| `litecord-mcp` | MCP (JSON-RPC 2.0 over stdio) adapter over the agent gateway. |
| `litecord-features` | First-party feature registry and command registry; hooks return `AppIntent`s and never mutate state. |
| `litecord-app` | Composition root and runtime: event reactor, hydration worker, reminder ticker, application services and UI view models. |
| `litecord-desktop` | Binary. Headless shell today (`demo`, `mcp`, `status`); the future GUI plugs in here or in a sibling crate. |
| `discord-ffi` | The C ABI bridge to the native Social SDK (feature-gated linking). |
| `discord-adapter` | `SocialBackend` implementations: deterministic mock/demo backend and the (skeleton) Social SDK backend; SDK→domain conversions. |

## Data flow

```text
Social SDK callback ─► discord-ffi ─► discord-adapter ─► SourceEnvelope{DiscordEvent}
                                                              │ bounded mpsc (try_send on
                                                              │ callback threads: overflow ⇒
                                                              ▼ drop + ResyncRequired)
Hydrator (snapshots) ────────────────────────────────► event reactor (litecord-app)
                                                              │
                                     reducer: ONE SQLite tx ═ ONE revision
                                     canonical rows + FTS triggers + events log
                                                              │
                             ApplicationEvent::StateChanged{revision, changes}
                   ┌───────────────┬──────────────┬───────────┴───────────┐
                   ▼               ▼              ▼                       ▼
            hydration follow-ups  memory       UI invalidation     agents / features
            (targeted)            extraction   (view models)
```

Agent path:

```text
MCP client (Codex/OpenCode/...) ─► litecord-mcp ─► litecord-agent tools
    read tools  ─► retrieval / context compiler ─► ContextPack (as_of_revision, trust labels)
    local writes ─► Action Engine (policy: auto-execute, audited)
    Discord writes ─► Action Engine (policy: proposal) ─► pending in SQLite
                                   ▼
            user approves in the app (edit ⇒ new hash) ─► ApprovalToken (hash-bound, expiring, single-use)
                                   ▼
            revalidate against current revision/state ─► executor ─► SocialBackend ─► Discord
                                   ▼
                          action_history (audit)
```

## Revisions and snapshots

* `revision_counter` is incremented inside every write transaction
  (`Database::write`). The increment rolls back with the transaction, and a
  write that changes nothing (idempotent re-observation) does not bump it.
* Rows record the revision that last wrote them; the `events` table records
  one `UnifiedEvent` per change with its revision.
* `Database::read` opens a deferred transaction; with WAL this pins a snapshot,
  so everything read in one closure is consistent with `ReadTx::revision()`.
  The context compiler runs entirely inside one read and stamps
  `ContextPack::as_of_revision`.
* Proposals record `based_on_revision`. Before execution the engine re-reads
  the current state; if the revision advanced, it re-validates targets (e.g.
  recipient still a friend and not blocked, message still exists, conversation
  still visible).

## Provenance and trust

* `Origin` on every canonical row and memory item: `discord_social_sdk`,
  `discord_bot_gateway`, `user_provided`, `local_application`,
  `agent_derived`, `imported`, `synthetic`.
* Demo/mock data is `synthetic` and must be labelled in the UI.
* `TrustLevel` in context serialization: Discord content is
  `external_discord_content` with `trusted_as_instruction: false`. Only
  `system_policy` and `user_instruction` can authorize actions.
* Bot and user identities (`DiscordIdentity`) are never merged; proposals
  record which identity would act.

## Hydration

Three triggers, never polling everything: events (targeted, immediate),
staleness sweeps (background, per-domain `stale_after`), explicit requests
(active screen / agent / user refresh). The scheduler deduplicates by
`HydrationKey`, orders by priority, backs off exponentially on failure,
pauses while offline (known state is kept), and on reconnect marks everything
dirty and re-hydrates the core set. Results re-enter through the ingest queue.

## Memory tiers

* HOT — in RAM, bounded LRU caches (active conversation window, recently used
  users). Sizes are reported as metrics.
* WARM — SQLite, served on demand (recent conversations, summaries, bookmarks).
* COLD — SQLite, excluded from default retrieval (old messages, archived runs),
  subject to retention settings.

## Concurrency model

* One ingest queue with a single consumer (the reactor) → no write races in
  canonical state; SQLite serializes writers anyway.
* Long-lived tasks are spawned only through `TaskSupervisor`, each with a
  name (owner), a cancellation token, logged errors and bounded shutdown.
* No global `Arc<Mutex<HashMap>>`: state is in SQLite; in-memory caches are
  owned by a single service.

## Error handling

Typed errors per boundary (`StoreError`, `BackendError`, `ActionError`,
`HydrationError`) convert into the categorized `litecord_core::Error`
(`ErrorKind::{Configuration, Authentication, Discord, Storage, Hydration,
Retrieval, Agent, ActionPolicy, Validation, ...}`). Error strings never
include secrets or message content.

See also: `SECURITY.md`, `docs/UI_INTEGRATION.md`, `docs/MCP.md`,
`docs/IMPLEMENTATION_STATUS.md`, `docs/HANDOFF.md`.
