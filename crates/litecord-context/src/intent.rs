//! Deterministic request analysis (no LLM).

use litecord_retrieval::temporal;
use litecord_store::repos::fts;
use litecord_types::Timestamp;

use crate::pack::IntentSummary;

const STOPWORDS: &[&str] = &[
    "what",
    "did",
    "does",
    "do",
    "anyone",
    "someone",
    "anybody",
    "me",
    "my",
    "mine",
    "you",
    "your",
    "the",
    "an",
    "about",
    "any",
    "anything",
    "is",
    "are",
    "was",
    "were",
    "be",
    "to",
    "of",
    "for",
    "in",
    "on",
    "at",
    "with",
    "who",
    "whom",
    "should",
    "have",
    "has",
    "had",
    "message",
    "messages",
    "tell",
    "show",
    "say",
    "said",
    "today",
    "yesterday",
    "week",
    "weeks",
    "last",
    "past",
    "this",
    "days",
    "day",
    "hours",
    "hour",
    "recently",
    "lately",
    "month",
    "and",
    "or",
    "it",
    "that",
    "there",
    "from",
    "can",
    "could",
    "would",
    "please",
    "help",
    "get",
    "got",
    "which",
    "how",
    "when",
    "where",
    "why",
    "up",
    "all",
    "some",
    "new",
    "since",
    "by",
    "so",
    "we",
    "us",
    "our",
    "they",
    "them",
    "their",
    "he",
    "she",
    "him",
    "her",
    "his",
    "hers",
    "not",
    "no",
    "yes",
];

const REPLY_WORDS: &[&str] = &[
    "reply",
    "respond",
    "answer",
    "forgot",
    "forgotten",
    "owe",
    "unanswered",
    "replied",
    "responded",
];
const TASK_WORDS: &[&str] = &[
    "task",
    "tasks",
    "todo",
    "todos",
    "remind",
    "reminder",
    "reminders",
    "commit",
    "commitment",
    "commitments",
    "deadline",
    "deadlines",
    "due",
];
const CATCH_UP_WORDS: &[&str] = &[
    "miss", "missed", "happened", "recap", "catch", "updates", "update",
];

/// Analyze an instruction: keywords (stopwords and trigger words removed),
/// time range and coarse wants. `resolved_entities` is filled by the compiler.
pub fn analyze(instruction: &str, now: Timestamp, utc_offset_minutes: i32) -> IntentSummary {
    let terms = fts::terms(instruction);
    let has = |set: &[&str]| terms.iter().any(|t| set.contains(&t.as_str()));
    let lower = instruction.to_lowercase();
    let range = temporal::parse(instruction, now, utc_offset_minutes);
    let keywords = terms
        .iter()
        .filter(|t| {
            !STOPWORDS.contains(&t.as_str())
                && !REPLY_WORDS.contains(&t.as_str())
                && !TASK_WORDS.contains(&t.as_str())
                && !CATCH_UP_WORDS.contains(&t.as_str())
                && !t.chars().all(|c| c.is_ascii_digit())
        })
        .cloned()
        .collect();
    IntentSummary {
        keywords,
        since: range.map(|r| r.since),
        until: range.and_then(|r| r.until),
        resolved_entities: Vec::new(),
        wants_pending_replies: has(REPLY_WORDS),
        wants_tasks: has(TASK_WORDS) || lower.contains("to-do"),
        wants_catch_up: has(CATCH_UP_WORDS) || lower.contains("catch up"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Timestamp = Timestamp(1_790_262_000_000);

    #[test]
    fn extracts_keywords_time_and_wants() {
        let i = analyze(
            "Did anyone message me about the prototype this week?",
            NOW,
            0,
        );
        assert_eq!(i.keywords, vec!["prototype".to_string()]);
        assert!(i.since.is_some());
        assert!(!i.wants_pending_replies);
        let r = analyze("Who should I reply to?", NOW, 0);
        assert!(r.wants_pending_replies);
        assert!(r.keywords.is_empty());
        let c = analyze("what did I miss today", NOW, 0);
        assert!(c.wants_catch_up && c.since.is_some());
        let t = analyze("any deadlines or tasks for grace?", NOW, 0);
        assert!(t.wants_tasks);
        assert_eq!(t.keywords, vec!["grace".to_string()]);
    }
}
