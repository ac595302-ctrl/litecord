//! Deterministic temporal phrase parsing ("today", "last week", "past 3
//! days"). No LLM, no locale magic: a small, documented grammar evaluated
//! against an explicit `now` and UTC offset.

use serde::Serialize;

use litecord_types::{DurationMs, Timestamp};

const DAY_MS: i64 = 86_400_000;

/// A half-open time range `[since, until)`. `until = None` means "now".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TimeRange {
    pub since: Timestamp,
    pub until: Option<Timestamp>,
}

impl TimeRange {
    pub fn contains(&self, t: Timestamp) -> bool {
        t >= self.since && self.until.is_none_or(|u| t < u)
    }
}

/// Start of the local day containing `now` (offset in minutes east of UTC).
fn day_start(now: Timestamp, utc_offset_minutes: i32) -> i64 {
    let off = i64::from(utc_offset_minutes) * 60_000;
    let local = now.as_millis() + off;
    local.div_euclid(DAY_MS) * DAY_MS - off
}

/// Find the first recognized temporal phrase in `text`.
///
/// Grammar (case-insensitive): `today`, `yesterday`, `this week`, `last week`,
/// `this month`, `recently`, `past|last N (hours|days|weeks)`. Weeks start on
/// Monday. Returns `None` when nothing matches.
pub fn parse(text: &str, now: Timestamp, utc_offset_minutes: i32) -> Option<TimeRange> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let today = day_start(now, utc_offset_minutes);
    // 1970-01-01 was a Thursday; Monday-based weekday index of `today`.
    let weekday =
        ((today + i64::from(utc_offset_minutes) * 60_000).div_euclid(DAY_MS) + 3).rem_euclid(7);
    let week_start = today - weekday * DAY_MS;

    for (i, w) in words.iter().enumerate() {
        let next = words.get(i + 1).copied();
        let range = match (*w, next) {
            ("today", _) => Some((today, None)),
            ("yesterday", _) => Some((today - DAY_MS, Some(today))),
            ("this", Some("week")) => Some((week_start, None)),
            ("last", Some("week")) => Some((week_start - 7 * DAY_MS, Some(week_start))),
            ("this", Some("month")) => Some((today - 30 * DAY_MS, None)),
            ("recently", _) | ("lately", _) => Some((now.as_millis() - 3 * DAY_MS, None)),
            ("past" | "last", Some(n)) => {
                let (amount, unit) = match n.parse::<i64>() {
                    Ok(v) => (v, words.get(i + 2).copied()),
                    Err(_) => (1, Some(n)),
                };
                let unit_ms = match unit {
                    Some("hour" | "hours") => Some(3_600_000),
                    Some("day" | "days") => Some(DAY_MS),
                    Some("week" | "weeks") if amount != 1 || n != "week" => Some(7 * DAY_MS),
                    _ => None,
                };
                unit_ms.map(|u| (now.as_millis() - amount.clamp(0, 3650) * u, None))
            }
            _ => None,
        };
        if let Some((since, until)) = range {
            return Some(TimeRange {
                since: Timestamp::from_millis(since),
                until: until.map(Timestamp::from_millis),
            });
        }
    }
    None
}

/// Convenience: a range covering the last `d`.
pub fn last(d: DurationMs, now: Timestamp) -> TimeRange {
    TimeRange {
        since: now.saturating_sub(d),
        until: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-09-24T15:00:00Z, a Thursday.
    const NOW: Timestamp = Timestamp(1_790_262_000_000);

    #[test]
    fn today_and_yesterday() {
        let t = parse("what did I miss today?", NOW, 0).unwrap();
        assert_eq!(t.since, Timestamp(1_790_208_000_000));
        assert_eq!(t.until, None);
        let y = parse("Yesterday's messages", NOW, 0).unwrap();
        assert_eq!(y.until, Some(Timestamp(1_790_208_000_000)));
        assert_eq!(y.since, Timestamp(1_790_208_000_000 - DAY_MS));
    }

    #[test]
    fn weeks_start_on_monday() {
        let w = parse("anything this week", NOW, 0).unwrap();
        // Monday 2026-09-21T00:00Z
        assert_eq!(w.since, Timestamp(1_790_208_000_000 - 3 * DAY_MS));
        let lw = parse("last week", NOW, 0).unwrap();
        assert_eq!(lw.until, Some(w.since));
    }

    #[test]
    fn past_n_units_and_offsets() {
        let r = parse("messages from the past 3 days", NOW, 0).unwrap();
        assert_eq!(r.since, Timestamp(NOW.0 - 3 * DAY_MS));
        let h = parse("last 2 hours", NOW, 0).unwrap();
        assert_eq!(h.since, Timestamp(NOW.0 - 2 * 3_600_000));
        // UTC-8: local day starts at 08:00Z.
        let t = parse("today", NOW, -480).unwrap();
        assert_eq!(t.since, Timestamp(1_790_208_000_000 + 8 * 3_600_000));
        assert!(parse("tell me about the project", NOW, 0).is_none());
    }
}
