# discord-ffi

The C ABI boundary to the official Discord Social SDK. This is the only crate
in the Litecord workspace allowed to reference native Discord Social SDK
symbols, and the only crate allowed `unsafe` code (the workspace lints deny
`unsafe_code` everywhere else — see the root `Cargo.toml`).

## The SDK is proprietary and is not redistributed here

The Discord Social SDK is proprietary. **It is not, and will never be,
vendored into or redistributed by this repository.** Anyone who wants to
build with real Discord connectivity must obtain the SDK themselves from
Discord directly and vendor it locally; everyone else can build and test this
entire workspace without it (see "Building without the SDK" below).

## Obtaining and vendoring the SDK

1. Obtain the Discord Social SDK for your platform from Discord (developer
   portal / partner program access is required).
2. Extract it so that the following layout exists relative to the repository
   root:

   ```
   vendor/discord-sdk/
     include/            # discordpp.h and friends
     lib/release/        # libdiscord_partner_sdk.* (or platform equivalent)
   ```

   See `vendor/discord-sdk/README.md` for the exact expected layout; that
   directory is `.gitignore`d (except for its own README) so nothing
   proprietary is ever committed.

3. Point the build at it:

   ```sh
   export LITECORD_DISCORD_SDK_DIR="$(pwd)/vendor/discord-sdk"
   ```

4. Build with the feature enabled:

   ```sh
   cargo build -p discord-ffi -p discord-adapter --features discord-adapter/discord-social-sdk
   ```

## Feature flag

* `default = []` — no native code is compiled or linked. `SDK_LINKED` is
  `false`. This is what CI, `cargo test`, and anyone without the SDK use.
* `discord-social-sdk` — compiles `native/discord_bridge.cpp` (a thin C++
  shim over the SDK's `discordpp.h`) and links against the vendored SDK
  library. Requires `LITECORD_DISCORD_SDK_DIR` to be set at build time (see
  above); if the feature is enabled without that env var, `build.rs` emits a
  `cargo:warning` and does nothing else, so `cargo check`/`cargo clippy
  --all-features` remain green without the SDK present. Actually linking a
  binary in that configuration will fail, which is expected.

## Building without the SDK

This is the default and the CI configuration. `discord-ffi` compiles to plain,
safe Rust (the `#[repr(C)]` mirror types and `LcStr` conversion helpers are
always compiled; the `extern "C"` declarations and the `Bridge` wrapper are
not). `discord-adapter`'s `SocialSdkBackend` is a documented skeleton in this
configuration; the mock backend (`discord-adapter::mock::MockBackend`) is
fully functional and is what demo/dev builds should use.

## Status

The C++ implementation in `native/discord_bridge.cpp` is a sketch written
against Discord Social SDK 1.x headers as documented at the time it was
written. **It has not been compiled or run against a real SDK checkout** and
must be verified (and very likely adjusted) against whatever SDK version is
actually vendored before it is trusted. See the header comment at the top of
that file and `docs/IMPLEMENTATION_STATUS.md`.
