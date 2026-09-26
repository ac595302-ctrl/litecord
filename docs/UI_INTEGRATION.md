# Native UI integration checkpoint

This guide records the current egui/eframe UI implementation and its app
integration. Stages 1–2 established the visual contract, layout model, and
profile persistence. Stages 3–5 now provide canonical Messages and Friends, a
profile-backed shell/workspace renderer, and baseline screens for all nine
primary destinations. Stage 6 headless GUI checks, screenshot/render review,
and dependency checks remain ongoing; this checkpoint does not claim full
visual fidelity to the reference PDF.

The UI talks to `LitecordApp` through view models and a bounded asynchronous
bridge. The real Social SDK backend is still a skeleton, so the demo backend
provides the available end-to-end behavior and is labelled `Demo · synthetic`.
`UI_SCREEN_MATRIX.md` remains useful for app view-model shapes and backend
capability limits; this file records current UI behavior.

## 1. Where the UI lives

Follow `UI_DESIGN.md` for the supplied mock PDF's visual language and
`UI_WORKSPACE.md` for layout/profile invariants. The framework-independent
layout model and typed persistence services are implemented. User layouts
belong behind typed `LitecordApp` services, not egui persistence.

```text
crates/
  litecord-layout/     ← tree, panel registry, profiles, edit session
  litecord-app/        ← view models, typed services, and profile persistence
  litecord-ui/         ← native egui/eframe shell, bridge, renderer, and screens
  litecord-desktop/    ← GUI feature plus headless demo/MCP/status commands
```

The UI depends on `litecord-app` for application services and profile
persistence, and on `litecord-layout` for `LayoutNode`, `LayoutProfile`,
`LayoutProfiles`, `Placement`, and panel-registry metadata. Add direct
dependencies on `litecord-types` or `litecord-features` when the renderer uses
their IDs/enums or message-render/action types exposed by app view models. The
`litecord-app` crate owns SQLite access and validation; `litecord-layout` has
no UI-framework or storage dependency. The `litecord-ui` crate contains theme
and icon tokens, the background bridge, workspace/profile rendering, and the
current screen implementations. Stage 6 behavior and visual verification
remain in progress, along with the unsupported capabilities listed below.

The panel registry is metadata, not a widget registry. `PanelDescriptor`
exposes panel ID, label, minimum width/height, compatible destinations, and
supported orientations. The renderer maps registered panel IDs to workspace
content. Viewport minimum-size and narrow-window behavior remain part of the
visual review.

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

Launch the native UI with `cargo run -p litecord-desktop --features gui -- gui`.
Headless `demo`, `mcp`, and `status` commands remain available. Optional hidden
GUI screenshot QA uses `cargo run -p litecord-desktop --features screenshots
-- gui --screenshot PATH --screen Friends --width 760`; screenshot capture,
graceful close, and joined runtime shutdown have been observed.

## 3. Reactivity: when to re-render

`app.subscribe()` yields `litecord_core::events::ApplicationEvent`:

| Event | UI reaction |
|---|---|
| `StateChanged { revision, changes }` | Re-fetch the views whose data changed. `changes` lists ids (`MessageCreated{conversation_id,..}`, `PresenceChanged{user_id}`, `ActionProposed{..}`, …). Simple strategy: debounce ~16–50 ms, then refresh visible views whose `as_of_revision < revision`. |
| `SessionChanged { state }` | Update the connection banner. `SessionState` is `LoggedOut/Authorizing/Connecting/Hydrating/Ready/Reconnecting/Offline/Error`. **Never assume Ready.** Offline keeps showing cached data. |
| `ActionAwaitingApproval { action_id }` | Show an approval toast / badge on Agent Inbox. Also fires for proposals made by an external agent via MCP. |
| `ReminderDue { reminder_id }` | Show a notification (OS notification integration is a UI task). |
| `ResyncRequired` / `RecvError::Lagged` | Drop incremental assumptions; re-fetch every visible view. |

The UI runs app access through a bounded background bridge. It refreshes from
app events and also checks for a new snapshot every two seconds to detect
cross-process changes. The egui thread renders snapshots; commands are sent
to the bridge rather than calling backend methods from widgets.

Typed layout-profile writes publish `StateChanged` with a
`SettingChanged { key: "workspace.layout_profiles" }` after the SQLite
transaction commits. Refresh `layout_profiles_view()` for that key. A no-op
write emits no change event. The view's global `as_of_revision` is useful for
staleness displays; use its `storage_token`, not that revision, for optimistic
profile updates.

## 4. Screens → methods

