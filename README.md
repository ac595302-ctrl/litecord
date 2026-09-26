# Litecord

A native, lightweight Discord social client written in Rust on Discord's
**official** Social SDK, with a first-class **Unified Memory and Agent layer**:
local, provenance-tracked knowledge that external agents (Codex, OpenCode, …)
can query through MCP, with every Discord-side action gated by explicit user
approval.

> Discord is the source of social state. Unified Memory is the source of
> application knowledge. The model is the reasoning layer. The Action Engine
> is the only controlled path back to Discord.

## Status

This repository is the **backend foundation**. It works end to end on a
deterministic *demo backend* (synthetic data). The real Social SDK integration
is a clearly marked skeleton (the SDK is proprietary and not vendored), and
the GUI is intentionally left for a dedicated UI pass. See
[`docs/IMPLEMENTATION_STATUS.md`](docs/IMPLEMENTATION_STATUS.md) for an honest
matrix and [`docs/HANDOFF.md`](docs/HANDOFF.md) for how to continue.

## Quick start

```sh
cargo test --workspace                       # ~270 tests, no credentials needed
cargo run -p litecord-desktop -- demo --in-memory
cargo run -p litecord-desktop -- mcp --data-dir .litecord   # MCP server on stdio
```

Requirements: stable Rust. SQLite is bundled.

## Architecture at a glance

```text
Discord Social SDK ─► discord-ffi ─► discord-adapter ─┐   (SocialBackend port)
                                                      ▼
                         bounded ingest queue ─► reducer (1 tx = 1 revision)
                                                      ▼
            SQLite: canonical state · event log · memory · tasks · actions · FTS5
              ▲            ▲              ▲               ▲
          hydrator     memory service  retrieval ─► context compiler
                                                      ▼
                             agent gateway ─► MCP server ─► Codex / OpenCode
                                   │
                         proposals ▼
                        Action Engine ◄── user approval (UI) ─► SocialBackend ─► Discord
```

Crates, dependency direction and invariants: [`ARCHITECTURE.md`](ARCHITECTURE.md).
Security model: [`SECURITY.md`](SECURITY.md).

## What needs Discord SDK access

Everything Discord-facing goes through `SocialBackend`. With the real SDK
(feature `discord-social-sdk`, plus SDK binaries and a Discord application):
auth (OAuth2 PKCE), current user, relationships, presence, DM summaries and
recent history, sending/editing/deleting DMs, lobbies/linked channels, voice.
Arbitrary guild message history is **not** available through the SDK and is
modelled as `Unsupported` with an "Open in Discord" fallback. Today all of
this is exercised through the mock backend.

| Component | State |
|---|---|
| Mock/demo backend | complete, deterministic, labelled synthetic |
| Social SDK backend + FFI bridge | skeleton (unverified against the SDK) |
| Bot gateway adapter | not started (provenance/identity plumbing exists) |
| Summaries / LLM extraction | not started (non-LLM heuristics work) |

## MCP

`litecord mcp` exposes read tools, local-write tools and *proposal* tools
over stdio. Agents never get credentials, raw SQL or the power to send.
Setup for Codex/OpenCode: [`docs/MCP.md`](docs/MCP.md).

## Where the UI goes

A UI depends only on `litecord-app` (view models + services + event stream).
The full guide, including a screen→method map for the mock designs, is
[`docs/UI_INTEGRATION.md`](docs/UI_INTEGRATION.md).

## Development

See [`CONTRIBUTING.md`](CONTRIBUTING.md). CI runs `cargo fmt --check`,
`cargo clippy --workspace --all-targets --all-features -D warnings`,
`cargo test --workspace` (Linux/macOS/Windows) and `cargo deny`.
