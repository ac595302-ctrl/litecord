//! Approval tokens.
//!
//! An [`ApprovalToken`] is issued only by the engine when the *user* approves a
//! proposal. It binds:
//!
//! * the action id,
//! * the exact payload hash at approval time (changing recipient, content or
//!   operation afterwards invalidates it),
//! * an expiry,
//! * a single-use nonce recorded in `action_approvals` (consumed on execute),
//!
//! and carries a keyed-blake3 MAC under a per-engine random key so tokens
//! cannot be fabricated or carried across app instances. Tokens are never
//! given to agents: the MCP/agent layer has no API that returns one.

use std::fmt;

use litecord_types::ids::ActionId;
use litecord_types::{DurationMs, Timestamp};

use crate::error::ActionError;

#[derive(Clone, PartialEq, Eq)]
pub struct ApprovalToken {
    action_id: ActionId,
    payload_hash: String,
    approved_at: Timestamp,
    expires_at: Timestamp,
    nonce: String,
    mac: [u8; 32],
}

impl ApprovalToken {
    pub fn action_id(&self) -> ActionId {
        self.action_id
    }
    pub fn payload_hash(&self) -> &str {
        &self.payload_hash
    }
    pub fn approved_at(&self) -> Timestamp {
        self.approved_at
    }
    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }
    pub(crate) fn nonce(&self) -> &str {
        &self.nonce
    }
}

impl fmt::Debug for ApprovalToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApprovalToken")
            .field("action_id", &self.action_id)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// Issues and verifies tokens with a process-local key.
pub struct TokenIssuer {
    key: [u8; 32],
}

impl fmt::Debug for TokenIssuer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenIssuer([REDACTED])")
    }
}

impl TokenIssuer {
    /// New issuer with a random key from the OS.
    pub fn new() -> Result<Self, ActionError> {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key)
            .map_err(|e| ActionError::Internal(format!("os randomness unavailable: {e}")))?;
        Ok(Self { key })
    }

    fn mac(
        &self,
        id: ActionId,
        hash: &str,
        at: Timestamp,
        exp: Timestamp,
        nonce: &str,
    ) -> [u8; 32] {
        let msg = format!(
            "{}|{}|{}|{}|{}",
            id.get(),
            hash,
            at.as_millis(),
            exp.as_millis(),
            nonce
        );
        *blake3::keyed_hash(&self.key, msg.as_bytes()).as_bytes()
    }

    pub(crate) fn issue(
        &self,
        action_id: ActionId,
        payload_hash: &str,
        now: Timestamp,
        ttl: DurationMs,
    ) -> Result<ApprovalToken, ActionError> {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw)
            .map_err(|e| ActionError::Internal(format!("os randomness unavailable: {e}")))?;
        let nonce: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let expires_at = now.saturating_add(ttl);
        let mac = self.mac(action_id, payload_hash, now, expires_at, &nonce);
        Ok(ApprovalToken {
            action_id,
            payload_hash: payload_hash.to_owned(),
            approved_at: now,
            expires_at,
            nonce,
            mac,
        })
    }

    /// Check authenticity and expiry (not single-use or payload match; the
    /// engine checks those against the database).
    pub(crate) fn verify(&self, token: &ApprovalToken, now: Timestamp) -> Result<(), ActionError> {
        let expected = self.mac(
            token.action_id,
            &token.payload_hash,
            token.approved_at,
            token.expires_at,
            &token.nonce,
        );
        // blake3::Hash equality is constant-time.
        if blake3::Hash::from(expected) != blake3::Hash::from(token.mac) {
            return Err(ActionError::InvalidToken);
        }
        if now >= token.expires_at {
            return Err(ActionError::TokenExpired);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_verify_expire_and_resist_tampering() {
        let issuer = TokenIssuer::new().unwrap();
        let t = issuer
            .issue(
                ActionId(1),
                "abc",
                Timestamp(1_000),
                DurationMs::from_secs(60),
            )
            .unwrap();
        issuer.verify(&t, Timestamp(2_000)).unwrap();
        assert!(matches!(
            issuer.verify(&t, Timestamp(61_000)),
            Err(ActionError::TokenExpired)
        ));

        let mut forged = t.clone();
        forged.payload_hash = "def".into();
        assert!(matches!(
            issuer.verify(&forged, Timestamp(2_000)),
            Err(ActionError::InvalidToken)
        ));

        let other = TokenIssuer::new().unwrap();
        assert!(matches!(
            other.verify(&t, Timestamp(2_000)),
            Err(ActionError::InvalidToken)
        ));
        assert!(!format!("{t:?}").contains(t.nonce()));
    }
}
