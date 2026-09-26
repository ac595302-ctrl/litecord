# UI Screen Matrix

This matrix maps the nine screen destinations and the shared overlays in the
mock design to the app-facing data and operations that exist in this
repository. It is based on `crates/litecord-app/src/view.rs`,
`crates/litecord-app/src/services.rs`, `crates/litecord-features/src/intent.rs`,
and the domain types, not on the mock alone. The repository still has no GUI
crate or rendered screens; `litecord-app` is the backend surface for a UI to
bind to.

## Backend status

The view and service methods below read/write the local app database and route
Discord operations through `SocialBackend` and the Action Engine. They do not
prove that a live Discord connection is available. `MockBackend` has
deterministic synthetic data and working mock operations. `SocialSdkBackend`
is a skeleton: it reports no capabilities, `connect` and required connection
methods return `NotConnected`, and optional methods return `Unsupported`.
Additionally, `AppBuilder::start` currently rejects the `SocialSdk`
configuration as not wired. OAuth2 PKCE, token refresh, and a verified SDK
bridge are unfinished. See `crates/discord-adapter/src/social_sdk.rs`,
`crates/litecord-app/src/app.rs`, and `docs/IMPLEMENTATION_STATUS.md`.

## Destinations

| Destination | Existing read data | Existing UI-facing actions | Missing or constrained behavior |
|---|---|---|---|
| **Home** | No Home view model or `home_view()` service. `agent_inbox_view()` and the other screen views can supply separate blocks. | Navigation to the existing screens can be implemented by the UI. | The app has no home/dashboard aggregate, recent-activity model, summary feed, or Home nav target. The layout and composition are UI work. Summary generation is also unfinished. |
| **Messages** | `conversations_view(limit)` returns rows with kind, title, recipient, last activity, a 120-character latest-message preview, awaiting-reply flag, agent visibility, and origin. `conversation_view(id, limit, before)` returns a message window, `has_more`, capabilities, visibility, and open drafts. Each `MessageRow` has author id, ownership, sent/edited/bookmarked state, extras, origin, feature-rendered message, and feature actions. | `send_message`, `edit_message`, `delete_message`; message action intents via `apply_intents`; `set_conversation_visibility`; `refresh(HydrationKey::DmConversation { .. })`. | This is conversation/DM data, not arbitrary guild-channel history. The SDK design explicitly treats guild history as unavailable with an Open in Discord fallback. Message sending/editing/deletion only work when the active backend reports the capabilities; the current SDK backend reports none. No rendered composer or message list exists yet. |
| **Friends** | `friends_view()` returns online, offline, incoming-request, outgoing-request, and blocked rows. Rows include user/display data, presence/activity, relationship, alias/favorite, optional DM id, and origin. | `set_user_note(UserNote)` saves local alias/note/favorite. Relationship operations can be represented as `AgentAction::RelationshipChange` and submitted through `apply_intents(AppIntent::ProposeAction { .. })`. `refresh(HydrationKey::Relationships)` refreshes source data. | There is no dedicated app service for friend request/accept/reject/remove/block controls; UI would have to form a generic action proposal. The real SDK backend currently supports no relationship reads or writes. |
| **Servers** | `guilds_view()` returns guilds and their channel rows. A channel carries kind, access (`native`, `linked`, `discord_only`), capability bits, and a Discord deep link. | Open the supplied channel URL from the UI; `apply_intents(OpenExternal)` returns an `OpenUrl` effect. `refresh(HydrationKey::Guilds)` or `GuildChannels { guild_id }` requests hydration. | There is no guild-channel message view/read method or guild-message history API in the UI services. Server/channel deep links are the fallback. The SDK backend currently reports even guild listing/channel capabilities unsupported. |
| **Voice** | `voice_view()` returns `VoiceState`, participant names, and `voice_supported` / `devices_supported`. State includes connected/lobby, mute/deafen, device, volume, noise suppression, push-to-talk, and participants. | `voice(VoiceControl)` sends user-initiated controls. Feature intents support mute, deafen, and leave; the command palette has gated voice commands. | Voice UI, device discovery and actual transport are not provided by the app surface. Controls depend on backend capability; the SDK backend currently reports none. |
| **Inbox** | `agent_inbox_view()` returns `needs_attention` items (pending replies, due reminders, task candidates, commitments) and pending actions. `PendingActionRow` includes kind/class/status, summary, target label, full optional content, actor, rationale, and creation time. | `approve_action(action_id, edited)` approves and executes; `reject_action(action_id)` rejects. Pending reply items can navigate to their conversation. | This is an agent/action attention inbox, not a general Discord notification inbox. Reminder events are emitted, but OS notifications are a UI integration task. The SDK transport limitation applies to approved Discord writes. |
| **Memory** | `memory_view(entity, include_history)` returns memory items with kind/content/payload, origin/status/confidence/source refs/entities, timestamps/revision/supersession and retrieval metadata; counts by status; and neighbors only when an entity is selected. Without history it omits superseded, rejected, and expired rows. | `confirm_memory(id)`, `reject_memory(id)`. Agent visibility controls are separate: `set_conversation_visibility(id, visibility)` governs access to conversation data. | No UI service creates or edits a memory directly. Extraction is heuristic and non-LLM; topic/project extraction is unfinished and only `ParticipatesIn` edges are automatically created. Derived items remain distinct from Discord facts. |
| **Tasks** | `tasks_view()` returns up to 100 open tasks, 100 candidates, and 100 pending/fired reminders. Task and reminder domain rows retain status, origin, source, related entities, and due information. | `confirm_task(id)`, `dismiss_task(id)`, `complete_task(id)`. | No dedicated app methods create/edit tasks or reminders, or dismiss/cancel reminders. The action model has `CreateTask` and `CreateReminder`, which can be passed as generic proposals, but there is no purpose-built task form/service. OS notification display remains UI work. |
| **Settings** | `settings_view()` returns settings descriptors grouped by their declared section, current JSON values/defaults, and feature metadata/enabled state. Current built-in setting descriptors cover compact mode, highlight terms, privacy mode, plus one enabled toggle per built-in feature. | `set_setting(key, value)` persists a value and updates the feature registry for `features.<id>.enabled`; feature commands can return setting intents through `apply_intents`. | There are no profile-edit or account-preference descriptors/services. Section names such as Account exist, but a section constant is not an implemented setting. `set_setting` is a local key/value store and does not update Discord profile/presence; unknown keys have no runtime behavior. A generic `ChangePresence` action exists, but there is no dedicated profile editor service. |

