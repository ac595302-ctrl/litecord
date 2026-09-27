# Native UI verification — September 26, 2026

The initial native UI is functional on the synthetic demo backend. This
checkpoint does not certify full mock fidelity or a live Discord integration.

## Completed checks

- Full workspace tests passed locally on Windows, followed by focused app/UI
  tests after the final interaction changes.
- Formatting and all-target/all-feature workspace Clippy pass.
- cargo-deny advisories, bans, licenses and sources pass. Eframe is pinned to
  0.34.3, which removes the earlier unmaintained font-parser dependency.
  Clipboard/font license exceptions are scoped to their specific packages.
- Six UI tests exercise successful/failed sends, canonical snapshots, privacy
  rendering, nine destinations at wide/narrow sizes, actual pointer docking
  with cancel/apply and draft preservation, and registered shortcuts independent
  of palette queries. App tests cover saved-profile restart/conflicts/recovery,
  canonical identity and rooms, and accept/block/unblock backend events.
- Native PNG captures cover Home, Messages, Friends, Servers, Voice, Inbox,
  Memory, Tasks, Settings, narrow Messages, and a horizontal shell/server strip.
  Each capture exits normally and joins the application's runtime tasks.
- A CI startup race in the signed-out account test was corrected by explicitly
  committing a setting before asserting that the snapshot revision is positive.
  Identity assertions remain unchanged.

Local verification used workspace-local Rust 1.98.1 and LLVM-MinGW. The hosted
CI matrix also checks Linux, macOS and Windows; consult the latest Actions run
for final platform results rather than assuming local success proves them.

## Release memory sample

A release build with the optional PNG QA feature was sampled on Windows about
two seconds after opening Friends, before screenshot capture, with the default
1586 × 992 logical viewport (host display/DPI constrained the physical image).
The synthetic QA database contained the standard demo data.

| Measurement | Observed value |
|---|---:|
| Process working set | 137.82 MiB |
| Private bytes | 115.44 MiB |
| Accumulated process CPU time | 0.9375 seconds |
| Executable size | 14,867,968 bytes |

This is an early process sample, not a sustained idle benchmark, peak-memory
bound, or frame-latency measurement. It exceeds the proposed 50 MiB target.
Memory attribution and optimization remain necessary.

### Stage A before/after (Linux, September 26, 2026)

Same workload for both builds: release `--features gui`, Xvfb 1600 × 1000,
fresh data directory with demo data, launched on Messages. Resident memory
and CPU time (user + system, 10 ms ticks) were read after 20 seconds.
"Before" is commit 8da696a; "after" is fb008ff. Three runs each:

| Build | Resident (KiB) | CPU ticks |
|---|---|---|
| Before | 138820 · 138828 · 138556 · 138380 · 138888 · 138540 | 121–135 |
| After | 140704 · 139968 · 140028 | 140–143 |

Idle startup did not improve. It is about 1.2 MiB higher (+0.9%) and uses
about 10 more CPU ticks. Possible causes, none measured separately:
- the demo data now includes 260 deep-history messages;
- the history-sync task wakes every 5 s;
- the Home/Inbox cards and the header are richer.

Most of the ~137 MiB is the renderer, fonts and window surfaces, not
Litecord data.

What Stage A changes is behaviour under load:
- Snapshots reload only the visible destination's views. Hidden
  destinations reuse the previous `Arc` values (test
  `snapshots_reload_only_the_visible_destination`).
- The ingest queue is capped at 8 MiB of queued events.
- The open conversation's message window is capped at 4 MiB.
- The Settings inspector shows the live figures.

Large startup snapshots are **not** split into batches. Snapshot events
replace state authoritatively, so splitting them would need a new partial-
snapshot protocol in the reducer. The byte budget admits an oversized
envelope only when the queue is otherwise empty, so memory stays bounded.

## Omni sign-in, driven through the real UI (September 27, 2026)

Real harnesses (codex-cli 0.157.1, opencode 1.18.32) in throwaway homes,
driven by `xdotool` clicks and typing under Xvfb against the debug build:

| Flow | Result |
|---|---|
| Codex: open Settings → Omni | Sign-in state is checked on its own; shows "Not signed in", "Sign in with ChatGPT", API key and device code |
| Codex: click "Sign in with ChatGPT" | Codex starts its browser sign-in; the card shows "Finish signing in" with Open page / Check / Cancel (no browser in the sandbox, so the page itself did not open) |
| Codex: Add key → type a dummy key → Save key | Codex writes its own `auth.json`; card shows "Signed in · OpenAI API key" |
| Codex: Sign out | `auth.json` removed |
| OpenCode: open Settings → Omni | Lists ChatGPT Pro/Plus, headless, API keys (Anthropic, OpenAI, ...), GitHub Copilot, More providers |
| OpenCode: Anthropic → Add key → Save | Stored in OpenCode's `auth.json`; Anthropic shows Connected |
| OpenCode: Anthropic → Sign out | Only that credential removed |
| Omni slide-over (`--omni`) while signed out | Shows the same setup instead of a chat |
| Leak check | The dummy keys appear nowhere in Litecord's data folder or log |

Not verified: completing a real ChatGPT/OAuth sign-in (needs a person and
an account) and device-code flows (auth hosts are blocked in this sandbox).

Screenshot hooks: `LITECORD_SCREENSHOT_SECTION=Omni` opens one Settings
section and `LITECORD_SCREENSHOT_DELAY=<secs>` waits longer before capture.

## Remaining work

Finish fidelity against the mock, accessibility/keyboard review, long-history
and resize stress testing, and sustained performance profiling. Native media,
channel-message transport, in-room files/messages, generative Omni features,
device discovery and real SDK authentication still require their named backend
services. Task/reminder creation forms and OS notifications remain absent.

## Reproduce native screenshots

```sh
cargo run -p litecord-desktop --features screenshots -- gui --screen Friends --screenshot /absolute/path/friends.png
cargo run -p litecord-desktop --features screenshots -- gui --width 760 --screenshot /absolute/path/messages-narrow.png
cargo run -p litecord-ui --example layout_preview --features screenshots -- /absolute/path/horizontal-layout.png
```

Screenshot capture is an optional QA feature; the normal command is
`cargo run -p litecord-desktop --features gui -- gui`.
