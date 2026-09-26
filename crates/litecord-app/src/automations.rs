//! Omni automations: user-defined jobs that run on a schedule or when a
//! matching message arrives (docs/ROADMAP.md §1).
//!
//! Every run happens in the automation's own rolling Assistant-mode
//! session, so it can read Litecord memory through the tools but never run
//! local commands. Discord writes stay proposals; the prompt forbids them
//! and the Action Engine still requires approval. Results that are not
//! `AUTOMATION_OK` show up in the Inbox.

use serde::{Deserialize, Serialize};

use litecord_agent::prompts::{AutomationOutput, AutomationPrompt};
use litecord_core::error::{Error, Result};
use litecord_harness::{LoginState, OmniMode};
use litecord_store::repos;
use litecord_store::repos::omni::{AutomationRecord, NewSession};
use litecord_types::provenance::DiscordIdentity;
use litecord_types::social::ConversationKind;
use litecord_types::trust::AgentVisibility;
use litecord_types::{Revision, Timestamp, UserId};

use crate::omni::{in_quiet_hours, OmniService};

/// What makes an automation run. Times are UTC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutomationTrigger {
    /// At `minute_of_day` (0..1440) on the days in `weekdays` (bit 0 =
    /// Monday … bit 6 = Sunday; 0 = every day).
    Daily { minute_of_day: u16, weekdays: u8 },
    /// Every `hours` hours (1..=168).
    Every { hours: u16 },
    /// A new message from this person in your DM with them.
    DirectMessageFrom { user_id: UserId },
    /// A new message (not yours) containing `keyword` (case-insensitive).
    Keyword { keyword: String },
}

impl AutomationTrigger {
    pub fn describe(&self) -> String {
        match self {
            Self::Daily {
                minute_of_day,
                weekdays,
            } => {
                let days = match *weekdays {
                    0 | 0x7f => "every day".to_owned(),
                    0x1f => "on weekdays".to_owned(),
                    mask => {
                        const NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
                        let days: Vec<&str> = (0..7)
                            .filter(|i| mask & (1 << i) != 0)
                            .map(|i| NAMES[i])
                            .collect();
                        format!("on {}", days.join(", "))
                    }
                };
                format!(
                    "daily at {:02}:{:02} UTC {days}",
                    minute_of_day / 60,
                    minute_of_day % 60
                )
            }
            Self::Every { hours } => format!("every {hours} h"),
            Self::DirectMessageFrom { .. } => "a new direct message".into(),
            Self::Keyword { keyword } => format!("a message mentioning \"{keyword}\""),
        }
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::Daily { minute_of_day, .. } if *minute_of_day >= 1440 => {
                Err(Error::validation("time of day must be before 24:00"))
            }
            Self::Every { hours } if !(1..=168).contains(hours) => {
                Err(Error::validation("interval must be 1 to 168 hours"))
            }
            Self::Keyword { keyword } if keyword.trim().chars().count() < 2 => {
                Err(Error::validation("keyword must have at least 2 characters"))
            }
            _ => Ok(()),
        }
    }

    fn is_event(&self) -> bool {
        matches!(self, Self::DirectMessageFrom { .. } | Self::Keyword { .. })
    }
}

/// Input for creating an automation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationDraft {
    pub name: String,
    /// What Omni should do, in the user's words.
    pub prompt: String,
    pub trigger: AutomationTrigger,
    pub output: AutomationOutputKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationOutputKind {
    Inbox,
    Tasks,
    Drafts,
}

impl AutomationOutputKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Tasks => "tasks",
            Self::Drafts => "drafts",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "tasks" => Self::Tasks,
            "drafts" => Self::Drafts,
            _ => Self::Inbox,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Inbox => "Summary in Inbox",
            Self::Tasks => "Suggested tasks",
            Self::Drafts => "Draft replies",
        }
    }

    fn prompt_output(self) -> AutomationOutput {
        match self {
            Self::Inbox => AutomationOutput::Inbox,
            Self::Tasks => AutomationOutput::Tasks,
            Self::Drafts => AutomationOutput::Drafts,
        }
    }
}

