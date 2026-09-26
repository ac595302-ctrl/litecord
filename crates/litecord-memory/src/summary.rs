//! Deterministic, non-LLM conversation summaries (V2 §24 "segment/recent/
//! weekly/long_term" summaries, heuristic tier).
//!
//! [`Summarizer`] is the pluggable seam: [`HeuristicSummarizer`] is a
//! content-light digest built only from counts, per-author activity, coarse
//! lexical frequency and a couple of surface markers (`?`, "I'll ..."). It
//! never attempts to paraphrase or interpret message content. A future
//! LLM-backed summarizer implements the same trait; per the provenance rule
//! that governs every derived item in this workspace, it must still be
//! wired up (by whoever constructs it) to store its output with
//! `Origin::AgentDerived`, not `Origin::LocalApplication` — see
//! [`crate::service::MemoryService::summarize_conversation`], which is what
//! actually persists a summarizer's output and chooses the origin for it.

use std::collections::HashMap;

use litecord_store::repos::fts;
use litecord_types::social::Message;
use litecord_types::Timestamp;

/// One message paired with its author's display name, as read by the caller
/// (typically `MemoryService::summarize_conversation`) so summarizers never
/// need to look up users themselves.
pub type NamedMessage<'a> = (String, &'a Message);

/// Everything a [`Summarizer`] needs to produce a digest for one period of
/// one conversation.
///
/// Message content is necessarily visible here (a summarizer reads it to
/// summarize it), but per the workspace rule, nothing downstream may *log*
/// it — only the derived `content` string a summarizer returns is ever
/// persisted or traced.
#[derive(Debug, Clone, Copy)]
pub struct SummaryInput<'a> {
    pub conversation_title: &'a str,
    pub messages: &'a [NamedMessage<'a>],
    pub period_start: Timestamp,
    pub period_end: Timestamp,
}

/// Turns a window of messages into a short summary string, or `None` if
/// there isn't enough material to be worth storing.
pub trait Summarizer: Send + Sync + std::fmt::Debug {
    fn summarize(&self, input: &SummaryInput<'_>) -> Option<String>;
}

/// Below this many messages, a summary would just restate the conversation.
const MIN_MESSAGES: usize = 3;

/// Terms shorter than this are treated as noise, not topics.
const MIN_TOPIC_LEN: usize = 4;

/// How many top terms are reported as "frequent topics".
const TOPIC_COUNT: usize = 3;

/// Small, deliberately short stopword list: common English function/filler
/// words that would otherwise dominate frequency counts without carrying
/// topical meaning. Not exhaustive — this is a heuristic, not NLP.
const STOPWORDS: &[&str] = &[
    "that", "this", "with", "from", "have", "your", "about", "just", "what", "when", "were",
    "been", "they", "them", "their", "there", "here", "would", "could", "should", "will", "also",
    "some", "more", "than", "then", "into", "over", "only", "very", "like", "said", "says",
    "going", "dont", "cant", "yeah", "okay", "sure", "thanks", "hello", "there's", "we're",
    "that's", "it's", "i'm", "youre", "gonna", "wanna", "does", "doing", "done", "make", "made",
];

const MONTH_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MS_PER_DAY: i64 = 86_400_000;

/// Days-since-epoch (`1970-01-01`) to a proleptic Gregorian `(year, month,
/// day)`. Howard Hinnant's `civil_from_days` algorithm — exact for the whole
/// `i64` range, no external date/time crate needed for this one heuristic.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn ymd(at: Timestamp) -> (i64, u32, u32) {
    civil_from_days(at.as_millis().div_euclid(MS_PER_DAY))
}

/// Render `[start, end]` as e.g. `"Sep 20–26"`, `"Sep 28–Oct 2"`, or
/// `"Dec 30 2026–Jan 2 2027"` when the range crosses a year boundary.
fn format_range(start: Timestamp, end: Timestamp) -> String {
    let (ys, ms, ds) = ymd(start);
    let (ye, me, de) = ymd(end);
    let mon = |m: u32| MONTH_ABBR[(m - 1) as usize];
    if ys == ye && ms == me {
        format!("{} {ds}\u{2013}{de}", mon(ms))
    } else if ys == ye {
        format!("{} {ds}\u{2013}{} {de}", mon(ms), mon(me))
    } else {
        format!("{} {ds} {ys}\u{2013}{} {de} {ye}", mon(ms), mon(me))
    }
}

/// Join names as `"a"`, `"a and b"` or `"a, b and c"`.
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        _ => match names.split_last() {
            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
            None => String::new(),
        },
    }
}

/// Deterministic, content-light digest. See the module docs for what it
/// intentionally does *not* do (paraphrase, interpret, or use an LLM).
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicSummarizer;

