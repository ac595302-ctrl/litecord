# Building the UI on top of Litecord

This guide is the handoff for implementing the native interface (the mock
design PDF is the visual reference). The GUI renderer and screens are not yet
implemented. The app exposes view models and services, and the workspace
layout/profile model is now implemented separately. The Social SDK adapter is
still a skeleton, so app methods do not imply live Discord transport; see
`UI_SCREEN_MATRIX.md` for those limits.

## 1. Where the UI lives

Follow `UI_DESIGN.md` for the supplied mock PDF's visual language,
`UI_WORKSPACE.md` for layout/profile invariants, and `UI_SCREEN_MATRIX.md` for
the source-verified screen APIs and gaps. Stage 1 selected native egui/eframe.
Stage 2 implemented the layout model and persistence services; Stage 3 still
needs to build and verify the actual native screens and docking renderer.
Framework persistence is not the profile store. User layouts belong behind
typed `LitecordApp` services.

```text
crates/
  litecord-layout/     ← implemented tree, panel registry, profiles, edit session
  litecord-app/        ← view models, typed services, and profile persistence
  litecord-ui/         ← native egui/eframe renderer and screens (Stage 3 work)
  litecord-desktop/    ← headless binary today; wire the UI into its lifecycle
```

The UI should depend on `litecord-app` for application services and profile
persistence, and on `litecord-layout` for `LayoutNode`, `LayoutProfile`,
`LayoutProfiles`, `Placement`, and panel-registry metadata. Add direct
dependencies on `litecord-types` or `litecord-features` when the renderer uses
their IDs/enums or message-render/action types exposed by app view models. The
`litecord-app` crate owns SQLite access and validation; `litecord-layout` has
no UI-framework or storage dependency. The `litecord-ui` crate has theme,
icon, and event-bridge scaffolding, but its workspace and screen widgets are
not implemented. It is not a working GUI: panel widget factories, docking
interactions and visual verification remain Stage 3 work.

The panel registry is metadata, not a widget registry. `PanelDescriptor`
exposes panel ID, label, minimum width/height, compatible destinations, and
supported orientations. The renderer must map panel IDs to widget factories
and enforce those minimum sizes in the available viewport.

Rules that keep the architecture intact:

* The UI **never** holds canonical state. It holds view models (snapshots with
  `as_of_revision`) and ids, and re-requests them.
* The UI **never** calls a `SocialBackend`. Every write goes through a
  `LitecordApp` method.
* The UI uses `litecord-layout` pure operations for temporary trees and drafts;
  it persists only through the typed workspace methods below.
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
| `SessionChanged { state }` | Update the connection banner. `SessionState` is `LoggedOut/Authorizing/Connecting/Hydrating/Ready/Reconnecting/Offline/Error`. **Never assume Ready.** Offline keeps showing cached data. |
| `ActionAwaitingApproval { action_id }` | Show an approval toast / badge on Agent Inbox. Also fires for proposals made by an external agent via MCP. |
| `ReminderDue { reminder_id }` | Show a notification (OS notification integration is a UI task). |
| `ResyncRequired` / `RecvError::Lagged` | Drop incremental assumptions; re-fetch every visible view. |

Typed layout-profile writes publish `StateChanged` with a
`SettingChanged { key: "workspace.layout_profiles" }` after the SQLite
transaction commits. Refresh `layout_profiles_view()` for that key. A no-op
write emits no change event. The view's global `as_of_revision` is useful for
staleness displays; use its `storage_token`, not that revision, for optimistic
profile updates.

## 4. Screens → methods

