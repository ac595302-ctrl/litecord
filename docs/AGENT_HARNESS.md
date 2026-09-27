# Omni on Codex / OpenCode — harness integration design

Status (September 27, 2026): **backend implemented; sign-in verified
against real binaries (codex-cli 0.157.1, opencode 1.18.32); UI in
progress.**

| Piece | Where | Verified by |
|---|---|---|
| Driver trait, types, env allowlist, scripted fake | `crates/litecord-harness` | unit tests |
| Codex `app-server` driver (feature `codex`) | `litecord-harness/src/codex.rs` | scripted JSON-RPC peer tests; real binary |
| OpenCode `serve` driver (feature `opencode`) | `litecord-harness/src/opencode.rs` | fake HTTP/SSE server tests; real binary |
| `OmniService`: lazy sidecar, idle stop, sessions, capped transcripts, approval bridge by mode, "remember" into memory, heartbeats | `litecord-app/src/omni.rs`, `migrations/0003_omni.sql` | `litecord-app/tests/omni.rs` |
| Desktop wiring (launchers, `litecord mcp --harness`) | `litecord-desktop` | builds |
| Every sign-in method (browser, device code, pasted code, API key passed straight to the harness), cancel/retry, sign-out, model list and choice | trait defaults in `driver.rs`; `codex.rs`, `opencode.rs`, `OmniService` | driver tests; `tests/omni.rs`; `tests/real_harness.rs` (opt-in) |
| Automations: daily / every N hours / DM from a person / keyword; output to Inbox, suggested tasks or drafts; `AUTOMATION_OK`; hourly budget; Assistant mode only | `litecord-app/src/automations.rs`, `0004_omni_automations.sql`, `prompts/automation.md` | unit + `tests/omni.rs` |
| CLI: `litecord omni status, harness, login, logout, models, automations, run, doctor` | `litecord-desktop` | run against the demo harness and the real binaries |

`litecord omni doctor` checks the installed harness: its version, whether
every method, payload field or endpoint Litecord uses is in that version's
schema (Codex `generate-json-schema`, OpenCode `GET /doc`), and sign-in. The
sign-in paths run against the real binaries with
`LITECORD_REAL_HARNESS=1 cargo test -p litecord-harness --features
codex,opencode --test real_harness -- --ignored --test-threads=1` (isolated
`CODEX_HOME`/`XDG_*` homes, a dummy API key, no model calls). Known gaps:
- Completing a ChatGPT (or any provider) OAuth sign-in needs a person and
  a real account, so only the start (auth URL, localhost callback bound),
  cancel and retry are exercised automatically.
- Device-code flows (Codex `chatgptDeviceCode`, OpenCode's ChatGPT
  "headless" and GitHub Copilot) follow the schema but could not be run
  where this was verified (the auth hosts were unreachable).
- OpenCode cannot cancel a pending OAuth sign-in; `OmniService` restarts the
  sidecar instead (a retry for the same provider could otherwise fail with
  "port in use").
- Codex has no confirmed per-thread switch to turn off its shell tool.
  Assistant mode therefore relies on `sandbox: read-only`,
  `approvalPolicy: untrusted`, and Litecord declining every command request.
- A "safe" read-only command Codex runs without asking could still read local
  files.

## 1. Goal

Omni, the in-app assistant, is powered by the user's own **Codex** or
**OpenCode** install, signed in with that harness's own login (ChatGPT for
Codex; any OpenCode provider). The harness supplies the agent machinery:
- the model loop,
- compaction,
- subagents,
- plans and todos,
- shell, files and computer-use tools,
- model choice.

Litecord supplies what only it has: Discord state, unified memory, approvals
and scheduled check-ins (heartbeats).

Constraints:
- **Light.** No harness is bundled, so the binary does not grow by the size
  of a harness. Nothing runs until Omni is used. The whole integration is one
  small crate that adds no heavy dependencies.
- **Nothing breaks.** It is additive and feature-gated. The app, the MCP
  server and all current invariants behave the same when no harness is
  installed.
- **Full harness functionality, not a reimplementation.** Litecord drives
  the harness's features through the harness's own protocol and shows them in
  the UI. It does not copy them.

Non-goals:
- running inference inside Litecord;
- storing model-provider credentials;
- the harness features that don't fit a chat client (§6, "Excluded").

## 2. Architecture