impl Summarizer for HeuristicSummarizer {
    fn summarize(&self, input: &SummaryInput<'_>) -> Option<String> {
        let total = input.messages.len();
        if total < MIN_MESSAGES {
            return None;
        }

        // Per-author counts, in order of first appearance (for determinism
        // and a stable "most active" tie-break).
        let mut order: Vec<String> = Vec::new();
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for (name, _) in input.messages {
            if !counts.contains_key(name.as_str()) {
                order.push(name.clone());
            }
            *counts.entry(name.as_str()).or_insert(0) += 1;
        }
        let most_active = order
            .iter()
            .max_by_key(|n| (counts[n.as_str()], std::cmp::Reverse(n.as_str())))
            .cloned()
            .unwrap_or_default();

        // Term document-frequency: how many distinct messages mention a
        // term, using the same tokenizer FTS queries use so "frequent
        // topics" line up with what search would find.
        let mut freq: HashMap<String, usize> = HashMap::new();
        let mut first_seen: HashMap<String, usize> = HashMap::new();
        for (i, (_, m)) in input.messages.iter().enumerate() {
            for term in fts::terms(&m.content) {
                if term.chars().count() < MIN_TOPIC_LEN || STOPWORDS.contains(&term.as_str()) {
                    continue;
                }
                *freq.entry(term.clone()).or_insert(0) += 1;
                first_seen.entry(term).or_insert(i);
            }
        }
        let mut ranked: Vec<&String> = freq.keys().collect();
        ranked.sort_by(|a, b| {
            freq[*b]
                .cmp(&freq[*a])
                .then_with(|| first_seen[*a].cmp(&first_seen[*b]))
                .then_with(|| a.cmp(b))
        });
        let topics: Vec<String> = ranked.into_iter().take(TOPIC_COUNT).cloned().collect();

        let questions = input
            .messages
            .iter()
            .filter(|(_, m)| m.content.trim_end().ends_with('?'))
            .count();
        let commitments = input
            .messages
            .iter()
            .filter(|(_, m)| {
                let lower = m.content.to_lowercase();
                lower.contains("i'll ") || lower.contains("i will ")
            })
            .count();

        let mut out = format!(
            "{total} messages between {}, {}. Most active: {most_active} ({}).",
            join_names(&order),
            format_range(input.period_start, input.period_end),
            counts[most_active.as_str()],
        );
        if !topics.is_empty() {
            out.push_str(&format!(" Frequent topics: {}.", topics.join(", ")));
        }
        out.push_str(&format!(
            " {questions} questions, {commitments} commitments."
        ));
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litecord_types::ids::{ConversationId, MessageId, UserId};

    fn msg(id: u64, text: &str, sent_at: i64) -> Message {
        Message {
            id: MessageId(id),
            conversation_id: ConversationId(1),
            author_id: UserId(1),
            content: text.into(),
            sent_at: Timestamp::from_millis(sent_at),
            edited_at: None,
            reply_to: None,
            extras: vec![],
        }
    }

    #[test]
    fn too_few_messages_yields_none() {
        let a = msg(1, "hi", 0);
        let b = msg(2, "hello", 1);
        let owned = [("ada".to_string(), &a), ("you".to_string(), &b)];
        let input = SummaryInput {
            conversation_title: "ada",
            messages: &owned,
            period_start: Timestamp::from_millis(0),
            period_end: Timestamp::from_millis(1),
        };
        assert!(HeuristicSummarizer.summarize(&input).is_none());
    }

    #[test]
    fn digest_is_deterministic_and_reports_expected_content() {
        // Sun 2026-09-20T00:00:00Z .. Sat 2026-09-26T00:00:00Z
        let start = Timestamp(1_789_862_400_000);
        let end = Timestamp(1_790_380_800_000);
        let m1 = msg(1, "The prototype review went well", start.as_millis());
        let m2 = msg(
            2,
            "Did you finish the prototype review?",
            start.as_millis() + 1,
        );
        let m3 = msg(
            3,
            "I'll send the prototype review notes tomorrow",
            start.as_millis() + 2,
        );
        let m4 = msg(4, "Sounds good, thanks", start.as_millis() + 3);
        let owned = [
            ("ada".to_string(), &m1),
            ("you".to_string(), &m2),
            ("ada".to_string(), &m3),
            ("ada".to_string(), &m4),
        ];
        let input = SummaryInput {
            conversation_title: "ada",
            messages: &owned,
            period_start: start,
            period_end: end,
        };
        let out1 = HeuristicSummarizer.summarize(&input).unwrap();
        let out2 = HeuristicSummarizer.summarize(&input).unwrap();
        assert_eq!(out1, out2, "must be deterministic");
        assert!(out1.starts_with("4 messages between ada and you, Sep 20\u{2013}26."));
        assert!(out1.contains("Most active: ada (3)"));
        assert!(out1.contains("prototype"));
        assert!(out1.contains("review"));
        assert!(out1.contains("1 questions, 1 commitments."));
    }

    #[test]
    fn short_and_stopword_terms_are_excluded_from_topics() {
        let m1 = msg(1, "the cat sat on the mat over there", 0);
        let m2 = msg(2, "elephants elephants elephants are big", 1);
        let m3 = msg(3, "elephants are also gray", 2);
        let owned = [
            ("ada".to_string(), &m1),
            ("ada".to_string(), &m2),
            ("ada".to_string(), &m3),
        ];
        let input = SummaryInput {
            conversation_title: "ada",
            messages: &owned,
            period_start: Timestamp::from_millis(0),
            period_end: Timestamp::from_millis(2),
        };
        let out = HeuristicSummarizer.summarize(&input).unwrap();
        assert!(out.contains("elephants"));
        assert!(!out.contains(" cat "));
        assert!(!out.contains("also"));
    }
}