| Screen (PDF) | Read | Actions |
|---|---|---|
| Home | No `home_view()` exists; compose Home from the existing view services or treat it as UI-owned navigation/summary. The layout model does define `Destination::Home`. | The app intent model has no `NavTarget::Home`; screen navigation and Home content are UI work. |
| Friends | `friends_view()` → online/offline/pending/blocked `FriendRow`s (status, activity, alias, favorite, `dm_conversation_id`) | `set_user_note`, relationship changes are Discord writes → propose via `apply_intents(vec![AppIntent::ProposeAction{..}])` or add a service |
| DM list / sidebar | `conversations_view(limit)` → `ConversationRow` (title, preview, `awaiting_reply`, recipient presence, agent visibility) | `set_conversation_visibility(id, Some(AgentVisibility::…))` for per-conversation agent access |
| Chat | `conversation_view(id, limit, before)` → oldest-first window, `has_more`, per-message `render` (already transformed by features: compact, highlight, privacy blur) and `actions` (context menu), `capabilities` (can_send/edit/delete, history, open-in-Discord URL), open drafts | `send_message`, `edit_message`, `delete_message`; message menu items return `AppIntent`s → `apply_intents` → `UiEffect`s |
| Agent Inbox (V2 flagship) | `agent_inbox_view()` → `needs_attention` (pending replies, due reminders, task candidates, commitments) + `pending_actions` with **full content** of what would be sent | `approve_action(id, edited)` (the user's Send/Approve button; pass an edited `AgentAction` for "Edit"), `reject_action(id)` |
| Tasks & reminders | `tasks_view()` | `confirm_task`, `dismiss_task`, `complete_task` |
| Memory inspector | `memory_view(entity, include_history)` → items with origin/status/confidence/supersession, graph edges, status counts | `confirm_memory`, `reject_memory` |
| Servers | `guilds_view()` → guilds + channels with `access` (native/linked/discord_only) and `open_in_discord_url` | open URL (UI) |
| Voice | `voice_view()` (state, participant names, supported flags) | `voice(VoiceControl::SetMuted(..))` etc. |
| Settings | `settings_view()` → registered feature settings and feature metadata. Workspace profiles use a separate view model; they are not part of this schema. | `set_setting(key, json)` for feature settings; `features.<id>.enabled` toggles features. `set_setting` rejects the reserved workspace layout key. |
| Workspace layout/profile editor | `layout_profiles_view()` → `LayoutProfilesViewModel { as_of_revision, profiles, storage_token, recovery_notice }` | `create_layout_profile`, `rename_layout_profile`, `duplicate_layout_profile`, `delete_layout_profile`, `activate_layout_profile`, `save_layout_profile`, `reset_layout_profile`, `reset_all_layout_profiles`; pass the current `storage_token` as `expected_token` on every write. |
| Command palette (Ctrl/Cmd+K) | `command_palette(query, active_conversation)` → ranked `CommandMatch` with shortcut + availability reason | `run_command(id, active)` → `Vec<UiEffect>` |
| Diagnostics overlay | `diagnostics_view()` → revision, session, backend mode, capabilities, hydration queue, counts, `MetricsSnapshot` (RSS, queue depths, db/context timings — real measurements only) | — |
| Model context debugger | `app.agent_gateway().call_tool("compile_context", json!({"instruction": …}), &Caller::new("ui"))` → `ContextPack` incl. `stats.included/excluded` | — |

`UiEffect` is what the backend cannot do for you: `Navigate`, `CopyToClipboard`,
`OpenUrl` (always user-initiated), `Notice`.

## 5. Workspace trees and profiles

`litecord-layout` is the pure model used by the native renderer. A
`LayoutProfiles` document has format version 1, a shared shell, and a layout
tree for each of the nine `Destination`s in every profile. `default_shell()`
and `default_workspace(destination)` provide validated defaults. The document
stores panel and node IDs, visibility, placement/orientation, split axes, and
weights; it does not store widget state, coordinates, conversation data, or
draft text. Validation currently caps the collection at 64 profiles, each
tree at 256 nodes and depth 16, and the serialized document at 1 MiB. Profile
names are trimmed, nonempty, and at most 64 characters; child weights must be
positive and finite, with a maximum of 1,000,000.

The built-in panel registry is metadata only. `registry::descriptor(id)` gives
the label, minimum width/height, compatible destinations, and supported
orientations. `registry::available(id, destination)` controls the
non-destructive `LayoutNode::project(destination)` view. Projection omits
hidden, unavailable, and unknown panels without changing the saved tree, and
collapses groups with one remaining child. The renderer still needs to map
panel IDs to widgets and enforce minimum sizes in its viewport.

The public `LayoutNode` operations are `dock(source, target, placement)`,
`resize(split_id, weights)`, and `reorder(split_id, from, to)`. Dock moves an
existing panel leaf relative to another panel leaf with Left, Right, Top, or
Bottom placement; it rejects self moves, group sources, and Center placement.
The operation validates a draft before replacing the original tree. It keeps
surviving node IDs, adds a wrapper split, and collapses a split that becomes a
single child. Resize requires one positive, finite weight per child (each at
most 1,000,000); reorder moves each child together with its weight. Neither
operation touches storage. The renderer should interpret weights as relative
proportions; the model does not normalize their sum.

For Edit Layout, construct `LayoutEditSession::new(profile)` and edit its
`draft`. `cancel()` returns the untouched original; `apply()` validates and
returns the edited `LayoutProfile` but does not persist it. Call
`save_layout_profile(profile, expected_token)` to save the result. The session
does not manage widget state or enforce apply/discard when the user switches
profiles; the UI owns those interactions.

`LitecordApp` profile persistence is separate from `settings_view()` and uses
the reserved key `workspace.layout_profiles`. Read the current
`LayoutProfilesViewModel` with `layout_profiles_view()`. It contains the
profiles, global `as_of_revision`, an opaque `storage_token`, and an optional
`recovery_notice`. Use the returned token for every mutation:

| Method | Effect |
|---|---|
| `create_layout_profile(name, expected_token)` | Create a profile with default trees; returns its ID. |
| `rename_layout_profile(id, name, expected_token)` | Rename a profile. |
| `duplicate_layout_profile(id, name, expected_token)` | Clone its trees to a new ID; returns the new ID. |
| `delete_layout_profile(id, expected_token)` | Delete it, selecting a surviving profile if it was active; refuses to remove the last profile. |
| `activate_layout_profile(id, expected_token)` | Change the active profile. |
| `save_layout_profile(profile, expected_token)` | Validate and replace a profile after applying an edit session. |
| `reset_layout_profile(id, expected_token)` | Restore default trees while preserving the profile's ID and name. |
| `reset_all_layout_profiles(expected_token)` | Replace the collection with the single Default profile. |

The token hashes only the raw layout setting, so unrelated Discord/database
revision changes do not make an edit stale. Each mutation rechecks that token,
validates and writes the complete document in one SQLite transaction, then
publishes `StateChanged` with the `SettingChanged` key after commit. If the
token is stale, reload `layout_profiles_view()` before applying the user's
draft. No-op writes do not advance the revision or publish a layout change.

If the saved JSON is corrupt or has an unsupported version,
`layout_profiles_view()` returns default profiles and a recovery notice while
leaving the original raw value intact. Mutations are blocked until the user
explicitly calls `reset_all_layout_profiles`; that reset replaces the corrupt
or future value with the current format. The generic `set_setting()` method
rejects `workspace.layout_profiles` so callers cannot bypass validation.

## 6. Things the UI must get right

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

## 7. Extending

* New screen data → add a view model + service method + test in
  `litecord-app`.
* New per-message decoration or menu item → a `Feature` in
  `litecord-features/src/builtin/` (hooks return intents; never mutate).
* New command → register in a feature's `register_commands`.
* Theming (V1 §18) is UI-owned; persist theme choice with `set_setting`.