```
┌──────────── Litecord app (one process) ────────────┐
│ UI ─ services ─ OmniService ─ HarnessDriver ───────┼──► harness sidecar
│                    │   ▲         (codex | opencode)│     codex app-server   (stdio JSON-RPC)
│  HeartbeatScheduler┘   │ ephemeral stream (deltas) │     opencode serve     (HTTP + SSE, loopback)
│  ActionEngine ◄─ approvals ◄─ harness requests     │            │
│  SQLite (WAL) ◄────────────────────────────────────┼── litecord mcp (spawned by the harness,
└────────────────────────────────────────────────────┘    stdio; same gateway and policy as today)
```

- **One trait, two drivers.** `HarnessDriver` covers:
  - `login_status`, `begin_login`, `logout`
  - `start_session`, `resume_session`, `send`, `steer`, `interrupt`, `compact`, `fork`
  - `answer_request`, `events()`, `shutdown`

  `CodexDriver` speaks app-server JSON-RPC over the child's stdio.
  `OpenCodeDriver` speaks HTTP to `opencode serve` on `127.0.0.1` with a
  random port and password, and reads its SSE event stream.
- **Tools reach Litecord through MCP, the path that exists today.** The
  driver registers `litecord mcp --harness <codex|opencode> --session <id>`
  as an MCP server in that session's config:
  - Codex: the per-thread `config` override `mcp_servers.litecord`.
  - OpenCode: `OPENCODE_CONFIG_CONTENT` at spawn, or `POST /mcp`.

  The agent therefore gets exactly the tool surface, visibility rules and
  proposal-only Discord writes that exist today. `--session` tags the audit
  trail and `agent_runs` with the Omni session.
- **Optional later step: in-process tools for Codex.** Codex `dynamicTools`
  (`item/tool/call`) could route straight to the in-process `AgentGateway`.
  That removes the extra process and the cross-process refresh delay.
  OpenCode keeps using the MCP server.
- **Discord state never goes to the harness except through tools.** The
  harness sees only what the tools return.

## 3. Sign-in (the harness's own login)

Litecord never sees or stores provider credentials. They stay in the
harness's own store (`~/.codex`, OpenCode's auth file). Litecord records only
which harness and model the user picked and whether they are signed in.

| Harness | Detect | Status | Sign in | Sign out |
|---|---|---|---|---|
| Codex | `codex` on `PATH`, then the usual npm/pnpm/bun/Homebrew/nvm bin dirs (or a path set in config). The npm shim is bypassed: the native binary in `@openai/codex`'s platform package is started directly, so `node` need not be on the app's `PATH`. | `account/read` (`account: null` with `requiresOpenaiAuth: false` counts as ready) | `account/login/start {type:"chatgpt"}` returns `authUrl` + `loginId`; open it, Codex listens on `localhost:1455` (or the next free port) and reports `account/login/completed`. Completions for another `loginId` (Codex reports the superseded attempt as "Login cancelled" before answering a retry) are ignored. `chatgptDeviceCode` and `apiKey` (stored in `CODEX_HOME/auth.json`) are the other methods; `account/login/cancel` frees the port. | `account/logout` (removes `auth.json`) |
| OpenCode | `opencode` on `PATH` and the same dirs plus `~/.opencode/bin`; on Windows the npm `.cmd` shim is bypassed for `node_modules/opencode-ai/bin/opencode.exe`. `GET /global/health` | `GET /provider` (`connected`, `source`); its own free provider is always connected | `GET /provider/auth` lists plugin methods (ChatGPT browser/headless, GitHub Copilot, GitLab, xAI, ...) with optional `prompts`; every other provider in `GET /provider` takes an API key, as in `opencode auth login`. OAuth: `POST /provider/{id}/oauth/authorize {method, inputs}` returns a URL, then `POST /provider/{id}/oauth/callback`. API keys: `PUT /auth/{id} {type:"api", key, metadata}`. After any credential change `POST /instance/dispose`, or OpenCode keeps the old provider list. | `DELETE /auth/{id}` for stored credentials (env/config ones stay) |

Litecord keeps no copy of any key or token. `GET /provider` and
`GET /config/providers` include stored keys in their bodies, so the driver
never logs response bodies; errors quote only the server's message with the
submitted key removed.

For OpenCode, whether a provider's subscription can be used from a
third-party app is that provider's policy. The UI lists the providers the
harness reports and links to the harness's docs; it makes no claims of its
own.

Settings UI states: not installed → installed, signed out → signing in → ready
(shows model and, for Codex, rate limits via `account/rateLimits/read`).

`OmniService` API for the UI (all in `litecord-app/src/omni.rs`):

