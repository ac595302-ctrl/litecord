# Building the UI on top of Litecord

This guide is for whoever implements the real interface (the mock-design PDF
is the visual reference). The backend is finished to the point that **the UI
never needs to touch SQLite, the Discord SDK, the Action Engine internals or
any agent code**. You bind to one object: `litecord_app::LitecordApp`.

## 1. Where the UI lives

Stage 1 design baseline: follow `UI_DESIGN.md` for the supplied mock PDF's
visual language, `UI_WORKSPACE.md` for the independent layout/profile model,
and `UI_SCREEN_MATRIX.md` for the verified API/feature gaps. The chosen
direction is native egui/eframe; visual and runtime feasibility still require
a runnable stage 3 implementation. Framework persistence is not the profile
store. User layouts belong behind typed `LitecordApp` services.

```text
crates/
  litecord-app/        ← the only crate your UI depends on (plus litecord-types
                         for ids/enums and litecord-features for render types)
  litecord-ui/         ← NEW: your widgets/screens (framework of your choice)
  litecord-desktop/    ← binary; add a `gui` subcommand or make GUI the default
```

No UI framework is chosen yet, deliberately. Everything the UI consumes is
plain, `Serialize`-able Rust data, so any of these work:

| Option | How it binds |
|---|---|
| **egui / iced / Slint** (native, recommended for the "lightweight" thesis) | call `LitecordApp` methods directly on a Tokio runtime owned by the binary; re-render on events |
| **Tauri / webview** | expose each view/service method as a Tauri command; view models serialize to JSON as-is |
| **TUI (ratatui)** | same as native |

Rules that keep the architecture intact:

* The UI **never** holds canonical state. It holds view models (snapshots with
  `as_of_revision`) and ids, and re-requests them.
* The UI **never** calls a `SocialBackend`. Every write goes through a
  `LitecordApp` method.
* Business logic stays out of widgets. If a screen needs a new derived value,
  add it to a view model in `crates/litecord-app/src/view.rs` +
  `services.rs` (with a test), not in the UI.

## 2. Lifecycle

```rust
let cfg = litecord_core::config::ConfigLoader::new()
    .file("litecord.toml").env(std::env::vars()).load()?;
let app = litecord_app::LitecordApp::builder(cfg).start().await?; // demo backend by default
let mut events = app.subscribe();
// ... run UI ...
app.shutdown().await; // cancels and joins all background tasks
```

`start()` opens the database (WAL), spawns the supervised runtime (event
reactor, hydrator, reconciler, reminder ticker, maintenance, action watcher),
connects the backend and kicks off initial hydration. It returns immediately;
data streams in.

## 3. Reactivity: when to re-render

`app.subscribe()` yields `litecord_core::events::ApplicationEvent`:

| Event | UI reaction |
|---|---|
| `StateChanged { revision, changes }` | Re-fetch the views whose data changed. `changes` lists ids (`MessageCreated{conversation_id,..}`, `PresenceChanged{user_id}`, `ActionProposed{..}`, …). Simple strategy: debounce ~16–50 ms, then refresh visible views whose `as_of_revision < revision`. |
| `SessionChanged { state }` | Update the connection banner. `SessionState` is `LoggedOut/Connecting/Hydrating/Ready/Reconnecting/Offline/Error`. **Never assume Ready.** Offline keeps showing cached data. |
| `ActionAwaitingApproval { action_id }` | Show an approval toast / badge on Agent Inbox. Also fires for proposals made by an external agent via MCP. |
| `ReminderDue { reminder_id }` | Show a notification (OS notification integration is a UI task). |
| `ResyncRequired` / `RecvError::Lagged` | Drop incremental assumptions; re-fetch every visible view. |

## 4. Screens → methods

