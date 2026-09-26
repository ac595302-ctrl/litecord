//! Token budgeting.
//!
//! Token counts are estimates (≈4 characters per token plus fixed per-item
//! overhead for ids/metadata). The estimate is deliberately conservative and
//! vendor-neutral; a harness with a real tokenizer can re-check.

use serde::Serialize;

/// Per-item structural overhead (ids, timestamps, labels).
pub const ITEM_OVERHEAD_TOKENS: u32 = 16;

pub fn estimate_tokens(text: &str) -> u32 {
    let chars = text.chars().count() as u32;
    chars.div_ceil(4) + ITEM_OVERHEAD_TOKENS
}

/// Tracks remaining budget while sections are filled greedily.
#[derive(Debug, Clone, Serialize)]
pub struct BudgetTracker {
    max: u32,
    used: u32,
    dropped: u32,
}

impl BudgetTracker {
    pub fn new(max: u32) -> Self {
        Self {
            max,
            used: 0,
            dropped: 0,
        }
    }

    /// Reserve `tokens`; returns `false` (and counts a drop) if it does not fit.
    pub fn try_take(&mut self, tokens: u32) -> bool {
        if self.used.saturating_add(tokens) <= self.max {
            self.used += tokens;
            true
        } else {
            self.dropped += 1;
            false
        }
    }

    pub fn used(&self) -> u32 {
        self.used
    }
    pub fn remaining(&self) -> u32 {
        self.max - self.used
    }
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_and_budget() {
        assert_eq!(estimate_tokens(""), ITEM_OVERHEAD_TOKENS);
        assert_eq!(estimate_tokens("abcd"), 1 + ITEM_OVERHEAD_TOKENS);
        let mut b = BudgetTracker::new(40);
        assert!(b.try_take(30));
        assert!(!b.try_take(20));
        assert!(b.try_take(10));
        assert_eq!((b.used(), b.remaining(), b.dropped()), (40, 0, 1));
    }
}
