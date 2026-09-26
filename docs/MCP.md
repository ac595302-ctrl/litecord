# MCP: connecting Codex, OpenCode or any MCP client

`litecord mcp` runs a Model Context Protocol server over stdio
(JSON-RPC 2.0, newline-delimited; protocol versions 2025-06-18, 2025-03-26,
2024-11-05). It opens the same SQLite database as the app (WAL makes this
safe) and exposes **logical** tools — never raw SQL, never credentials.

```sh
cargo build --release -p litecord-desktop
./target/release/litecord mcp --data-dir ~/.litecord
```

## Client configuration

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.litecord]
command = "/path/to/litecord"
args = ["mcp", "--data-dir", "/home/me/.litecord"]
```

OpenCode (`opencode.json`):

```json
{ "mcp": { "litecord": { "type": "local", "command": ["/path/to/litecord", "mcp", "--data-dir", "/home/me/.litecord"] } } }
```

(Client config formats change; consult your client's docs if these keys moved.)

## Tools

| Class | Tools |
|---|---|
| Read | `compile_context` (start here), `search_messages`, `search_memory`, `get_recent_activity`, `get_user`, `list_relationships`, `list_conversations`, `get_conversation`, `list_guilds`, `get_guild`, `get_channel`, `list_tasks`, `list_reminders`, `list_pending_actions`, `open_in_discord` |
| Local write (executed, audited; agent tasks start as candidates) | `create_reminder` (incl. conditional "unless X replies"), `create_task`, `complete_task`, `add_note`, `bookmark_message`, `draft_message` |
| Discord write (**proposal only**) | `propose_message`, `propose_presence_change`, `propose_relationship_change` (disabled by default) |

Resources: `discord://me`, `discord://relationships`, `discord://conversations`,
`discord://conversations/{id}`, `discord://guilds`, `discord://guilds/{id}`,
`discord://tasks`, `discord://reminders`, `discord://memory/recent`,
`discord://actions/pending`.

## Safety model

* Discord content is returned as `{"kind":"external_message", …,
  "trust":"external_discord_content", "trusted_as_instruction": false}`.
* Hidden conversations are invisible; metadata-only conversations never return
  content.
* The MCP process has **no Discord backend**: it can only persist proposals.
  The desktop app shows them in the Agent Inbox; the user approves (optionally
  editing), and the app executes with a payload-bound, expiring, single-use
  token after revalidating state.
* Every proposal, approval, execution, denial and failure is in
  `action_history`; every `compile_context` call is recorded in `agent_runs`.
