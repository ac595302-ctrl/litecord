//! Pluggable semantic embeddings.
//!
//! Semantic retrieval is optional. [`EmbeddingProvider`] is synchronous and
//! local by design (a remote provider can implement it behind its own queue).
//! [`HashingEmbedder`] is a deterministic, dependency-free bag-of-words
//! feature-hashing embedder — good enough for tests and as an offline
//! fallback, and honest about being lexical, not neural.

use litecord_core::{Error, ErrorKind};

pub trait EmbeddingProvider: Send + Sync + std::fmt::Debug {
    /// Stable identifier stored with vectors (changing it invalidates them).
    fn model_id(&self) -> &str;
    fn dims(&self) -> usize;
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error>;
}

/// Feature-hashing embedder over lowercase alphanumeric terms.
#[derive(Debug, Clone)]
pub struct HashingEmbedder {
    dims: usize,
    model_id: String,
}

impl HashingEmbedder {
    pub fn new(dims: usize) -> Self {
        let dims = dims.max(8);
        Self {
            dims,
            model_id: format!("litecord-hashing-v1-{dims}"),
        }
    }
}

impl Default for HashingEmbedder {
    fn default() -> Self {
        Self::new(256)
    }
}

/// FNV-1a; stable across platforms and releases.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

impl EmbeddingProvider for HashingEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dims(&self) -> usize {
        self.dims
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0f32; self.dims];
                for term in t
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| w.chars().count() >= 2)
                {
                    let h = fnv1a(&term.to_lowercase());
                    let idx = (h % self.dims as u64) as usize;
                    let sign = if (h >> 63) == 0 { 1.0 } else { -1.0 };
                    v[idx] += sign;
                }
                normalize(&mut v);
                v
            })
            .collect())
    }
}

/// Provider used when semantic search is disabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct DisabledEmbeddings;

impl EmbeddingProvider for DisabledEmbeddings {
    fn model_id(&self) -> &str {
        "disabled"
    }
    fn dims(&self) -> usize {
        0
    }
    fn embed(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, Error> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "semantic search is disabled",
        ))
    }
}

fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

/// Cosine similarity mapped to `[0, 1]` (negative similarity → 0).
pub fn cosine01(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        (dot / (na * nb)).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashing_embedder_is_deterministic_and_similarity_sensible() {
        let e = HashingEmbedder::default();
        let v = e
            .embed(&[
                "project prototype review",
                "prototype review for the project",
                "cats",
            ])
            .unwrap();
        assert_eq!(v[0], e.embed(&["project prototype review"]).unwrap()[0]);
        assert!(cosine01(&v[0], &v[1]) > cosine01(&v[0], &v[2]));
        assert!(DisabledEmbeddings.embed(&["x"]).is_err());
    }
}
