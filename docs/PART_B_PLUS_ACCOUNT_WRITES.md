# Experimental Discord account actions

This pass adds message send, reply, edit, delete, presence updates and relationship
actions to the existing account adapter.
All UI writes go through the Action Engine. REST confirmations are translated to
source-labelled events and await the canonical reducer transaction. The adapter
never writes SQLite directly. Gateway echoes use the existing canonical message ID
and deduplication path.

## Run and sign in

CI produces ready-to-open macOS Apple Silicon and Windows x64 desktop archives
only after all checks pass. Opening the packaged app selects account mode with a
dedicated per-user account data directory and shows Settings when sign-in is needed.
The macOS build is ad-hoc signed, not notarized; Windows has no publisher signature.

For a source build, build the desktop with `--features gui,discord-user-session`, then launch using
`--config config/litecord.account.example.toml --backend user-session gui`.
Use a dedicated account database. In Settings, paste an account-owner supplied
session credential into the masked field and select **Sign in**. The adapter
validates `/users/@me`, rejects bot accounts and account changes, saves the credential
in the OS keyring, and starts the Gateway and normal hydration/recovery workers.
This is an experimental credential sign-in flow, not Discord OAuth or an
email/password login. Live account compatibility still requires owner testing.

`[backend] access = "read_write"` enables implemented account operations;
`"read_only"` disables them at the transport, backend and Action Engine.
Desktop configuration defaults to read/write; direct backend constructors and the
legacy transport constructor retain their read-only default. Capabilities report
writes only while an authenticated credential is present. Settings and the
message composer reflect those capabilities and use the account source identity.
Settings also has an **Allow account writes** switch. It waits for an active
mutation to finish, blocks future mutations and pending approvals immediately, and
saves the choice across restarts. Explicit read-only configuration cannot be
overridden by the switch.

The HTTP transport accepts only the implemented numeric message routes, DM
creation route and numeric relationship routes for mutations. Edit/delete receive the canonical conversation ID
from the executor and re-fetch the remote message to verify its author and channel.
The Action Engine also validates ownership and retains approval for agent actions.
Logout waits for active mutations, stops the driver and clears memory before
attempting credential deletion. A deletion error is visible and never publishes
LoggedOut. Failed first-time credential storage never pins an account.

## Delivery uncertainty and remaining scope

Sends use a random nonce and `enforce_nonce` and transmit once. No automatic write
retry occurs. Network failures or local confirmation failures must be reconciled
by refreshing history before a manual resend. Existing proposals/audit records
retain the requested action; the durable outbound-operation ledger records submitted, confirmed, uncertain and failed
operations. Exact account/channel/nonce observations from Gateway or REST reconcile
uncertain sends. Interrupted submissions become uncertain on the next launch.

Relationship actions revalidate the current relationship before mutation and
refresh it afterward. Matching account observations can settle an uncertain
relationship action. Presence updates use Gateway opcode 3 and a five-second
cooldown. Discord provides no separate acknowledgement for that opcode; the local
presence reflects successful submission and can be refined by later events.
Omni and MCP proposals default to the account identity when an account is bound,
and still require approval before execution.

Reactions, typing, voice, moderation, attachments and additional account operations
remain unsupported. They are not advertised as capabilities. No owner credential
was supplied for live verification. These account endpoints are experimental and
are not an officially supported Discord account API.

## Verification

Regression tests cover credential deletion failure and retry, first-pin persistence
failure, transport mode/path gates, and account sign-in → send/reply/edit/delete →
canonical persistence → logout, presence submission, relationship transitions and
late receipt reconciliation. CI on main runs formatting, strict all-feature
Clippy and workspace tests on Linux, macOS and Windows. Check the exact commit's
CI result before treating a new build as verified.