/// Ready-made automations offered in Settings.
pub fn presets() -> Vec<AutomationDraft> {
    vec![
        AutomationDraft {
            name: "Morning brief".into(),
            prompt: "Catch me up on what happened since yesterday: who messaged, what's \
                     waiting on me, and anything due today."
                .into(),
            trigger: AutomationTrigger::Daily {
                minute_of_day: 8 * 60,
                weekdays: 0,
            },
            output: AutomationOutputKind::Inbox,
        },
        AutomationDraft {
            name: "Reply radar".into(),
            prompt: "Find conversations where someone is waiting on my reply and draft a \
                     short, friendly answer for each."
                .into(),
            trigger: AutomationTrigger::Every { hours: 4 },
            output: AutomationOutputKind::Drafts,
        },
        AutomationDraft {
            name: "Commitment tracker".into(),
            prompt: "Look for things I promised to do in recent conversations and make sure \
                     each has a task with a sensible due date."
                .into(),
            trigger: AutomationTrigger::Daily {
                minute_of_day: 18 * 60,
                weekdays: 0x1f,
            },
            output: AutomationOutputKind::Tasks,
        },
        AutomationDraft {
            name: "Weekly people digest".into(),
            prompt: "Summarize my week with the people I talk to most: what we discussed, \
                     plans we made, and anyone I haven't replied to."
                .into(),
            trigger: AutomationTrigger::Daily {
                minute_of_day: 17 * 60,
                weekdays: 1 << 4,
            },
            output: AutomationOutputKind::Inbox,
        },
    ]
}

/// Row for the Settings list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutomationRow {
    pub id: i64,
    pub name: String,
    pub prompt: String,
    pub trigger: Option<AutomationTrigger>,
    pub trigger_label: String,
    pub output: AutomationOutputKind,
    pub enabled: bool,
    pub runs: u32,
    pub last_run_at: Option<Timestamp>,
    pub last_error: Option<String>,
    pub session_id: Option<i64>,
}

impl From<&AutomationRecord> for AutomationRow {
    fn from(a: &AutomationRecord) -> Self {
        let trigger: Option<AutomationTrigger> = serde_json::from_str(&a.trigger).ok();
        Self {
            id: a.id,
            name: a.name.clone(),
            prompt: a.prompt.clone(),
            trigger_label: trigger
                .as_ref()
                .map_or_else(|| "unknown trigger".into(), AutomationTrigger::describe),
            trigger,
            output: AutomationOutputKind::parse(&a.output),
            enabled: a.enabled,
            runs: a.runs,
            last_run_at: a.last_run_at,
            last_error: a.last_error.clone(),
            session_id: a.session_id,
        }
    }
}

/// Whether a time-based trigger is due at `now` given the last run.
/// Event triggers are never "due" by time.
pub fn schedule_due(
    trigger: &AutomationTrigger,
    last_run: Option<Timestamp>,
    now: Timestamp,
) -> bool {
    const DAY: i64 = 86_400_000;
    let now_ms = now.as_millis();
    match trigger {
        AutomationTrigger::Daily {
            minute_of_day,
            weekdays,
        } => {
            let day_start = now_ms.div_euclid(DAY) * DAY;
            let occurrence = day_start + i64::from(*minute_of_day) * 60_000;
            // 1970-01-01 was a Thursday (index 3 with Monday = 0).
            let weekday = (now_ms.div_euclid(DAY) + 3).rem_euclid(7);
            let day_ok = *weekdays == 0 || weekdays & (1 << weekday) != 0;
            day_ok
                && now_ms >= occurrence
                // Don't fire a long-missed slot hours later (e.g. after the
                // app was closed all day): only within 6 h of the time.
                && now_ms - occurrence < 6 * 3_600_000
                && last_run.is_none_or(|t| t.as_millis() < occurrence)
        }
        AutomationTrigger::Every { hours } => {
            last_run.is_none_or(|t| now_ms - t.as_millis() >= i64::from(*hours) * 3_600_000)
        }
        AutomationTrigger::DirectMessageFrom { .. } | AutomationTrigger::Keyword { .. } => false,
    }
}

/// Result of one automation tick for one automation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AutomationOutcome {
    Skipped(&'static str),
    Ran { automation_id: i64, session_id: i64 },
}

impl OmniService {
    pub fn automations(&self) -> Result<Vec<AutomationRow>> {
        Ok(self
            .db()
            .read(|r| repos::omni::automations(r))?
            .iter()
            .map(AutomationRow::from)
            .collect())
    }

    pub fn create_automation(&self, draft: &AutomationDraft) -> Result<i64> {
        let name = draft.name.trim();
        if name.is_empty() {
            return Err(Error::validation("give the automation a name"));
        }
        if draft.prompt.trim().is_empty() {
            return Err(Error::validation("tell Omni what to do"));
        }
        draft.trigger.validate()?;
        let trigger =
            serde_json::to_string(&draft.trigger).map_err(|e| Error::internal(e.to_string()))?;
        let revision = self.db().current_revision()?.get();
        let id = self
            .db()
            .write(|tx| {
                repos::omni::create_automation(
                    tx,
                    name,
                    draft.prompt.trim(),
                    &trigger,
                    draft.output.as_str(),
                    revision,
                )
            })?
            .value;
        self.notify_changed();
        Ok(id)
    }

