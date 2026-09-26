# Litecord roadmap: data breadth, efficiency, Omni

Agreed September 26, 2026. Each stage ships separately with tests and an
entry in `HANDOFF.md`.

## Not planned: user-account session (self-bot)

Litecord will not connect as the user's own account through the Gateway or
REST with a user token. Discord prohibits automating user accounts outside
OAuth2 and the bot API. Account breadth comes from the supported sources
below instead.

## 1. Omni: full Codex/OpenCode integration and automations (done)

- **Every sign-in method** the harness offers:
  - browser OAuth;
  - OpenCode providers that need a pasted code;
  - API keys, passed straight to the harness and never stored by Litecord.
  - The list of methods comes from the harness at runtime.
- **Model choice** from the harness's own model list, saved per install.
- **Headless CLI:**
  - `litecord omni status | login | logout | models`;
  - `litecord omni doctor`, which checks the installed harness version and
    its protocol schema against the method names the drivers use.
- **Automations:** user-defined Omni jobs.
  - Triggers:
    - a time of day on chosen weekdays;
    - every N hours;
    - a new DM from a chosen person;
    - a message containing a keyword.
  - Output goes to the Inbox, to suggested tasks, or to drafts.
  - Omni never sends anything itself; Discord writes still need approval.
  - Runs are in Assistant mode only, under a global hourly limit and quiet
    hours.
  - A reply of exactly `AUTOMATION_OK` means "nothing to report".
  - Presets: Morning brief, Reply radar, Commitment tracker, Weekly people
    digest.

## 2. Stage A: efficiency baseline

- Snapshots only for the visible destination; refresh on data change.
- Byte budgets on the ingest queue and the message cache (2–4 MiB for the
  active timeline, next to the 200-message window).
- Split large startup snapshots into bounded batches.
- Show byte usage in Diagnostics.
- Re-measure memory under the same workload as `UI_QA.md` (137.82 MiB).

## 3. Stage C: paged history and resumable sync

- **History pages:** `HistoryPageRequest { conversation_id, before, after, limit }`
  returns `HistoryPage { messages, oldest, newest, has_more }`.
- **Checkpoints:** each page and its checkpoint are saved together in
  `sync_state`.
- **Backfill** runs at background priority, one conversation at a time:
  - only for conversations the user selected;
  - pauses while the user is active, under memory pressure, or when
    rate-limited.
- **Catch-up after reconnect** starts from the last saved cursor.
- **Disk quota** and a retention setting are required before whole-account
  sync is offered.
- **Sources:** the bot (full history of servers where it is installed) now;
  the Social SDK once it is connected.

## 4. Discord data export importer

- **Input:** the zip Discord emails from Settings → Data & Privacy → Request
  all of my data. It contains the user's own messages in every DM, group and
  server, plus servers and account details.
- **Labelling:** everything is stored with `Origin::Imported`.
- **Process:** a resumable background job whose results go through the
  reducer.

## 5. Multi-source provenance

A message observed by more than one source (SDK, bot, import) is stored once,
and every source that observed it is recorded.

## Invariants for every stage

- The reducer is the only writer of Discord data.
- One app process and one database.
- Attachments stay metadata until the user opens them.
- Message edits and deletes are never dropped or merged away.
- A sync checkpoint never moves past uncommitted data.
- Credentials never appear in the UI, logs, events or MCP.