| Step | Call | Notes |
|---|---|---|
| Detect / re-detect installs | `status().harnesses` (`installed`, `path`, `install_hint`); `refresh_installed()` after the user installs one | Detection is cached 5 s; `select()` re-detects too. No restart needed. |
| List methods | `login_options()`, then `status().login_groups()` (or `omni::login_groups(&options)`) | Group heading `LoginGroup::label`, entries `LoginOption::method_label`; show `featured` or `connected` groups first, the rest under "More providers". |
| Browser sign-in | `sign_in_with(id)` / `sign_in_with_inputs(id, &inputs)` → `SigningIn { url, instructions, needs_code }` | Open `url`; show `instructions` (device codes). Retrying cancels the earlier attempt (restarting OpenCode if needed). |
| Pasted code | `submit_login_code(code)` when `needs_code` | |
| API key | `sign_in_api_key(id, &Secret)` / `sign_in_api_key_with(id, &Secret, &inputs)` | Extra fields come from `LoginOption::prompts` (show a prompt only when `when.applies(&inputs)`). |
| Completion | `subscribe()` → `OmniEvent::Changed`, then `status().login` | `SigningIn` → `Ready { account }` or `Error { message }`. |
| Cancel | `cancel_sign_in()` | Back to the harness's state. |
| Sign out | `sign_out()`; OpenCode per provider: `sign_out_provider(provider)` | Read `status().login` after: OpenCode can stay `Ready` through env-configured or free providers. |

## 4. Process model and performance

| Concern | Decision |
|---|---|
| Idle cost | Zero. No sidecar until Omni is opened or a heartbeat fires. |
| Sidecars | At most one, for the selected harness. Stopped after 10 min idle (configurable). Restarted on demand, and the session is resumed (§6). |
| Binary size | Target under 300 KB added. Reuses tokio `process`, `serde_json`, and `reqwest` (already optional) behind `harness-codex` / `harness-opencode` features. The SSE parser is hand-rolled (about 60 lines). No HTTP server and no websocket in Litecord. |
| Token streaming | Deltas travel on an **ephemeral broadcast channel**, coalesced to ≤30 Hz for the UI. They are never written to SQLite and never create a revision. Only completed items are stored, in a capped transcript (§7). |
| Harness memory | Belongs to the harness (Codex is Rust; OpenCode runs on Bun). Shown in Diagnostics and bounded by the idle shutdown. The docs say plainly that this cost is outside Litecord. |
| Context size | The system prompt is about 650 tokens (tested: `prompts_stay_small`). No context is pre-loaded; the model calls `compile_context`. Heartbeats send a content-free delta (§8). |
| Environment | The child environment is cleared, then only an allowlist is added back: `PATH`, `HOME`, locale, proxy variables, and the harness's own variables. `LITECORD_BOT_TOKEN` and every other Litecord secret never reach the child or its tools. |

## 5. Modes (what Omni may touch)

The user picks the mode per session. The default is **Assistant**.

| Mode | Built-in harness tools | Scope |
|---|---|---|
| **Assistant** (default) | Off: Litecord tools only | Nothing outside Litecord memory |
| **Workspace** | Shell and files, sandboxed | Codex `sandbox: workspace-write` in `$DATA/omni-workspace`, no network unless enabled. OpenCode: per-agent `permission` set to `ask` for edit/bash and `deny` for external directories |
| **Computer use** | Whatever computer-use MCP servers the user has configured in the harness | Per session, with a visible "Omni is controlling your computer" banner and a stop button (`turn/interrupt` / `abort`) |

Nuances, stated plainly in the UI and in SECURITY.md:
- **Visibility holds only through the tools.** An agent with a shell can read
  any file the OS user can read, including Litecord's SQLite database. Hidden
  conversations are then protected only by the sandbox's read limits. The
  driver therefore:
  - uses a cwd outside the data directory;
  - sets the harness's read and deny rules to cover `$DATA` wherever the
    harness supports that;
  - warns when it cannot.

  Encrypting the database at rest (key in the OS keychain) is what would
  close this gap. It is listed as future work.
- **Never automate Discord.** Shell, browser and computer-use tools must not
  drive the Discord website, the desktop client or its API. That would bypass
  the Action Engine and break Discord's terms (self-botting). The prompt says
  so; `discord.com`, `discordapp.com` and `discord.gg` go on the harness
  network deny-list wherever one exists. Desktop automation cannot be fully
  policed, so the banner and stop button are the backstop.
- **Harness approvals come to Litecord.** Codex
  `item/commandExecution/requestApproval` and
  `item/fileChange/requestApproval`, and OpenCode `permission.*` events, are
  shown in the same approval UI as Discord proposals, but are labelled
  "local command" or "file change". The user answers there, and the driver
  forwards the decision (Codex `accept` / `decline`; OpenCode `once` /
  `always` / `reject`). "Always for this session" is allowed for local
  commands. **Discord proposals are never auto-approved in any mode.**

