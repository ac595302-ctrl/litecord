# Litecord UI design contract

Status: stage 1 design baseline. This is an implementation contract, not a claim that the GUI exists.
Source: user-supplied `litecord_ui_mock_audit.pdf` (20 pages), reviewed visually as well as by text. A01 / page 4 is the primary anchor. Pages 5–15 establish destination variations; pages 17–20 enumerate outstanding work. The audit's surrounding annotations are not app UI.

## Product identity

Litecord is a dense, calm communications workspace. Preserve the reference's navy/charcoal surfaces, narrow primary rail, conversation/context sidebar, expansive center and optional inspector. Do not replace it with an ordinary egui tool window, a bright dashboard, a generic Discord clone or permanently visible docking chrome.

Default navigation order is Home, Messages, Friends, Servers, Voice, Inbox, Memory, Tasks, Settings. The visible name Inbox maps to the application's agent inbox. Diagnostics and the command palette are auxiliary surfaces, not additional primary destinations. Assistant branding is Omni; capability labels must distinguish heuristic suggestions, external-agent results and unavailable generation.

Navigation identity and panel identity are separate. Messages remains Messages even when its conversation list moves. The selected destination has a blue indicator, background and readable text; color alone is insufficient.

## Native framework decision

Use **egui + eframe**, native desktop, with a single renderer (Glow), built-in fonts and AccessKit. Disable framework persistence: Litecord's application services own durable preferences. Build a small structural split renderer over our independent layout model; do not serialize egui widget IDs or adopt a library's private docking tree as the durable format.

Rationale: custom styling and painting can closely follow the supplied shell, immediate-mode drag/split interactions are straightforward, virtualized rows and input widgets already exist, and the app can own a Tokio runtime without adopting a browser or another application data layer. Iced's PaneGrid is a credible alternative with built-in docking/resizing, but does not remove the need for an independent profile model. Slint/webview are not needed for the first native implementation.

This is an engineering choice, **not a measured memory/startup claim**. At stage 3 record build/launch behavior and verify rendering, DPI, focus, AccessKit names and idle repaint frequency. Use native controls where possible; custom painted controls must publish accessible widget information and have a keyboard equivalent. Reassess only if those checks reveal an actual limitation.

Official references reviewed September 26, 2026:

