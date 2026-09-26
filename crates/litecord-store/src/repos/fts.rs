//! Turning untrusted text into safe FTS5 `MATCH` expressions.
//!
//! User and agent queries are never passed to FTS5 verbatim (its query syntax
//! has operators, column filters and can raise syntax errors). Instead the
//! input is tokenized into alphanumeric terms, each term is double-quoted,
//! and terms are combined with `AND` or `OR`.

/// How terms are combined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtsMode {
    /// Every term must match (precise).
    All,
    /// Any term may match; bm25 ranks documents matching more terms higher
    /// (recall-oriented, used by retrieval).
    Any,
}

/// Maximum number of terms kept from a query.
pub const MAX_TERMS: usize = 16;

/// Tokenize `input` into lowercase search terms (letters/digits, len >= 2).
pub fn terms(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in input.split(|c: char| !c.is_alphanumeric()) {
        if raw.chars().count() < 2 {
            continue;
        }
        let t = raw.to_lowercase();
        if !out.contains(&t) {
            out.push(t);
        }
        if out.len() >= MAX_TERMS {
            break;
        }
    }
    out
}

/// Build a safe MATCH expression, or `None` if the input has no usable terms.
/// When `prefix_last` is set the final term also matches as a prefix (for
/// as-you-type search).
pub fn match_expr(input: &str, mode: FtsMode, prefix_last: bool) -> Option<String> {
    let terms = terms(input);
    if terms.is_empty() {
        return None;
    }
    let last = terms.len() - 1;
    let quoted: Vec<String> = terms
        .iter()
        .enumerate()
        .map(|(i, t)| {
            // Terms contain only alphanumerics, so no quote escaping needed.
            if prefix_last && i == last {
                format!("\"{t}\"*")
            } else {
                format!("\"{t}\"")
            }
        })
        .collect();
    let joiner = match mode {
        FtsMode::All => " AND ",
        FtsMode::Any => " OR ",
    };
    Some(quoted.join(joiner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_input_is_neutralized() {
        let e = match_expr(r#"proto" OR content:* NEAR( -x "#, FtsMode::All, false).unwrap();
        assert_eq!(e, r#""proto" AND "or" AND "content" AND "near""#);
        assert_eq!(match_expr("  ! ? ", FtsMode::Any, false), None);
    }

    #[test]
    fn prefix_and_any_mode() {
        assert_eq!(
            match_expr("Project upd", FtsMode::Any, true).unwrap(),
            r#""project" OR "upd"*"#
        );
    }
}
