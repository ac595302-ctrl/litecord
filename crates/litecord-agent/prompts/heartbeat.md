Scheduled check-in ({{at}}), not a user message; the user is probably not watching.
Changes since the last check-in (revision {{from}} → {{to}}):
{{delta}}
1. If nothing needs the user's attention, reply exactly `HEARTBEAT_OK` and stop.
2. Otherwise call `compile_context` about what changed, then make at most {{max_actions}} local writes (tasks, reminders, drafts). Do not propose Discord actions from a check-in{{proposals_note}}.
3. End with at most 3 short bullets for the user's inbox.
