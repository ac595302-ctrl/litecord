//! OAuth2 authorization-code flow with PKCE (RFC 7636) for Discord.
//!
//! This module is transport-free and fully testable: it generates the PKCE
//! verifier/challenge and CSRF `state`, builds the authorization URL, and
//! validates the redirect. Exchanging the code for tokens is delegated to a
//! [`TokenExchange`] implementation — with the Social SDK that is the SDK's
//! own `GetToken` call, so Litecord never needs a client secret.
//!
//! Security properties:
//! * the verifier is a [`Secret`] and never leaves this process;
//! * the redirect must carry the exact `state` we generated (CSRF);
//! * errors never echo tokens or codes.

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use litecord_core::secrets::Secret;
use litecord_types::Timestamp;

pub const DISCORD_AUTHORIZE_URL: &str = "https://discord.com/oauth2/authorize";

/// Scopes needed by the Social SDK's full feature set.
pub const DEFAULT_SCOPES: &[&str] = &["openid", "sdk.social_layer"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OAuthError {
    #[error("authorization was denied: {0}")]
    Denied(String),
    #[error("redirect state does not match the sign-in attempt")]
    StateMismatch,
    #[error("redirect is missing the authorization code")]
    MissingCode,
    #[error("malformed redirect URL")]
    Malformed,
    #[error("os randomness unavailable")]
    Randomness,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    pub client_id: u64,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub authorize_url: String,
}

impl OAuthConfig {
    pub fn new(client_id: u64, redirect_uri: impl Into<String>) -> Self {
        Self {
            client_id,
            redirect_uri: redirect_uri.into(),
            scopes: DEFAULT_SCOPES.iter().map(|s| (*s).to_owned()).collect(),
            authorize_url: DISCORD_AUTHORIZE_URL.to_owned(),
        }
    }
}

/// One sign-in attempt.
#[derive(Debug)]
pub struct PkceSession {
    verifier: Secret<String>,
    challenge: String,
    state: String,
}

fn random_token(bytes: usize) -> Result<String, OAuthError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|_| OAuthError::Randomness)?;
    Ok(base64url(&buf))
}

/// Unpadded base64url (RFC 4648 §5).
pub fn base64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..chars {
            let idx = ((n >> (18 - 6 * i)) & 0x3f) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    out
}

/// S256 code challenge for a verifier.
pub fn code_challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

impl PkceSession {
    /// Fresh verifier (43 chars from 32 random bytes) and state.
    pub fn new() -> Result<Self, OAuthError> {
        let verifier = random_token(32)?;
        let challenge = code_challenge(&verifier);
        Ok(Self {
            verifier: Secret::new(verifier),
            challenge,
            state: random_token(16)?,
        })
    }

    pub fn state(&self) -> &str {
        &self.state
    }

    pub fn challenge(&self) -> &str {
        &self.challenge
    }

    /// For the token exchange only.
    pub fn verifier(&self) -> &Secret<String> {
        &self.verifier
    }

    pub fn authorization_url(&self, cfg: &OAuthConfig) -> String {
        let scopes = cfg.scopes.join(" ");
        format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
            cfg.authorize_url,
            cfg.client_id,
            percent_encode(&cfg.redirect_uri),
            percent_encode(&scopes),
            percent_encode(&self.state),
            percent_encode(&self.challenge),
        )
    }

    /// Validate the redirect and return the authorization code.
    pub fn parse_redirect(&self, redirect_url: &str) -> Result<Secret<String>, OAuthError> {
        let query = redirect_url
            .split_once('?')
            .map(|(_, q)| q)
            .ok_or(OAuthError::Malformed)?;
        let query = query.split('#').next().unwrap_or(query);
        let mut code = None;
        let mut state = None;
        let mut error = None;
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = percent_decode(v).ok_or(OAuthError::Malformed)?;
            match k {
                "code" => code = Some(v),
                "state" => state = Some(v),
                "error" => error = Some(v),
                _ => {}
            }
        }
        // Check state first: an attacker-crafted redirect must not even be
        // able to surface an error message.
        if state.as_deref() != Some(self.state.as_str()) {
            return Err(OAuthError::StateMismatch);
        }
        if let Some(e) = error {
            return Err(OAuthError::Denied(e));
        }
        code.filter(|c| !c.is_empty())
            .map(Secret::new)
            .ok_or(OAuthError::MissingCode)
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Tokens obtained from the exchange. Only the adapter ever sees these; they
/// go straight into a `SecretStore`.
#[derive(Debug)]
pub struct TokenSet {
    pub access_token: Secret<String>,
    pub refresh_token: Option<Secret<String>>,
    pub expires_at: Option<Timestamp>,
}

/// Exchanges an authorization code (+ PKCE verifier) for tokens. The Social
/// SDK implementation calls `Client::GetToken`; tests use a fake.
#[async_trait]
pub trait TokenExchange: Send + Sync + std::fmt::Debug {
    async fn exchange(
        &self,
        code: &Secret<String>,
        verifier: &Secret<String>,
        redirect_uri: &str,
    ) -> Result<TokenSet, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_challenge_matches_independent_implementation() {
        // Expected value computed with Python's hashlib + base64.urlsafe_b64encode.
        assert_eq!(
            code_challenge("dBjftJeZ4CVP-1mB5ZuDsxZVxnOiDKc3oi7JRg1aZ7N"),
            "fUlpwIe1ERdZPwBJv2DLndAhi52HSVR4lASEx8xuw08"
        );
    }

    #[test]
    fn base64url_padding_free() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn authorization_url_carries_pkce_and_state() {
        let s = PkceSession::new().unwrap();
        assert_eq!(s.verifier().expose_secret().len(), 43);
        let url = s.authorization_url(&OAuthConfig::new(42, "http://127.0.0.1:53134/callback"));
        assert!(
            url.starts_with("https://discord.com/oauth2/authorize?response_type=code&client_id=42")
        );
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A53134%2Fcallback"));
        assert!(url.contains("scope=openid%20sdk.social_layer"));
        assert!(url.contains(&format!("code_challenge={}", s.challenge())));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(
            !url.contains(s.verifier().expose_secret().as_str()),
            "verifier never in URL"
        );
    }

    #[test]
    fn redirect_validation() {
        let s = PkceSession::new().unwrap();
        let ok = format!("http://127.0.0.1/cb?code=abc%20123&state={}", s.state());
        assert_eq!(s.parse_redirect(&ok).unwrap().expose_secret(), "abc 123");
        assert_eq!(
            s.parse_redirect("http://127.0.0.1/cb?code=abc&state=forged")
                .err(),
            Some(OAuthError::StateMismatch)
        );
        let denied = format!("http://x/cb?error=access_denied&state={}", s.state());
        assert_eq!(
            s.parse_redirect(&denied).err(),
            Some(OAuthError::Denied("access_denied".into()))
        );
        let missing = format!("http://x/cb?state={}", s.state());
        assert_eq!(
            s.parse_redirect(&missing).err(),
            Some(OAuthError::MissingCode)
        );
        assert_eq!(
            s.parse_redirect("not a url").err(),
            Some(OAuthError::Malformed)
        );
        // Two sessions never share state.
        assert_ne!(PkceSession::new().unwrap().state(), s.state());
    }
}
