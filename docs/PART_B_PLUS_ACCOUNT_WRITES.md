# Experimental account message writes

This pass adds message send, reply, edit, and delete to the existing account adapter.
All UI writes go through the Action Engine. REST confirmations are translated to
source-labelled events and await the canonical reducer transaction. The adapter
never writes SQLite directly. Gateway echoes use the existing canonical message ID
and deduplication path.

## Run and sign in

Build the desktop with `--features gui,discord-user-session`, then launch using
`--config config/litecord.account.example.toml --backend user-session`.
Use a dedicated account database. In Settings, paste an account-owner supplied
session credential into the masked field and select **Sign in**. The adapter
validates `/users/@me`, rejects bot accounts and account changes, saves the credential
in the OS keyring, and starts the Gateway and normal hydration/recovery workers.
This is an experimental credential sign-in flow, not Discord OAuth or an
email/password login. Live account compatibility still requires owner testing.

`[backend] access = "read_write"` enables implemented message operations;
`"read_only"` disables them at the transport, backend and Action Engine.
Desktop configuration defaults to read/write; direct backend constructors and the
legacy transport constructor retain their read-only default. Capabilities report
writes only while an authenticated credential is present. Settings and the
message composer reflect those capabilities and use the account source identity.

The HTTP transport accepts only the implemented numeric message routes and DM
creation route for mutations. Edit/delete receive the canonical conversation ID
from the executor and re-fetch the remote message to verify its author and channel.
The Action Engine also validates ownership and retains approval for agent actions.
Logout waits for active mutations, stops the driver and clears memory before
attempting credential deletion. A deletion error is visible and never publishes
LoggedOut. Failed first-time credential storage never pins an account.

## Delivery uncertainty and remaining scope

Sends use a random nonce and `enforce_nonce` and transmit once. No automatic write
retry occurs. Network failures or local confirmation failures must be reconciled
by refreshing history before a manual resend. Existing proposals/audit records
retain the requested action; a dedicated durable outbound-operation ledger and
nonce-based reconciliation have **not** been implemented in this pass.

Presence writes, relationship writes, reactions, typing, voice, moderation,
attachments and additional account operations remain unsupported. They are not
advertised as capabilities. This is the first message-write increment, not complete
Discord feature coverage. No owner credential was supplied for live verification.

## Verification

Regression tests cover credential deletion failure and retry, first-pin persistence
failure, transport mode/path gates, and account sign-in → send/reply/edit/delete →
canonical persistence → logout. CI on main runs formatting, strict all-feature
Clippy and workspace tests on Linux, macOS and Windows. Check the exact commit's
CI result before treating a new build as verified.
