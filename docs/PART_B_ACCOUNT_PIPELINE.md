> Message-write extension: see [PART_B_PLUS_ACCOUNT_WRITES.md](PART_B_PLUS_ACCOUNT_WRITES.md).
> The read-only descriptions below document the original Part B baseline.

# Part B: experimental account data pipeline

Part B adds an opt-in, read-only Discord user-session source. It uses the
existing app/reducer pipeline; the account adapter does not write directly to
SQLite. This path is experimental. The automated tests use scripted HTTP and
Gateway transports, and do not establish that a real Discord account can
connect successfully.

Discord ingestion runs inside the native Rust desktop process: egui renders
the frontend, the Tokio app layer schedules I/O/work, and SQLite stores
canonical state, FTS search, memory, and checkpoints. There is no separate
Discord client, data service, or second database. Existing optional Omni/MCP
harness subprocesses provide AI tooling and are not Discord data sources.

## End-to-end path

```text
Settings (email/password + optional TOTP, or advanced credential field)
  └─ DiscordPasswordLogin → Discord auth exchange → ConnectSession(Secret<String>)
     or ConnectSession(Secret<String>) directly
       └─ LitecordApp::authenticate_session
            └─ UserSessionBackend
                 ├─ GET /users/@me validates a non-bot account and account binding
                 ├─ OS keyring stores discord.user_session_token
                 └─ read-only REST + Gateway v10 driver
                      └─ normalized SourceEnvelope(UserSession, DiscordEvent)
                           └─ bounded ingest queue
                                └─ event reactor → reducer transaction → SQLite
                                     └─ revisioned app event → UI snapshots

Account forward recovery:
  account conversation snapshots → verified GET-after pages
    → MessagesCatchupPage → committed ingest acknowledgement
      → message upserts + independent forward watermark in one transaction

Stage C history sync:
  page GET → MessagesPage → committed ingest acknowledgement
            → message upserts + backward-history checkpoint in one transaction
```

The desktop binary constructs the adapter only when its `discord-user-session`
feature is enabled and `backend.kind` selects `user_session`. `Settings`
submits login inputs through typed app commands; they are not normal
settings. The experimental password exchange uses Discord's unsupported
account-login endpoint and supports an authenticator TOTP challenge. It keeps
the MFA ticket in memory and passes a resulting token to the same credential
validation path. CAPTCHA and other MFA methods are not implemented. The
backend's `AuthStep::SessionCredential` remains a separate contract from OAuth
or a browser redirect. The adapter first validates the credential
with `GET /users/@me`, rejects bot accounts, and checks that the account
matches the account already bound to the database. Only after that check does
it save the credential through the OS keyring implementation and start the
Gateway driver. At startup it reads the saved entry and verifies the account
again before starting the Gateway or parallel hydration. Logging out clears
the in-memory credential and removes the keyring entry; the database's
account pin remains, so a later login cannot silently switch account
identities.

The keyring service namespace is tied to the absolute database path, so separate
Litecord databases use separate session entries. Keep the account database in a
dedicated `data_dir`; the app refuses to attach a user session to a database
whose current account is synthetic demo data, and it refuses a later account
switch in the same database. The source is also distinct in stored provenance:
`DiscordSource::UserSession`, `DiscordIdentity::UserSession`, and
`Origin::DiscordUserSession`. These labels keep account observations separate
from synthetic fixtures, Social SDK observations, and application-bot data.
Migration `0008` records message observations by message ID and origin,
preserving when each source first and last saw a message and whether that
source observed its deletion. Guild and channel memberships are also tracked
per source. A snapshot from one source retires only that source's memberships;
a shared canonical guild or channel remains active while another source still
retains it.

The password is masked in Settings and removed from its UI draft on submission.
Email, password, TOTP code, and optional session credential enter the bridge in
`Secret<String>` wrappers. `Secret` redacts its debug output and does not
implement serialization. Only a validated session credential is persisted to
the OS keyring; login inputs are not written to the TOML config, ordinary app
settings, or SQLite. The example config contains no credential.

## Build and run

Run these commands from the repository root. The example config uses a
dedicated `.litecord-account` data directory; the directory and database are
created on first start.

```powershell
cargo build -p litecord-desktop --features "gui,discord-user-session" --offline
cargo run -p litecord-desktop --features "gui,discord-user-session" --offline -- --config config/litecord.account.example.toml gui
```