| Screen (PDF) | Read | Actions |
|---|---|---|
| Friends | `friends_view()` → online/offline/pending/blocked `FriendRow`s (status, activity, alias, favorite, `dm_conversation_id`) | `set_user_note`, relationship changes are Discord writes → propose via `apply_intents(vec![AppIntent::ProposeAction{..}])` or add a service |
| DM list / sidebar | `conversations_view(limit)` → `ConversationRow` (title, preview, `awaiting_reply`, recipient presence, agent visibility) | `set_conversation_visibility(id, Some(AgentVisibility::…))` for the privacy toggle |
| Chat | `conversation_view(id, limit, before)` → oldest-first window, `has_more`, per-message `render` (already transformed by features: compact, highlight, privacy blur) and `actions` (context menu), `capabilities` (can_send/edit/delete, history, open-in-Discord URL), open drafts | `send_message`, `edit_message`, `delete_message`; message menu items return `AppIntent`s → `apply_intents` → `UiEffect`s |
| Agent Inbox (V2 flagship) | `agent_inbox_view()` → `needs_attention` (pending replies, due reminders, task candidates, commitments) + `pending_actions` with **full content** of what would be sent | `approve_action(id, edited)` (the user's Send/Approve button; pass an edited `AgentAction` for "Edit"), `reject_action(id)` |
| Tasks & reminders | `tasks_view()` | `confirm_task`, `dismiss_task`, `complete_task` |
| Memory inspector | `memory_view(entity, include_history)` → items with origin/status/confidence/supersession, graph edges, status counts | `confirm_memory`, `reject_memory` |
| Servers | `guilds_view()` → guilds + channels with `access` (native/linked/discord_only) and `open_in_discord_url` | open URL (UI) |
| Voice | `voice_view()` (state, participant names, supported flags) | `voice(VoiceControl::SetMuted(..))` etc. |
| Settings | `settings_view()` → sections (Appearance, Privacy, Plugins, …) with descriptors + current values; feature list | `set_setting(key, json)`; `features.<id>.enabled` toggles features |
| Command palette (Ctrl/Cmd+K) | `command_palette(query, active_conversation)` → ranked `CommandMatch` with shortcut + availability reason | `run_command(id, active)` → `Vec<UiEffect>` |
| Diagnostics overlay | `diagnostics_view()` → revision, session, backend mode, capabilities, hydration queue, counts, `MetricsSnapshot` (RSS, queue depths, db/context timings — real measurements only) | — |
| Model context debugger | `app.agent_gateway().call_tool("compile_context", json!({"instruction": …}), &Caller::new("ui"))` → `ContextPack` incl. `stats.included/excluded` | — |

`UiEffect` is what the backend cannot do for you: `Navigate`, `CopyToClipboard`,
`OpenUrl` (always user-initiated), `Notice`.

## 5. Things the UI must get right

* **Label synthetic data.** Rows carry `origin`; `synthetic` means demo data
  (V1 §26). Show a "Demo data" badge.
* **Capability-driven UI.** Disable/hide actions using `capabilities`
  (`ConversationViewModel.capabilities`, `DiagnosticsViewModel.capabilities`).
  Unsupported content shows "Open in Discord" with the provided URL, never a
  fake.
* **Approval UX.** Show exactly `PendingActionRow.content` and target before
  the user approves. Editing produces a new payload hash; the old approval is
  invalid automatically.
* **Provenance in memory UI.** Show `origin` and `status` (candidate vs
  confirmed vs superseded) — derived memories are not facts.
* **Virtualize** long conversations (V1 §21); page with `before`.
* **Keyboard first.** Shortcuts come from `CommandMatch.shortcut`
  (`Keybind` strings like `Ctrl+Shift+M`).
* **Threading.** `LitecordApp` is `Clone + Send + Sync`. Read methods are
  synchronous and fast (SQLite, WAL); call them from the UI thread or a
  worker. Async methods (sends, approvals, commands) need the Tokio runtime.

## 6. Extending

* New screen data → add a view model + service method + test in
  `litecord-app`.
* New per-message decoration or menu item → a `Feature` in
  `litecord-features/src/builtin/` (hooks return intents; never mutate).
* New command → register in a feature's `register_commands`.
* Theming (V1 §18) is UI-owned; persist theme choice with `set_setting`.
