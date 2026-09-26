//! Cheap, deterministic candidate extraction (V2 §21) — **no LLM**.
//!
//! ```text
//! message ─► heuristic classifier ─┬─ ordinary conversation (nothing)
//!                                  ├─ commitment        ─► Commitment candidate
//!                                  ├─ question to me    ─► PendingReply candidate
//!                                  └─ date + event word ─► ImportantDate candidate
//! ```
//!
//! Output is always `MemoryStatus::Candidate` with `Origin::LocalApplication`
//! and moderate confidence: a candidate is **not** a confirmed fact. The
//! message text itself stays external content; memory content is a short
//! derived description that quotes it.
//!
//! Fingerprints are chosen so that the same claim deduplicates and a changed
//! claim (e.g. "meeting Friday" → "meeting moved to Saturday") collides on
//! the same fingerprint with a *different payload*, which the memory service
//! treats as a contradiction and resolves by supersession.

use litecord_types::entity::EntityId;
use litecord_types::ids::UserId;
use litecord_types::memory::{MemoryFingerprint, MemoryKind, MemoryPayload, NewMemory};
use litecord_types::provenance::{Confidence, Origin, SourceRef};
use litecord_types::social::Message;
use litecord_types::{DurationMs, Timestamp};

const DAY_MS: i64 = 86_400_000;

/// Context the extractor needs beyond the message itself.
#[derive(Debug, Clone, Copy)]
pub struct ExtractionInput<'a> {
    pub message: &'a Message,
    /// The local user (to detect questions addressed to them).
    pub me: Option<UserId>,
    /// Display name of the author, used in derived descriptions.
    pub author_name: &'a str,
}

