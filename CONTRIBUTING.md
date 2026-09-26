# Contributing

## Prerequisites

* Stable Rust (see CI), no other system dependencies: SQLite is bundled.
* The Discord Social SDK is **optional** and only needed for
  `--features discord-social-sdk` (see `crates/discord-ffi/README.md`).

## Everyday commands

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo run -p litecord-desktop -- demo      # headless demo over synthetic data
```

## Rules of the road

* Respect the dependency direction in `ARCHITECTURE.md`. New crates need a
  clear responsibility; do not add crates for their own sake.
* All SQL goes in `litecord-store::repos`. Schema changes are new files in
  `migrations/` (never edit an applied migration) plus an entry in
  `litecord_store::migrations::MIGRATIONS`.
* Canonical Discord state changes only through the reducer.
* Anything that mutates Discord goes through `litecord-actions`.
* No `unwrap()`/`expect()` in runtime paths (clippy warns; CI denies).
* Never log secrets or message content.
* Long-lived tasks: spawn via `TaskSupervisor` with a name.
* Unsupported Discord features are modelled (`SupportLevel`,
  `ActionResult::OpenDiscord`), never faked.
* Tests: prefer behavior-level tests at boundaries (see
  `crates/*/tests/`). Tests must not need Discord credentials.
* Keep `docs/IMPLEMENTATION_STATUS.md` honest: do not mark something
  implemented because an interface exists.