## 6. Harness features and where they live

| Feature | Codex | OpenCode | Litecord's part |
|---|---|---|---|
| Streaming replies | `item/agentMessage/delta`, `item/*` | SSE `message.part.updated` | Ephemeral stream to the UI |
| Interrupt / steer | `turn/interrupt`, `turn/steer` | `POST /session/:id/abort`; a steer is sent as a new prompt | Stop button; typing while running steers |
| Compaction | Automatic, or `thread/compact/start`; `thread/compacted` | Automatic, or `POST /session/:id/summarize` | "Compact" command; shows token use (`thread/tokenUsage/updated`); session rotation (§7) |
| Resume after restart | `thread/resume` | Sessions persist in the server | `omni_sessions.external_id` |
| Fork / "try another way" | `thread/fork` | `POST /session/:id/fork` | Button on a message |
| Subagents | Collab agents (`collabAgentToolCall` items) | Agents / child sessions (`GET /session/:id/children`, `@agent`) | Rendered as nested runs; every child uses the same MCP policy; child runs are linked by `parent_id` in `agent_runs` |
| Plans / todos | Plan items | `GET /session/:id/todo`, `todo.updated` | Shown live; "Save as task" copies an item into Litecord tasks, only when the user clicks it |
| Agent profiles | `developerInstructions` presets | Custom agents in config | Litecord ships profiles (§9) and passes them per session |
| Models | `model` on `thread/start` | `model` on prompt; `GET /config/providers` | Settings picker, filled from the harness |
| Slash commands | Mapped to the matching methods | `POST /session/:id/command`, `GET /command` | Command palette: "Ask Omni…", "/compact", "/fork", "/new" |
| Liveness | Pending request timeout; child exit status | `GET /global/health`; SSE heartbeat | Restart with backoff; state shown in Diagnostics |
| Usage / limits | `account/rateLimits/read` | Token usage per message | Settings and a per-session meter |

Excluded, with reasons:
- Code review (`review/start`): off-purpose for a chat client.
- Session sharing (`/share`): would publish private conversations.
- OpenCode's TUI endpoints: not used.
- Git and VCS views: out of scope.

## 7. Memory and compaction strategy (three layers)

1. **Harness compaction** keeps a single thread within its context window.
   It is automatic, and the user can also trigger it.
2. **Session rotation.** An Omni session is rotated after a size limit or at
   the first message of a new day:
   - Litecord asks the harness for a short summary of the session.
   - It stores that summary as a memory item with `Origin::AgentDerived`,
     linked to the session.
   - It starts a new thread whose first turn carries only that summary.

   This keeps turns cheap without depending on how each harness implements
   compaction.
3. **Unified memory is the durable record.** Facts that matter go in as
   notes, tasks or reminders through the tools, where retention, visibility
   and provenance rules apply. Transcripts are a capped log:
   - completed items only;
   - the last N sessions, default 50;
   - covered by `RetentionPolicy`.

## 8. Heartbeats (scheduled check-ins)

The harness knows nothing about Discord, so Litecord decides when Omni wakes.

- **Triggers:**
  - a daily digest at a user-chosen time;
  - a reminder coming due;
  - memory extracting a pending reply or commitment in a conversation marked
    "watch";
  - a manual "check now".

  Triggers are coalesced: only one check-in runs at a time, and at most
  `max_per_hour` (default 2).
- **Skip when nothing changed.** Each check-in records the revision it saw.
  If the database revision has not moved, the model is not called at all.
- **Cheap prompt.** `prompts/heartbeat.md` is rendered with a
  **content-free** delta (counts and ids since the last revision). The model
  fetches content through `compile_context`, under the same visibility rules.
- **Suppression.** A reply of exactly `HEARTBEAT_OK` is dropped silently
  (`prompts::is_heartbeat_ok`). Anything else becomes one inbox item.
- **Limits:**
  - at most `max_actions` local writes per check-in, default 3;
  - no Discord proposals from a check-in unless the user enables it, and even
    then they wait for approval;
  - quiet hours;
  - paused while on battery saver or offline;
  - off by default until the user signs in to a harness and turns it on.
- **Separate session.** Check-ins run in their own rotating session with the
  Assistant mode, so they never inherit Workspace or Computer-use access.

## 9. Prompts and profiles

