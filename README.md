# Litecord

A native, lightweight Discord workspace written in Rust, with a first-class
**Unified Memory and Agent layer**:
local, provenance-tracked knowledge that external agents (Codex, OpenCode, …)
can query through MCP, with every Discord-side action gated by explicit user
approval.

> Discord is the source of social state. Unified Memory is the source of
> application knowledge. The model is the reasoning layer. The Action Engine
> is the only controlled path back to Discord.

## Status

The backend foundation works end to end on a deterministic demo backend,
clearly labelled as synthetic. The native egui/eframe UI now has an initial
functional checkpoint: Messages and Friends, a profile-backed workspace, and
baseline screens for all nine primary destinations. Stage 6 GUI, screenshot,
and dependency checks are ongoing; this checkpoint does not claim full visual
fidelity to the mock design. The real Social SDK integration remains a clearly
marked skeleton (the SDK is proprietary and not vendored). See
[`docs/IMPLEMENTATION_STATUS.md`](docs/IMPLEMENTATION_STATUS.md) and
[`docs/UI_INTEGRATION.md`](docs/UI_INTEGRATION.md) for current behavior and
limits, and [`docs/HANDOFF.md`](docs/HANDOFF.md) for next steps.

Part B adds an opt-in **experimental read-only account source** over Gateway
and REST, with OS credential storage, account isolation, live message
ingestion, and durable history/reconnect recovery. Its automated tests use
scripted transports; a real Discord account handshake has not been verified.
See [`docs/PART_B_ACCOUNT_PIPELINE.md`](docs/PART_B_ACCOUNT_PIPELINE.md) for
the complete pipeline, protocol limits, and submission/run instructions.

## Quick start

```sh
cargo test --workspace                       # no credentials needed
cargo run -p litecord-desktop --features gui -- gui
cargo run -p litecord-desktop --features gui,discord-user-session -- --config config/litecord.account.example.toml gui
cargo run -p litecord-desktop -- demo --in-memory  # headless synthetic demo
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
| Bot gateway adapter | implemented; optional `discord-bot` network feature |
| Read-only user-session source | experimental; scripted end-to-end tests, live protocol unverified |
| Summaries / LLM extraction | not started (non-LLM heuristics work) |

## MCP

`litecord mcp` exposes read tools, local-write tools and *proposal* tools
over stdio. Agents never get credentials, raw SQL or the power to send.
Setup for Codex/OpenCode: [`docs/MCP.md`](docs/MCP.md).

## Native UI checkpoint

Messages and Friends use canonical app snapshots through a bounded background
bridge, with event refresh and periodic cross-process polling. Workspace
profiles drive the shell and panels. Room/channel metadata can link to Discord;
native channel messages, transcription, device
discovery, and rich remote-media download are not implemented. The demo backend
is synthetic and labelled in the UI. The guide and current feature limits are in
[`docs/UI_INTEGRATION.md`](docs/UI_INTEGRATION.md).

## Development

See [`CONTRIBUTING.md`](CONTRIBUTING.md). CI runs `cargo fmt --check`,
`cargo clippy --workspace --all-targets --all-features -D warnings`,
`cargo test --workspace` (Linux/macOS/Windows) and `cargo deny`.
