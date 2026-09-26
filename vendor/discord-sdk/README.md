# vendor/discord-sdk/

This directory is where a locally obtained copy of the (proprietary) Discord
Social SDK is vendored, for anyone who wants to build Litecord with real
Discord connectivity (the `discord-social-sdk` Cargo feature).

**Nothing in this directory except this README is committed to the
repository.** The root `.gitignore` ignores everything else under
`vendor/discord-sdk/`. The SDK is proprietary and must be obtained directly
from Discord; see `crates/discord-ffi/README.md` for the full instructions.

## Expected layout

```
vendor/discord-sdk/
  include/            # discordpp.h and the rest of the SDK's C++ headers
  lib/release/         # libdiscord_partner_sdk.* (platform-specific)
```

`crates/discord-ffi/build.rs` reads `$LITECORD_DISCORD_SDK_DIR/include` for
headers and links against `$LITECORD_DISCORD_SDK_DIR/lib/release`. Set
`LITECORD_DISCORD_SDK_DIR` to the absolute path of this directory (or wherever
you extracted the SDK) before building with `--features
discord-adapter/discord-social-sdk`.

If this directory is empty (the default, e.g. on CI), the workspace still
builds and tests fully: `discord-ffi`'s native bridge simply isn't compiled,
and `discord-adapter`'s mock backend provides full demo functionality without
any native dependency.
