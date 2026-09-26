//! Explainable retrieval scoring (V2 §19).
//!
//! `total = Σ weightᵢ · componentᵢ`, every component in `[0, 1]`. Components
//! are kept separately so the Model Context Debugger can show *why* an item
//! was selected. Semantic relevance is optional and zero when no embedding
//! provider is configured — the system stays useful without AI.

use serde::Serialize;

use litecord_types::{DurationMs, Timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct RetrievalScore {
    pub lexical: f32,
    pub semantic: f32,
    pub recency: f32,
    pub entity: f32,
    pub conversation: f32,
    pub pinned: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ScoringWeights {
    pub lexical: f32,
    pub semantic: f32,
    pub recency: f32,
    pub entity: f32,
    pub conversation: f32,
    pub pinned: f32,
    /// Recency half-life.
    pub recency_half_life: DurationMs,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            lexical: 1.0,
            semantic: 0.8,
            recency: 0.6,
            entity: 0.8,
            conversation: 0.5,
            pinned: 0.5,
            recency_half_life: DurationMs::from_days(3),
        }
    }
}

impl RetrievalScore {
    pub fn total(&self, w: &ScoringWeights) -> f32 {
        self.lexical * w.lexical
            + self.semantic * w.semantic
            + self.recency * w.recency
            + self.entity * w.entity
            + self.conversation * w.conversation
            + self.pinned * w.pinned
    }
}

/// Map an FTS5 `bm25()` value (lower/more negative is better) into `[0, 1)`.
pub fn lexical_from_bm25(bm25: f64) -> f32 {
    let relevance = (-bm25).max(0.0);
    (relevance / (1.0 + relevance)) as f32
}

/// Exponential decay: 1.0 now, 0.5 after one half-life.
pub fn recency(at: Timestamp, now: Timestamp, half_life: DurationMs) -> f32 {
    if half_life.as_millis() == 0 {
        return 0.0;
    }
    let age = now.since(at).as_millis() as f64;
    (0.5f64).powf(age / half_life.as_millis() as f64) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bm25_mapping_is_monotonic() {
        assert_eq!(lexical_from_bm25(0.0), 0.0);
        assert!(lexical_from_bm25(-5.0) > lexical_from_bm25(-1.0));
        assert!(lexical_from_bm25(-1e9) <= 1.0);
        assert_eq!(lexical_from_bm25(3.0), 0.0, "positive bm25 never boosts");
    }

    #[test]
    fn recency_halves_each_half_life() {
        let hl = DurationMs::from_days(1);
        let now = Timestamp(10 * 86_400_000);
        assert!((recency(now, now, hl) - 1.0).abs() < 1e-6);
        let one = recency(Timestamp(9 * 86_400_000), now, hl);
        assert!((one - 0.5).abs() < 1e-6);
    }

    #[test]
    fn total_weights_components() {
        let s = RetrievalScore {
            lexical: 1.0,
            pinned: 1.0,
            ..Default::default()
        };
        let w = ScoringWeights::default();
        assert!((s.total(&w) - 1.5).abs() < 1e-6);
    }
}