| Screen | Read | Current controls and limits |
|---|---|---|
| Home | Account identity plus conversation, friend, task, inbox, and diagnostics snapshots | Actual-count metric cards, selected activity previews, and restrained Omni status; generative replies are unavailable. |
| Messages | `conversations_view(limit)` and `conversation_view(id, 200, before)` | Canonical message list with variable-height virtualization and paged windows; send clears the draft only after successful execution. Edit/delete use confirmation dialogs; context actions run through app intents. |
| Friends | `friends_view()` plus canonical account/contact snapshots | Presence, contact details, and local notes use app data. Accept/decline incoming requests, remove/block friends, and unblock contacts through capability-gated controls and the typed `change_relationship` service. Per-conversation agent access is controlled by `set_conversation_visibility`; it is separate from presentation Privacy Mode. |
| Servers | `guilds_view()` → guild/channel metadata, access labels, and Discord URLs | Select guilds/channels and open the selected channel in Discord. There is no native channel message service. |
| Voice | `rooms_view()` for lobby metadata and an optional matching recent conversation; `voice_view()` for session state, participants, and support flags | Supported voice controls are sent through the app. Room Messages opens the linked conversation in the separate Messages destination. Files requires backend history; device discovery and room transcription are unavailable. |
| Inbox | `agent_inbox_view()` → `needs_attention` rows and pending actions with full payloads | Pending replies navigate to Messages; candidates/reminders to Tasks; commitments to Memory. Pending actions show the exact content and offer approve/reject. Approval is disabled when privacy masks the payload. |
| Tasks | `tasks_view()` | Confirm, dismiss, and complete supported tasks. Task priorities, subtasks, and comments are not implemented. |
| Memory | `memory_view(entity, include_history)` → provenance/status/confidence, supersession, graph edges, and counts | Confirm or reject supported memory items; commitments from Inbox open Memory. |
| Settings and profiles | `settings_view()` plus `layout_profiles_view()` | Feature settings and profile create/rename/duplicate/delete/activate/reset. Layout edit uses Edit/Apply/Cancel and typed profile saves. |
| Palette and diagnostics | `command_palette(query, active_conversation)`, `command_shortcuts(active_conversation)`, `diagnostics_view()` | Run available commands by palette or registered shortcut; Enter runs the first available current result. Inspect revision, session, backend mode, capabilities, queues, counts, and measured metrics. |

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
collapses groups with one remaining child. The current renderer maps registered
panels to workspace content. The shell includes an always-available menu beyond
the editable tree; optional sidebar and inspector panels can be hidden. The
server strip supports top or bottom orientation. Narrow-window and minimum-size
behavior remain part of visual review.

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

The native profile editor provides create, rename, duplicate, delete, activate,
and reset controls. Edit Layout keeps changes in a draft: Apply validates and
saves the edited profile, while Cancel discards the draft. Dragging a panel to
an edge docks it, and splitter changes save at interaction end. In lower-level
code, `LayoutEditSession::cancel()` returns the untouched original and
`apply()` validates a `LayoutProfile`; the UI then persists with
`save_layout_profile(profile, expected_token)`.

Ctrl/Cmd+Shift+L toggles layout editing; Escape cancels the active draft. The
header exposes Apply and Cancel while editing, and entering layout editing
closes the profile manager. `crates/litecord-ui/examples/layout_preview.rs`
demonstrates a horizontal shell with navigation/account panels and a horizontal
server strip for visual QA.

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

* **Label synthetic data.** Demo mode is visibly labelled `Demo · synthetic`;
  preserve row provenance in memory surfaces.
* **Capability-driven UI.** Disable/hide actions using `capabilities`
  (`ConversationViewModel.capabilities`, `DiagnosticsViewModel.capabilities`).
  Unsupported content shows "Open in Discord" with the provided URL, never a
  fake.
* **Approval UX.** Show exactly `PendingActionRow.content` and target before
  the user approves. Do not approve automatically. Keep approval disabled when
  Privacy Mode masks the exact payload. Editing produces a new payload hash;
  the old approval is invalid automatically.
* **Provenance in memory UI.** Show `origin` and `status` (candidate vs
  confirmed vs superseded) — derived memories are not facts.
* **Virtualize** long conversations with variable-height rows; page 200-message
  windows with `before`.
* **Keyboard first.** Shortcuts come from `CommandMatch.shortcut`
  (`Keybind` strings like `Ctrl+Shift+M`).
* **Threading.** `LitecordApp` is `Clone + Send + Sync`. The native UI uses a
  bounded bridge worker for app access, consumes app events, and polls every
  two seconds for cross-process changes. Keep widgets on the egui thread and
  route writes through the app bridge and Tokio runtime.

## 7. Extending

* New screen data → add a view model + service method + test in
  `litecord-app`.
* New per-message decoration or menu item → a `Feature` in
  `litecord-features/src/builtin/` (hooks return intents; never mutate).
* New command → register in a feature's `register_commands`.
* Theming (V1 §18) is UI-owned; persist theme choice with `set_setting`.