Open **Settings**, enter your Discord email or verified phone and password,
then choose **Sign in to Discord**. If prompted, enter the authenticator code.
The previous session-credential field remains under **Advanced**. The account
identity, connection state, and source are shown in the card. This path is
experimental and has not been verified against a real account. The same config
may be selected with `--backend user-session` if a CLI override is preferred.

The `discord-user-session` feature also enables the optional network stack and
the platform OS-keyring implementation. If keyring access is unavailable,
authentication reports a storage error. The credential is not accepted in a
config field or command-line argument. This command is account-only. The
optional bot source uses the separate `discord-bot` feature and also requires
`LITECORD_BOT_TOKEN`.

## Read-only boundary and protocol caveats

The account HTTP transport rejects every REST method except `GET`. The
adapter advertises read capabilities as partial, disables the channel write
bits it receives, and the app suppresses the account-session send identity.
There are no account message-send, edit, delete, or relationship-write
operations in this source. Data visibility depends on the account and on
Discord's current API behavior; unavailable data may remain absent or produce
an unsupported/permission error.

The implemented REST reads cover the current user and profiles, relationships,
guild list and channel metadata, private-channel summaries, and paged channel
messages. The transport rejects `POST`, `PATCH`, and `DELETE` for this source.
The account message-update path may fetch the full message after receiving a
partial Gateway update; that is still a `GET` and its result goes through the
same reducer.

The Gateway driver reuses Litecord's tested state machine for Hello, Identify,
Resume, sequence tracking, heartbeat ACKs, and reconnect handling. User
Identify omits bot intents and disables compression. A missing Gateway
heartbeat ACK is a transport-level signal to reconnect; it is separate from
Litecord's ingest commit acknowledgement described below. Account-session
Gateway compatibility has not been verified against Discord with a real
account. Discord may restrict this access, and protocol or account-policy
changes may break it.

## Bounded ingest, commit acknowledgement, and session epochs

The app ingest queue is bounded by both event count and approximate bytes. Its
defaults are 1,024 events and 8 MiB (`runtime.event_queue_capacity` and
`runtime.event_queue_max_bytes`). Account message create/update/delete events,
current-user changes, and selected session changes wait for a committed
reducer result, with a five-second deadline. If one cannot commit, the driver
stops and reports that reconnection is required for reconciliation.
Metadata-only events use non-blocking `try_send`. If a count or byte limit is
reached, the event is dropped, the overflow counter/flag is updated, and the
event reactor publishes `ResyncRequired` and schedules reconciliation.
Hydration producers that can wait use the async queue send path. App broadcasts
to UI subscribers are separately bounded (`broadcast_capacity`, default 256); a lagging
subscriber reloads revisioned views from the store.

For operations that need durable progress, `send_committed` waits for the
reducer to return the SQLite commit result and revision. Stage C uses this path
for every `MessagesPage`: message upserts and its backward-history cursor share
one transaction, so the checkpoint cannot move past an uncommitted page.
Account forward-recovery pages use it too, and the full-message GET requested
after a partial Gateway update waits for its commit. The account's forward
REST watermark is a third, independent progress marker: live Gateway events
never advance it, and an account `MessagesSnapshot` only seeds a missing
watermark rather than moving an established one. A later live message
therefore cannot conceal a dropped earlier event. Each `MessagesCatchupPage`
commits its message upserts and verified forward watermark atomically.

The account backend increments a session generation when stopping or signing
out. Gateway, Stage C history, and account-recovery senders stamp deliveries
with that generation; the receiver drops queued deliveries whose epoch is
stale. The REST path also checks the generation around in-flight requests and
both history paths use the same session guard. This prevents stale driver and
page results from being applied after logout or a session change.

## Account forward recovery

Migration `0008_account_recovery.sql` adds an account-specific forward
watermark for each known conversation and per-origin message observations.
Existing account messages seed the watermark during migration; incoming
account `MessagesSnapshot` events seed it for newly discovered conversations.
When the account session is Ready, the background `account-recovery` worker
checks one due conversation and requests one REST page after its newest
verified message ID (or the latest page if no watermark exists). It routes the
page as `MessagesCatchupPage` through the same reducer. If more pages remain,
the cursor becomes due for the next one-page step; otherwise that conversation
is checked again after 30 seconds. Reconnect marks coverage due again without
discarding rate-limit or permission cooldowns.