- `prompts/omni.md` (about 650 tokens) contains the persona, how Litecord
  works, the tools in cost order, trust and visibility rules, style, the
  workspace and computer-use rules, and guidance for long tasks. It is passed
  as Codex `developerInstructions` or as OpenCode's agent `prompt` / `system`.
- `prompts/mcp_instructions.md` is a harness-neutral subset with no persona.
  It is returned from MCP `initialize` **today**, so users who connect Codex
  or OpenCode to `litecord mcp` directly already get the rules.
- `prompts/heartbeat.md` is the check-in template.
- Profiles are small additions (≤80 tokens each) appended to `omni.md`:
  - `triage`: read-only; sorts what needs a reply. The default subagent for
    catch-ups.
  - `drafter`: local writes and drafts only.
  - `researcher`: read-only; Workspace mode for web and files when enabled.

Tests keep the prompts honest:
- a size budget;
- every tool named in `omni.md` must exist in `spec::tools()`;
- the heartbeat template fills every placeholder.

## 10. Security invariants (additions)

1. Harness children get a cleared environment plus an allowlist. They never
   receive Discord, bot or database secrets.
2. Provider credentials stay in the harness. Litecord never reads, logs or
   stores them.
3. Discord writes from any harness, subagent or check-in are proposals. No
   mode auto-approves them.
4. Harness approval requests come back to Litecord with a timeout. If the
   user doesn't answer in time, the answer is **decline**.
5. The loopback OpenCode server binds to `127.0.0.1` with a random port and a
   per-launch password. CORS is off.
6. Token deltas and transcripts are never logged. Tracing records ids, kinds
   and durations only.
7. Discord content remains `trusted_as_instruction: false`, and the prompts
   repeat this rule.

## 11. Code layout (planned)

- `crates/litecord-harness` (new; features `codex`, `opencode`):
  - `driver.rs`: the trait plus `HarnessEvent`, `HarnessRequest`,
    `LoginState` and `SessionInfo`;
  - `codex.rs`: a JSON-RPC client over `tokio::process`, holding the pending
    request map and the notification pump;
  - `opencode.rs`: REST calls, the SSE reader and permission replies;
  - `env.rs`: the child environment allowlist;
  - `fake.rs`: a scripted driver for tests, in the same pattern as the bot
    `FakeTransport`.
- `migrations/0003_omni.sql`:
  - `omni_sessions (id, harness, external_id, profile, mode, title, parent_id, created_at, last_active_at, tokens_in, tokens_out, status)`
  - `omni_items (session_id, seq, kind, summary, created_at)`: capped
  - a `heartbeat_state` row
- `litecord-app`:
  - `OmniService`: starts and stops the sidecar, routes events, bridges
    approvals;
  - `HeartbeatScheduler`, which plugs into the existing maintenance loop;
  - view models: `OmniViewModel`, `HarnessStatus`, and `PendingActionRow`
    kinds for local commands and file changes;
  - `ApplicationEvent::Omni*`.
- `litecord-desktop`: `litecord omni status|login|logout` commands for
  headless use.

## 12. Build plan (each step shippable, tests first)

1. **Prompts and MCP instructions** (done): `litecord_agent::prompts`, with
   tests.
2. **Driver trait, fake driver and environment allowlist.** Plus
   `omni_sessions` and `OmniService` running against the fake: streaming,
   approvals bridge, interrupt, resume after restart.
3. **Codex driver.** Pin a minimum version. Generate the protocol schema from
   the installed binary (`codex app-server generate-json-schema`) and check
   the method, decision and `dynamicTools` names against it. The names in §5
   and §6 come from the public app-server README and still need this check.
   Sign-in names were checked against codex-cli 0.157.1; the live sign-in
   test is opt-in through `LITECORD_REAL_HARNESS=1` (`tests/real_harness.rs`).
4. **OpenCode driver.** Check it against the server's OpenAPI (`GET /doc`) and
   the permission-config names. Sign-in was checked against opencode 1.18.32
   by the same opt-in test.
5. **Heartbeats**: scheduler, revision skip, `HEARTBEAT_OK`, limits.
6. **Session rotation, profiles and subagent rendering.**
7. **Workspace and computer-use modes**, with the sandbox and deny-list
   settings and the UI banner.
8. Optional: Codex `dynamicTools` in-process.

Open questions to settle while building:
- Can Codex's built-in shell tool be turned off per thread? If not,
  Assistant mode on Codex runs with `sandbox: read-only` in an empty
  directory, and the UI states the residual read access from §5.
- Exact OpenCode permission keys (`bash`, `edit`, `external_directory`) in
  the pinned version.