/// Anything that turns a message into memory candidates. A future
/// model-backed extractor implements the same trait and must still emit
/// candidates with `Origin::AgentDerived`.
pub trait CandidateExtractor: Send + Sync + std::fmt::Debug {
    fn extract(&self, input: &ExtractionInput<'_>) -> Vec<NewMemory>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicExtractor;

const COMMITMENT_MARKERS: &[&str] = &[
    "i'll ",
    "i will ",
    "i'm going to ",
    "im going to ",
    "i am going to ",
    "will send",
    "promise to",
];
const REQUEST_MARKERS: &[&str] = &[
    "can you",
    "could you",
    "would you",
    "will you",
    "are you",
    "do you",
    "did you",
    "have you",
    "please",
];
const EVENT_WORDS: &[&str] = &[
    "meeting", "call", "deadline", "due", "demo", "review", "standup", "launch", "sync",
];
const WEEKDAYS: &[&str] = &[
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

fn first_sentence_with<'a>(text: &'a str, lower: &str, marker: &str) -> Option<&'a str> {
    let pos = lower.find(marker)?;
    // Lowercasing can change byte lengths for some non-ASCII characters; only
    // reuse offsets on the original text when they are provably aligned.
    if lower.len() != text.len() || !text.is_char_boundary(pos) {
        return Some(text.trim());
    }
    let start = text[..pos]
        .rfind(['.', '!', '?', '\n'])
        .map(|i| i + 1)
        .unwrap_or(0);
    let end = text[pos..]
        .find(['.', '!', '?', '\n'])
        .map(|i| pos + i + 1)
        .unwrap_or(text.len());
    Some(text[start..end].trim())
}

/// Resolve "today/tomorrow/tonight/<weekday>" relative to `at` (UTC days).
fn resolve_day(lower: &str, at: Timestamp) -> Option<(Timestamp, &'static str)> {
    let day_start = at.as_millis().div_euclid(DAY_MS) * DAY_MS;
    let end_of = |d: i64| Timestamp::from_millis(day_start + d * DAY_MS + DAY_MS - 1);
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    for w in &words {
        match *w {
            "today" | "tonight" => return Some((end_of(0), "today")),
            "tomorrow" => return Some((end_of(1), "tomorrow")),
            _ => {}
        }
    }
    let weekday_now = ((day_start.div_euclid(DAY_MS) + 3).rem_euclid(7)) as usize; // Mon=0
    for w in &words {
        if let Some(idx) = WEEKDAYS.iter().position(|d| d == w) {
            let ahead = (idx + 7 - weekday_now) % 7;
            let ahead = if ahead == 0 { 7 } else { ahead } as i64;
            return Some((end_of(ahead), WEEKDAYS[idx]));
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

impl CandidateExtractor for HeuristicExtractor {
    fn extract(&self, input: &ExtractionInput<'_>) -> Vec<NewMemory> {
        let msg = input.message;
        let text = msg.content.as_ref();
        if text.trim().is_empty() {
            return Vec::new();
        }
        let lower = text.to_lowercase();
        let conv = EntityId::Conversation(msg.conversation_id);
        let author = EntityId::User(msg.author_id);
        let source = vec![SourceRef::new(EntityId::Message(msg.id))];
        let from_me = input.me == Some(msg.author_id);
        let mut out = Vec::new();

        // Commitments: "I'll send the design doc tomorrow".
        if let Some(sentence) = COMMITMENT_MARKERS
            .iter()
            .find_map(|m| first_sentence_with(text, &lower, m))
        {
            let due = resolve_day(&sentence.to_lowercase(), msg.sent_at);
            let what = truncate(sentence, 200);
            let mut m = NewMemory::new(
                MemoryKind::Commitment,
                format!("{} committed: \"{}\"", input.author_name, what),
                Origin::LocalApplication,
            );
            m.confidence = Confidence::clamped(0.55);
            m.payload = Some(MemoryPayload::Commitment {
                by: Some(msg.author_id),
                what,
                due_at: due.map(|d| d.0),
            });
            m.entities = vec![author, conv];
            m.source_refs = source.clone();
            m.observed_at = Some(msg.sent_at);
            // A commitment is stale a week after its due date (or sending).
            m.expires_at = Some(
                due.map(|d| d.0)
                    .unwrap_or(msg.sent_at)
                    .saturating_add(DurationMs::from_days(7)),
            );
            m.fingerprint = Some(MemoryFingerprint::compute(
                MemoryKind::Commitment,
                &m.entities,
                &format!("msg:{}", msg.id),
            ));
            out.push(m);
        }

        // Questions/requests addressed to me: likely need a reply.
        if !from_me
            && input.me.is_some()
            && (text.trim_end().ends_with('?') || REQUEST_MARKERS.iter().any(|m| lower.contains(m)))
        {
            let mut m = NewMemory::new(
                MemoryKind::PendingReply,
                format!(
                    "{} asked: \"{}\"",
                    input.author_name,
                    truncate(text.trim(), 160)
                ),
                Origin::LocalApplication,
            );
            m.confidence = Confidence::clamped(0.6);
            m.payload = Some(MemoryPayload::PendingReply {
                from: msg.author_id,
                conversation_id: msg.conversation_id,
                message_id: msg.id,
            });
            m.entities = vec![author, conv];
            m.source_refs = source.clone();
            m.observed_at = Some(msg.sent_at);
            m.importance = 0.7;
            // One open "needs reply" per conversation: newer questions
            // supersede older ones via the shared fingerprint.
            m.fingerprint = Some(MemoryFingerprint::compute(
                MemoryKind::PendingReply,
                &[conv],
                "open",
            ));
            out.push(m);
        }

        // Important dates: "meeting Friday", "meeting moved to Saturday".
        if let Some(event) = EVENT_WORDS.iter().find(|w| {
            lower
                .split(|c: char| !c.is_alphanumeric())
                .any(|t| t == **w)
        }) {
            if let Some((at, label)) = resolve_day(&lower, msg.sent_at) {
                let mut m = NewMemory::new(
                    MemoryKind::ImportantDate,
                    format!("{event} {label} (from {})", input.author_name),
                    Origin::LocalApplication,
                );
                m.confidence = Confidence::clamped(0.5);
                m.payload = Some(MemoryPayload::Date {
                    at,
                    label: format!("{event} {label}"),
                });
                m.entities = vec![conv];
                m.source_refs = source;
                m.observed_at = Some(msg.sent_at);
                m.expires_at = Some(at.saturating_add(DurationMs::from_days(1)));
                m.fingerprint = Some(MemoryFingerprint::compute(
                    MemoryKind::ImportantDate,
                    &[conv],
                    event,
                ));
                out.push(m);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litecord_types::ids::{ConversationId, MessageId};
    use litecord_types::memory::MemoryStatus;

    // Thursday 2026-09-24T15:00:00Z
    const NOW: Timestamp = Timestamp(1_790_262_000_000);

    fn msg(author: u64, text: &str) -> Message {
        Message {
            id: MessageId(1),
            conversation_id: ConversationId(9),
            author_id: UserId(author),
            content: text.into(),
            sent_at: NOW,
            edited_at: None,
            reply_to: None,
            extras: vec![],
        }
    }

    fn run(author: u64, text: &str) -> Vec<NewMemory> {
        let m = msg(author, text);
        HeuristicExtractor.extract(&ExtractionInput {
            message: &m,
            me: Some(UserId(1000)),
            author_name: "ada",
        })
    }

    #[test]
    fn commitments_are_candidates_with_due_dates() {
        let out = run(7, "Thanks! I'll send the design doc tomorrow.");
        let c = out
            .iter()
            .find(|m| m.kind == MemoryKind::Commitment)
            .unwrap();
        assert_eq!(c.status, MemoryStatus::Candidate);
        assert_eq!(c.origin, Origin::LocalApplication);
        match &c.payload {
            Some(MemoryPayload::Commitment {
                due_at: Some(d), ..
            }) => {
                assert_eq!(d.as_millis(), 1_790_208_000_000 + 2 * DAY_MS - 1)
            }
            other => panic!("unexpected payload {other:?}"),
        }
    }

    #[test]
    fn questions_to_me_become_pending_replies_but_mine_do_not() {
        assert!(run(7, "Can you review the prototype tonight?")
            .iter()
            .any(|m| m.kind == MemoryKind::PendingReply));
        assert!(!run(1000, "can you review it?")
            .iter()
            .any(|m| m.kind == MemoryKind::PendingReply));
    }

    #[test]
    fn rescheduled_meeting_shares_fingerprint_with_different_payload() {
        let a = run(7, "Meeting Friday to go over the project");
        let b = run(7, "Update: meeting moved to Saturday");
        let da = a
            .iter()
            .find(|m| m.kind == MemoryKind::ImportantDate)
            .unwrap();
        let db = b
            .iter()
            .find(|m| m.kind == MemoryKind::ImportantDate)
            .unwrap();
        assert_eq!(da.fingerprint, db.fingerprint);
        assert_ne!(da.payload, db.payload);
    }

    #[test]
    fn non_ascii_text_never_panics() {
        let out = run(
            7,
            "İstanbul trip: I'll send the İtinerary tomorrow. Straße?",
        );
        assert!(out.iter().any(|m| m.kind == MemoryKind::Commitment));
    }

    #[test]
    fn ordinary_chatter_yields_nothing() {
        assert!(run(7, "haha nice").is_empty());
        assert!(run(7, "   ").is_empty());
    }
}