The worker yields under the same database quota, resident-memory threshold,
and recent-UI-activity checks used by Stage C. Rate limits use Discord's retry
delay; unavailable or forbidden conversations cool down for an hour, and
other failures are retried later. This forward verification is separate from
Stage C's user-selected backward history walk: each has its own cursor and
transaction event, even though both use the backend's paged GET interface.

## Stage C history sync

`UserSessionBackend::history_page` implements cursor-based GET paging for
conversations the account can read. Page size is clamped to Discord's 1–100
range. The existing user-selected Stage C worker processes one conversation
at a time, waits between pages, and uses the same reducer and checkpoint path
as other sources. It resumes from the saved cursor after restart and pauses
while the UI is active, under the configured memory threshold, at the database
quota, or after a rate-limit response. Set
`retention.max_database_mb = 0` to disable whole-history sync; otherwise the
configured quota bounds it. The account adapter is wired into this path, but
the Stage C integration tests use `MockBackend`, not a live account transport.

## Verification coverage and limits

| Area | Automated coverage | Boundary |
| --- | --- | --- |
| Gateway state machine | Deterministic protocol tests for Identify/Resume, sequence numbers, heartbeat ACKs, reconnects, and close handling in `discord-adapter/src/bot/gateway.rs`. | Scripted frames; no live account handshake. |
| Account ingest | `crates/litecord-app/tests/user_session.rs` scripts REST responses and Gateway frames, checks source provenance for create/update/delete, sign-out cleanup, GET-only requests, and rejection of bot or mismatched accounts. | Uses fake transport and an in-memory secret store; does not validate a real HTTP/WebSocket connection or platform keyring. |
| Reducer provenance and membership | `crates/litecord-store/tests/reducer.rs` checks message-source labels/tombstones and source-scoped guild/channel membership; migrations `0006` and `0007` carry those storage changes. | Scripted database transactions; no live source reconciliation. |
| Queue and epoch | `crates/litecord-core/src/bus.rs` tests count/byte bounds, overflow signaling, commit acknowledgement success/failure, and stale-epoch delivery rejection. | Exercises the queue contract in process. |
| Account forward recovery | `crates/litecord-store/tests/account_recovery.rs` covers atomic message/watermark commits, live high IDs not advancing verified coverage, snapshot seeding, and independent origins; `crates/litecord-app/tests/user_session.rs` scripts reconnect catch-up. | Fake transport and database tests; no live account recovery run. |
| Stage C history | `crates/litecord-app/tests/history_sync.rs` covers page completion, restart from checkpoint, idle pausing/stopping, and quota/conversation checks. | Uses the synthetic `MockBackend`; not an account-source history test. |
| Settings secret handling | UI tests verify the credential is absent from rendered text and `ConnectSession` debug output is redacted. | Does not test OS keyring behavior or live authentication. |

Run the offline workspace checks from the repository root with the portable
build environment configured:

```powershell
cargo check --workspace --offline
cargo test -p discord-adapter -p litecord-core -p litecord-store -p litecord-app -p litecord-ui --offline
cargo check -p litecord-desktop --features "gui,discord-user-session" --offline
```

No test in this matrix claims a successful live Discord account login. The
production Gateway and REST compatibility, actual account visibility, and
OS-specific keyring behavior remain environment-dependent verification items.

On this Windows desktop, an explicit native Credential Manager round-trip
passed with a temporary dummy credential, including removal afterward. The
default suite ignores that test because CI/container native keyrings may be
unavailable. Run it on the target desktop with:

```powershell
cargo test -p litecord-core --features os-keychain native_credential_round_trip_and_cleanup -- --ignored
```

The merged-main GUI/account workspace tests and strict all-target/all-feature
Clippy passed. The account test layers cover three transport/lifecycle cases,
three isolation cases, and five transactional recovery/provenance cases.
The Windows release build starts a fresh account database, migrates through
version 8, renders Settings, and joins all supervised tasks on exit.

Forward verification currently tracks conversations whose recent windows are
cached. It can recover missed creations after its persisted watermark;
Gateway resume and recent-window reconciliation cover additional live changes.
It cannot infer every old edit/deletion while disconnected, discover every
thread/channel subscription, or promise a complete account archive. The quota
pauses background history and recovery; it is not a hard live-ingest disk cap.
Historical `MessageImported` event classification does not change source
provenance: account history remains `Origin::DiscordUserSession`.
