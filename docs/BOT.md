# Optional application-bot source

The Social SDK cannot read arbitrary guild channel history. For guilds where
**you** install your own Discord bot, Litecord can use it as a second,
clearly separated source.

* Everything it observes is stored with origin `discord_bot_gateway`
  (identity `application_bot`); it never merges with your user identity.
* Guild text channels, announcement channels and threads it can read become
  conversations (`kind = guild_channel`), with history, search, memory and
  agent context like DMs — subject to the same per-conversation agent
  visibility settings.
* Sending in those channels happens **as the bot**, only when you choose it:
  the composer reports `send_identity = application_bot`, agent proposals
  carry the identity, and the approval dialog must show it
  (`PendingActionRow.identity`). The bot is limited to posting, editing and
  deleting its own messages in guild channels; it never touches your
  friends, DMs or presence. Posts are sent with `allowed_mentions: none`.

## Try it without Discord

```toml
# litecord.toml
[backend]
demo_bot = true
```

`MockBackend::demo_bot` shows the demo guilds through a synthetic bot.

## Real bot setup

1. Discord Developer Portal → your application → **Bot** → create a bot,
   copy its token.
2. Enable the **Message Content** privileged intent (needed to read message
   text). Server Members / Presence intents are not required.
3. OAuth2 → URL generator → scope `bot`, permissions *View Channels*,
   *Send Messages*, *Read Message History*, *Manage Messages* (only if you
   want to delete others' messages; not needed otherwise). Invite it to your
   own server(s).
4. Build and run with the feature and the token in the environment (never in
   a config file):

   ```sh
   cargo run -p litecord-desktop --features gui,discord-bot -- gui
   LITECORD_BOT_TOKEN=... ./target/release/litecord gui
   ```

   Use only in servers where you are allowed to run a bot, and follow
   Discord's Developer Terms and bot policies.

## Architecture

```text
Discord gateway (wss) ─► HttpTransport ─► bot::gateway::GatewaySession (pure state machine:
                                              hello/identify/heartbeat/resume/reconnect)
                                        ─► bot::translate::dispatch ─► SourceEnvelope{BotGateway}
Discord REST ◄──────── bot::rest (builders, parsing, error/rate-limit mapping)
BotBackend (SocialBackend) owns the driver task; the app gives it its own hydrator and
session state (`bot_session_state`), and the executor picks it for `application_bot` proposals.
```

Status: protocol, session, backend and app integration are covered by
deterministic tests with a scripted transport. The real network transport
compiles and is clippy-clean but has **not** been exercised against live
Discord in CI (no token there).