- [eframe native integration and feature flags](https://docs.rs/eframe/latest/eframe/)
- [egui accessibility guidance](https://github.com/emilk/egui/blob/main/docs/accessibility.md)
- [Iced PaneGrid capabilities](https://docs.iced.rs/iced/widget/pane_grid/struct.PaneGrid.html)

## Initial design tokens

These values are deliberately derived approximations of A01, to be adjusted through screenshot comparison. They are not asserted to be exact sampled source colors. Use centralized tokens, never a separate palette per screen.

| Role | Dark baseline | Rule |
|---|---|---|
| Window/shell | `#111722` | navy charcoal, no neon gradient |
| Rail/sidebar | `#151D2B` | subtle separation from center |
| Workspace | `#171F2C` | quiet backdrop for readable messages |
| Raised surface/input | `#202A3C` | restrained cards and composer |
| Border/divider | `#2D3A50` | low emphasis; not a focus indicator |
| Primary text | `#E7ECF5` | names, messages and headings |
| Secondary text | `#B0BDD2` | metadata remains readable |
| Muted text | `#8E9CB4` | decorative/tertiary text only |
| Selection/action | `#4D7DFF` | blue/indigo, no resting glow |
| Selected surface | `#263957` | restrained row/rail selection |
| Omni | `#50D2C1` | teal, subtle surface tint; no purple AI glow |
| High priority/error | `#F08B91` | restrained red with text/icon meaning |
| Online/success | `#55D7A0` | retain a text or shape cue |

Normal button labels use primary text on dark surfaces or dark text on bright fills as needed for contrast; do not assume white on blue/teal passes for small type. Require 4.5:1 for normal text and 3:1 for large text and essential control boundaries. Test focus, disabled and selected states separately. Accessibility review can change a token while retaining the supplied hue/character.

Typography: proportional sans-serif, body 14 logical px, compact metadata 12, section titles 16–18, destination title 20. Do not shrink text to fit a window. Initial spacing scale: 4/8/12/16/24. Control radii 6–8; avatar radius circular. Avoid oversized headings, tall empty cards and excess whitespace.

## Geometry and hierarchy

Use a logical desktop reference of 1586 × 992, corresponding to the majority of the mock lineages. Approximate initial widths: rail 88, contextual sidebar 280, inspector 300; center takes the remainder. Header approximately 48 high. These are default layout preferences, not immutable coordinates. Screenshots must crop to the application, excluding PDF annotations.

Messages (A01): conversation search/filter/list at left, contact header at center top, chronological conversation below, composer anchored at center bottom, contact inspector at right. Composer remains available when history scrolls. Preserve avatars, author/time hierarchy, quiet separators, media blocks and concise menus. Unsupported mock actions such as video, gifts or attachment upload do not become enabled buttons merely because they appear in the reference.

Home (A02): the only destination with the broad dashboard metric pattern. Aggregation must use a tested application view model; identity comes from the signed-in account, not a copied mock persona.

Friends (A03): compact grouped relationships, presence and contextual selected-contact details. Servers (A04): a clean base workspace plus a restrained floating server switcher attached to the Servers item. The hover illustration does not establish a permanently open server column as the default.

Voice (A05–A08): one composite room. Participant cards, room tabs, device/settings area and bottom mute/deafen/disconnect controls retain relative geometry when changing tabs. Users can move/resize the composite, not accidentally separate essential call controls. Unsupported file/summary/transcription states are explained without invented activity.

Inbox (A09): compact attention items and exact pending action payloads. Today has one owner. Memory (A10): source/status/confidence and selected-entity relationships, with honest empty graph states. Tasks (A11): compact tasks and one conditional follow-up affordance; avoid reintroducing the removed dashboard clutter. Settings (A12): revise to cover supported features, appearance, layouts, privacy and application controls; omit unimplemented plans/connected accounts.

## Interaction and state contract

Normal mode shows no drag handles/drop zones. Edit Layout temporarily adds them. Focus rings, drop zones and selection use related blue semantics but remain distinct by shape/text. Keep critical send/approval meaning independent of docking.

Every destination has loading, empty, unavailable and error behavior. Hydrating is not Ready. Offline retains cached data with a clear banner and unavailable network operations; local profile/task operations remain usable where supported. Label synthetic demo data persistently and retain origin labels in memory.

Agent proposals show full content, target and acting identity before approval. Editing invalidates prior approval. Present errors/staleness without auto-retry of irreversible actions. Direct user composition follows existing application action policy rather than adding an unnecessary second proposal-review dialog.

Privacy Mode is a presentation contract across visible names, avatars, previews, inspectors, tooltips and attachments. Existing message transforms alone do not satisfy it. Per-conversation agent visibility is a separate data-access setting. Theme/retention/permission controls require actual runtime behavior, not merely a saved JSON value.

Keyboard: command palette on Ctrl/Cmd+K; shortcuts from backend command matches where defined. Every drag/resize action needs menu or keyboard access. Keep focus visible, provide labels/tooltips, support readable large text and a reduced-motion setting. Floating switcher/palette can open by keyboard and close with Escape.

## Visual acceptance before stage 3 is called complete

- Compare Messages screenshot with A01 for shell widths, visual hierarchy, density, selection, composer and inspector placement.
- Confirm supported behavior works with real `LitecordApp` demo snapshots; do not use copied chat text as canonical state.
- Compare Voice tab geometry and Settings organization to their reference pages.
- Check normal desktop, narrow window and high DPI: no clipped text, inaccessible controls or destructive saved-layout changes.
- Inspect light-theme tokens and focus/disabled/error/privacy states; the PDF does not supply a complete light system, so preserve contrast and hierarchy.
- Record screenshot paths and actual build/interaction checks in handoff. A styled shell alone is not a completed stage 3.