    pub fn set_automation_enabled(&self, id: i64, enabled: bool) -> Result<()> {
        self.db()
            .write(|tx| repos::omni::set_automation_enabled(tx, id, enabled))?;
        self.notify_changed();
        Ok(())
    }

    pub fn delete_automation(&self, id: i64) -> Result<()> {
        self.db()
            .write(|tx| repos::omni::delete_automation(tx, id))?;
        self.notify_changed();
        Ok(())
    }

    /// Run one automation now, ignoring its schedule (not the sign-in,
    /// busy or rate checks).
    pub async fn run_automation(&self, id: i64) -> Result<AutomationOutcome> {
        let a = self
            .db()
            .read(|r| repos::omni::automation(r, id))?
            .ok_or_else(|| Error::not_found("automation"))?;
        self.run(&a, "run manually", "").await
    }

    /// Check every enabled automation; runs whatever is due. Called from the
    /// app's once-a-minute Omni loop.
    pub async fn automations_tick(&self) -> Result<Vec<AutomationOutcome>> {
        let now = self.db().now();
        let hb = self.config().heartbeat.clone();
        let hour = ((now.as_millis() / 3_600_000).rem_euclid(24)) as u8;
        let quiet = in_quiet_hours(hour, hb.quiet_start_hour, hb.quiet_end_hour);
        let mut out = Vec::new();
        for a in self.db().read(|r| repos::omni::automations(r))? {
            if !a.enabled {
                continue;
            }
            let Ok(trigger) = serde_json::from_str::<AutomationTrigger>(&a.trigger) else {
                continue;
            };
            if quiet {
                // Event cursors don't advance, so matches fire after quiet hours.
                out.push(AutomationOutcome::Skipped("quiet hours"));
                continue;
            }
            if trigger.is_event() {
                let (current, matches) = self.event_matches(&a, &trigger)?;
                if matches.is_empty() {
                    self.db()
                        .write(|tx| repos::omni::set_automation_checked(tx, a.id, current))?;
                    continue;
                }
                let context = format!(
                    "new matching messages (ids): {}",
                    matches
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                let outcome = self.run(&a, &trigger.describe(), &context).await?;
                if matches!(outcome, AutomationOutcome::Ran { .. }) {
                    self.db()
                        .write(|tx| repos::omni::set_automation_checked(tx, a.id, current))?;
                }
                out.push(outcome);
            } else if schedule_due(&trigger, a.last_run_at, now) {
                out.push(self.run(&a, &trigger.describe(), "").await?);
            }
        }
        Ok(out)
    }

    /// New messages since the automation's cursor that match an event
    /// trigger: never your own, never from conversations hidden from agents.
    fn event_matches(
        &self,
        a: &AutomationRecord,
        trigger: &AutomationTrigger,
    ) -> Result<(u64, Vec<litecord_types::MessageId>)> {
        let default_visibility = self.default_visibility();
        self.db().read(|r| -> Result<_> {
            let current = r.revision().get();
            let me =
                repos::accounts::current(r, DiscordIdentity::UserSocialSdk)?.map(|a| a.user_id);
            let mut found = Vec::new();
            for e in repos::events::since(r, Revision(a.last_checked_revision), 500)? {
                let litecord_core::events::UnifiedEvent::MessageCreated {
                    message_id,
                    conversation_id,
                } = e.event
                else {
                    continue;
                };
                let Some(m) = repos::messages::get(r, message_id)? else {
                    continue;
                };
                if m.deleted || Some(m.message.author_id) == me {
                    continue;
                }
                if repos::conversations::effective_visibility(
                    r,
                    conversation_id,
                    default_visibility,
                )? != AgentVisibility::Allowed
                {
                    continue;
                }
                let hit = match trigger {
                    AutomationTrigger::DirectMessageFrom { user_id } => {
                        m.message.author_id == *user_id
                            && repos::conversations::get(r, conversation_id)?.is_some_and(|c| {
                                c.conversation.kind == ConversationKind::DirectMessage
                            })
                    }
                    AutomationTrigger::Keyword { keyword } => m
                        .message
                        .content
                        .to_lowercase()
                        .contains(&keyword.trim().to_lowercase()),
                    _ => false,
                };
                if hit && found.len() < 10 {
                    found.push(message_id);
                }
            }
            Ok((current, found))
        })
    }

    async fn run(
        &self,
        a: &AutomationRecord,
        trigger: &str,
        context: &str,
    ) -> Result<AutomationOutcome> {
        use AutomationOutcome::Skipped;
        if !self.status().login.is_ready() {
            // The sidecar may simply be stopped: ask it.
            if !matches!(self.refresh_login().await, Ok(LoginState::Ready { .. })) {
                return Ok(Skipped("signed out"));
            }
        }
        if !self.within_automation_budget()? {
            return Ok(Skipped("hourly limit"));
        }
        let session_id = self.automation_session(a)?;
        if self.is_running(session_id) {
            return Ok(Skipped("busy"));
        }
        let prompt = AutomationPrompt {
            name: &a.name,
            trigger,
            instructions: &a.prompt,
            context,
            output: AutomationOutputKind::parse(&a.output).prompt_output(),
        }
        .render();
        let result = self.send_system(session_id, &prompt).await;
        let error = result.as_ref().err().map(ToString::to_string);
        self.db()
            .write(|tx| repos::omni::record_automation_run(tx, a.id, error.as_deref()))?;
        self.note_automation_run()?;
        self.notify_changed();
        result?;
        Ok(AutomationOutcome::Ran {
            automation_id: a.id,
            session_id,
        })
    }

    /// The automation's own rolling Assistant session (rotated every 20 runs).
    fn automation_session(&self, a: &AutomationRecord) -> Result<i64> {
        const ROTATE_AFTER_TURNS: u32 = 20;
        if let Some(id) = a.session_id {
            if let Some(s) = self.db().read(|r| repos::omni::get(r, id))? {
                if s.status == "active" && s.turns < ROTATE_AFTER_TURNS {
                    return Ok(id);
                }
                self.archive(id)?;
            }
        }
        let harness = self.selected().map_or("none", |k| k.as_str());
        let aid = a.id;
        let name = a.name.clone();
        let id = self
            .db()
            .write(|tx| -> litecord_store::StoreResult<i64> {
                let id = repos::omni::create_session(
                    tx,
                    &NewSession {
                        harness,
                        kind: "automation",
                        mode: OmniMode::Assistant.as_str(),
                        profile: None,
                        title: &name,
                        parent_id: None,
                    },
                )?;
                repos::omni::set_automation_session(tx, aid, id)?;
                Ok(id)
            })?
            .value;
        Ok(id)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;
    /// Thursday 2026-09-24 00:00 UTC.
    const THU: i64 = 1_790_208_000_000;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_millis(ms)
    }

    #[test]
    fn daily_fires_once_per_slot_within_the_window() {
        let t = AutomationTrigger::Daily {
            minute_of_day: 8 * 60,
            weekdays: 0,
        };
        assert!(
            !schedule_due(&t, None, at(THU + 7 * HOUR)),
            "before the slot"
        );
        assert!(schedule_due(&t, None, at(THU + 8 * HOUR)));
        assert!(
            !schedule_due(&t, Some(at(THU + 8 * HOUR)), at(THU + 9 * HOUR)),
            "already ran"
        );
        assert!(
            !schedule_due(&t, None, at(THU + 15 * HOUR)),
            "missed by > 6 h"
        );
        assert!(
            schedule_due(&t, Some(at(THU + 8 * HOUR)), at(THU + 32 * HOUR)),
            "next day"
        );
    }

    #[test]
    fn daily_respects_weekdays() {
        // Weekdays only; Saturday 2026-09-26 is THU + 2 days.
        let t = AutomationTrigger::Daily {
            minute_of_day: 9 * 60,
            weekdays: 0x1f,
        };
        assert!(schedule_due(&t, None, at(THU + 9 * HOUR)), "Thursday");
        assert!(
            !schedule_due(&t, None, at(THU + 2 * 24 * HOUR + 9 * HOUR)),
            "Saturday"
        );
        assert_eq!(t.describe(), "daily at 09:00 UTC on weekdays");
    }

    #[test]
    fn every_n_hours() {
        let t = AutomationTrigger::Every { hours: 4 };
        assert!(schedule_due(&t, None, at(THU)));
        assert!(!schedule_due(&t, Some(at(THU)), at(THU + 3 * HOUR)));
        assert!(schedule_due(&t, Some(at(THU)), at(THU + 4 * HOUR)));
    }

    #[test]
    fn triggers_validate_and_roundtrip() {
        assert!(AutomationTrigger::Every { hours: 0 }.validate().is_err());
        assert!(AutomationTrigger::Keyword {
            keyword: "x".into()
        }
        .validate()
        .is_err());
        for p in presets() {
            p.trigger.validate().unwrap();
            let json = serde_json::to_string(&p.trigger).unwrap();
            assert_eq!(
                serde_json::from_str::<AutomationTrigger>(&json).unwrap(),
                p.trigger
            );
        }
    }
}