## Shared overlays and connection states

| Overlay or state | Existing read data | Existing UI-facing actions | Missing or constrained behavior |
|---|---|---|---|
| **Command palette** | `command_palette(query, active_conversation)` returns up to 20 fuzzy matches with id, name, category, shortcut, score, and availability/reason. Availability uses backend capabilities, online state, active conversation, and settings. | `run_command(id, active_conversation)` executes the handler and passes its intents to `apply_intents`; results are `UiEffect`s such as navigation, clipboard, URL, and notice. | The UI must draw the palette and apply effects. `CommandContext.selected_message` is always `None`, so commands requiring a selected message cannot become available through this service. Built-in palette/navigation coverage does not include every screen destination. |
| **Approval dialog** | Pending proposals are available in `agent_inbox_view()`. The row exposes full optional content and target for review. | Show the target and exact action content, then call `approve_action(id, edited)` or `reject_action(id)`. Editing changes the payload and invalidates the old approval; the engine revalidates current state and capability before execution. | The app returns an execution outcome/error; the UI must render confirmation and failure states. Approval protects proposed Discord writes, but cannot make an unsupported backend operation work. |
| **Diagnostics** | `diagnostics_view()` exposes current revision, session, backend mode, capability set, hydration pending/active counts, user/message/memory counts, and the metrics snapshot. | Read-only; subscribe to application events and reload. | There is no diagnostics view or export action. Queue values are aggregate counts, not per-job details. |
| **Auth / sign-in** | `SessionState` includes `LoggedOut`, `Authorizing`, `Connecting`, `Hydrating`, `Ready`, `Reconnecting`, `Offline`, and sanitized `Error`; diagnostics exposes the current state. | No sign-in, OAuth callback, logout, or credential-entry service exists on `LitecordApp`. Startup in demo mode connects synthetic fixtures. | OAuth2 PKCE and token refresh are unfinished. The configured Social SDK path is currently rejected by the app builder, and the backend skeleton itself cannot connect. An auth screen cannot complete a real sign-in with current services. |
| **Hydration / reconnect banner** | `LitecordApp::subscribe()` streams `SessionChanged`, `StateChanged`, `ActionAwaitingApproval`, `ReminderDue`, and `ResyncRequired`. `diagnostics_view()` exposes aggregate hydration counts. Opening a conversation requests stale data at active-screen priority. | `refresh(HydrationKey)` requests an explicit refresh. On `ResyncRequired` or a lagged event receiver, reload visible views. Session states include offline and reconnecting. | Hydration/reconnect scheduling is implemented and exercised with the mock backend. The pending queue is not persisted (`hydration_jobs` exists but is unused). A real SDK cannot supply live hydration until its adapter is implemented. The UI should keep cached data visible and avoid treating anything as Ready without checking session state. |

## Privacy and profile limits

The built-in Privacy Mode feature is a presentation transform on the chat
`RenderMessage`: when `privacy.enabled` is on, it replaces `author_display`
with “Hidden user”, replaces `content` with bullets, and sets `blurred`. It
does not redact the stored message or the surrounding `MessageRow` metadata.
In particular, the row still carries `author_id`, and the Quick Copy feature
builds its copy-text intent from the original message action context. List and
inbox previews, pending approval content, and memory data are separate view
models and are not transformed by this feature. Treat it as a chat display
mask, not as an account-wide privacy boundary or runtime policy.

`AgentVisibility` (`allowed`, `metadata_only`, `hidden`) is a separate
per-conversation control enforced by retrieval/context code for agent access.
It is not the screen-share mask. Profile or account settings are not enforced
by `set_setting`; only registered features interpret their known settings.

## Source map

- View model fields: `crates/litecord-app/src/view.rs`
- Screen read/write methods and intent dispatch: `crates/litecord-app/src/services.rs`
- Navigation and feature/command intents: `crates/litecord-features/src/intent.rs`
- Feature schemas and message transforms: `crates/litecord-features/src/builtin/`
- Connection lifecycle types: `crates/litecord-types/src/social.rs`
- Backend boundary and declared capability behavior: `crates/litecord-core/src/ports.rs`, `crates/litecord-types/src/capability.rs`
- Current real SDK adapter limitation: `crates/discord-adapter/src/social_sdk.rs`
