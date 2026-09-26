# Structural workspace and layout profile contract

Stage 1 contract. Pure layout code belongs in a new `litecord-layout` crate; persistence and commands belong in `litecord-app`; framework rendering belongs in `litecord-ui`. App must never depend on UI.

## Durable state

One versioned document under an application-owned settings key contains `version`, `active_profile_id` and a bounded collection of named profiles. A profile contains a shared shell tree and one tree per primary destination. All profile updates and active-ID changes are one transaction. Typed load/mutate APIs validate centrally; widgets never read/write SQLite or individual config files.

The shared shell holds `primary_navigation` and `user_controls`; a `workspace` outlet hosts the active destination tree. The default shell can group the rail/account vertically beside the workspace. Destination roots contain contextual list, primary content and optional inspector, using the design defaults from `UI_DESIGN.md`.

Node variants: `Panel` (stable node ID + panel ID + visibility + validated placement/orientation preference), `Split` (stable node ID, axis, ordered weighted children). Axis describes distribution; it does not automatically define leaf orientation. All sizes are logical. Panel minima and allowed placements are registry metadata, not untrusted profile authority. Widget IDs, global x/y, data snapshots and draft text are not serialized.

Use stable ASCII string IDs. Singleton panel IDs must be unique across the shared shell and each simultaneously mounted destination. Future multi-instance panels require an explicit instance identity; do not accidentally add duplicates now.

## Panel registry

Descriptors contain stable ID, label, applicable destinations, minimum sizes, supported orientations/placements and singleton policy. The renderer uses registered factories; persisted data contains IDs only. Initial panel roles include navigation/account/workspace outlet; conversation list/chat/contact inspector; friends; server list/channel list/server content; composite voice room; inbox; memory; tasks; settings; contextual lists/inspectors as supported.

Registry resolves contextual availability without modifying saved trees. Empty data is an empty panel, not a reason to erase it. Unknown IDs are retained in durable data and projected as unavailable/omitted. Render-time projection can prune empty groups and normalize weights without writing those changes back.

## Pure operations

Move a leaf relative to another using left/right/above/below docking or reorder in a compatible group. Validate source/target; disallow self/descendant moves; remove the source once; insert once; normalize empty and single-child groups. Failure leaves the original tree unchanged. IDs remain stable after moves. Split resizing stores finite positive proportions.

Placement metadata tells adaptable panels how to render: side placement gives server lists vertical orientation; top/bottom strips give horizontal orientation. A parent split axis is insufficient. Explicit user orientation overrides must be supported by the descriptor. Unsupported placements fail before mutation.

Edit Layout clones the active profile into a draft. Drops/resizes affect the draft. Apply validates/persists; Cancel/Escape restores the original. Switching profile while editing requires apply/discard. Normal-mode splitter changes save at interaction end. Preserve local composer/selection/focus state by panel identity when mounting positions change.

## Validation and recovery

Initial limits: 64 profiles, 256 nodes per tree, depth 16, profile name 1–64 trimmed characters, document at most 1 MiB. Reject zero/negative/nonfinite weights, empty splits, repeated node/singleton IDs, illegal placements, invalid active IDs and no usable primary workspace. Default trees obey the same validator. Tests define and enforce these bounds; changing a bound is a schema/service decision.

Missing settings use built-in Default. Corrupt or future-version settings load a safe default with a recovery notice while preserving the original stored value until an explicit reset/save decision. Unknown panel IDs alone are not corruption. Provide deterministic fallback when deleting an active profile; keep at least one profile. Duplicate allocates a new ID and independent trees. Reset-current preserves profile identity/name and replaces layout with defaults; reset-all creates the Default collection.

A always-reachable workspace/reset menu lives outside the editable tree. At narrow widths, temporarily collapse an optional inspector or use a drawer/scroll strategy; preserve saved user proportions. Essential controls must not disappear outside the window.

## Application and runtime APIs

Add typed load/create/rename/duplicate/delete/activate/reset/update methods to `LitecordApp`, plus a profile snapshot with an application revision and optional recovery notice. Reject stale update snapshots using the expected stored revision where practical. Existing `settings_view()` is schema-derived and cannot load arbitrary profile JSON by itself. Centralize reserved layout-key handling so generic settings writes cannot bypass validation.

Writes emit the existing setting-change event after commit; interested UI views refresh. No-op updates do not churn revisions. GUI owns an event bridge with bounded invalidations/results; async application calls run on the desktop-owned Tokio runtime. Defer background/hidden snapshots until needed; long message lists remain paged and virtualized.

## Stage 2 acceptance

Pure tests cover serialization, splits, docking directions, identity preservation, illegal moves, orientation, contextual projection, unknown IDs and bounds. Application tests cover profile CRUD/reset, atomic collection+active-ID updates, corruption/future-version preservation, event behavior and restoration from a file-backed database after complete app shutdown/restart. The persisted layout never contains canonical social data.
