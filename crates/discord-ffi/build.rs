//! Conditionally compiles and links the native Discord Social SDK bridge.
//!
//! Behavior matrix:
//!
//! * feature `discord-social-sdk` OFF: this script does nothing. The crate
//!   builds as plain Rust with no native code and no link requirements. Note
//!   that `cc` is an *optional* build-dependency, so its API must not be
//!   referenced at all when the feature is off — that whole code path is
//!   behind `#[cfg(feature = "discord-social-sdk")]` below, not just an `if`.
//! * feature ON, `LITECORD_DISCORD_SDK_DIR` unset: we cannot compile against
//!   headers we don't have, so we emit a `cargo:warning` explaining that and
//!   stop. This keeps `cargo check`/`cargo clippy --all-features` green on CI
//!   machines that do not (and must not) have the proprietary SDK vendored.
//!   Actually linking a binary in this configuration will fail at link time,
//!   which is expected and correct.
//! * feature ON, env var set: compile `native/discord_bridge.cpp` against the
//!   vendored SDK headers and link against the vendored SDK library.

const SDK_DIR_ENV: &str = "LITECORD_DISCORD_SDK_DIR";

fn main() {
    println!("cargo:rerun-if-env-changed={SDK_DIR_ENV}");
    println!("cargo:rerun-if-changed=native/discord_bridge.cpp");
    println!("cargo:rerun-if-changed=native/discord_bridge.h");

    #[cfg(feature = "discord-social-sdk")]
    compile_native();
}

#[cfg(feature = "discord-social-sdk")]
fn compile_native() {
    use std::env;
    use std::path::PathBuf;

    let Some(sdk_dir) = env::var_os(SDK_DIR_ENV) else {
        println!(
            "cargo:warning=discord-ffi: feature `discord-social-sdk` is enabled but \
             {SDK_DIR_ENV} is not set. The native bridge will NOT be compiled and \
             linking any binary that needs it will fail. Vendor the Discord Social \
             SDK (see crates/discord-ffi/README.md) and set {SDK_DIR_ENV} to its \
             root to enable real linking. `cargo check`/`cargo clippy` remain \
             usable without it."
        );
        return;
    };

    let sdk_dir = PathBuf::from(sdk_dir);
    let include_dir = sdk_dir.join("include");
    let lib_dir = sdk_dir.join("lib").join("release");

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("native/discord_bridge.cpp")
        .include(&include_dir)
        .include("native")
        .warnings(true)
        .compile("discord_bridge");

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=discord_partner_sdk");
}
