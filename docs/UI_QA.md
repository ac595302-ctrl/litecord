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
Memory attribution and optimization remain necessary. In particular, the
bridge currently refreshes all bounded screen snapshots, including hidden
destinations; event bursts are drained with a bounded coalescing pass.

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
