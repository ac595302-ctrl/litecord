//! Real network transport (feature `discord-bot`): reqwest for REST,
//! tokio-tungstenite for the gateway websocket, both over rustls.
//!
//! The token is attached per request as `Authorization: Bot <token>` and is
//! never logged; transport errors are mapped to content-free
//! [`BackendError`]s. Not exercised against live Discord in CI (no token);
//! the protocol layers above it are covered by deterministic tests.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use litecord_core::ports::BackendError;
use litecord_core::secrets::Secret;

use super::rest::{self, Method, RestRequest, API_BASE};
use super::transport::{BotTransport, GatewaySocket, SocketEvent};

#[derive(Debug, Default)]
struct RateLimits {
    global: Option<std::time::Instant>,
    routes: std::collections::HashMap<String, std::time::Instant>,
    buckets: std::collections::HashMap<String, String>,
}
impl RateLimits {
    fn key(req: &RestRequest) -> String {
        let parts: Vec<_> = req.path.split('/').collect();
        let major = if matches!(parts.get(1), Some(&"channels" | &"guilds")) {
            parts.get(2).copied().unwrap_or("")
        } else {
            ""
        };
        format!("{}:{major}", req.route)
    }
    fn remaining(&self, key: &str) -> Option<litecord_types::DurationMs> {
        let bucket = self.buckets.get(key).map(String::as_str).unwrap_or(key);
        self.global
            .into_iter()
            .chain(self.routes.get(bucket).copied())
            .max()
            .and_then(|t| t.checked_duration_since(std::time::Instant::now()))
            .map(|d| {
                litecord_types::DurationMs::from_millis(
                    d.as_millis().min(u128::from(u64::MAX)) as u64
                )
            })
    }
}

const USER_AGENT: &str = concat!(
    "DiscordBot (",
    env!("CARGO_PKG_REPOSITORY"),
    ", ",
    env!("CARGO_PKG_VERSION"),
    ")"
);

#[derive(Debug, Clone)]
pub struct HttpTransport {
    client: reqwest::Client,
    user_session: bool,
    access: litecord_types::capability::SessionAccessMode,
    limits: std::sync::Arc<tokio::sync::Mutex<RateLimits>>,
}

impl HttpTransport {
    pub fn new() -> Result<Self, BackendError> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| BackendError::Sdk("could not initialise HTTP client".into()))?;
        Ok(Self {
            client,
            user_session: false,
            access: litecord_types::capability::SessionAccessMode::ReadOnly,
            limits: Default::default(),
        })
    }

    /// Account transport with a read-only default. The credential has no Bot prefix.
    pub fn user_session() -> Result<Self, BackendError> {
        Self::user_session_with_access(litecord_types::capability::SessionAccessMode::ReadOnly)
    }
    pub fn user_session_with_access(
        access: litecord_types::capability::SessionAccessMode,
    ) -> Result<Self, BackendError> {
        let mut transport = Self::new()?;
        transport.user_session = true;
        transport.access = access;
        transport.client = reqwest::Client::builder()
            .user_agent(concat!("Litecord/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| BackendError::Offline)?;
        Ok(transport)
    }
}

#[async_trait]
impl BotTransport for HttpTransport {
    async fn request(
        &self,
        token: &Secret<String>,
        req: &RestRequest,
    ) -> Result<Value, BackendError> {
        if self.user_session && !crate::user_session::allows_request(self.access, req) {
            return Err(BackendError::Unsupported {
                capability: litecord_types::capability::Capability::DmSend,
            });
        }
        let route_key = RateLimits::key(req);
        if let Some(retry_after) = self.limits.lock().await.remaining(&route_key) {
            return Err(BackendError::RateLimited { retry_after });
        }
        let url = format!("{API_BASE}{}", req.path);
        let builder = match req.method {
            Method::Get => self.client.get(&url),
            Method::Post => self.client.post(&url),
            Method::Put => self.client.put(&url),
            Method::Patch => self.client.patch(&url),
            Method::Delete => self.client.delete(&url),
        };
        // Marked sensitive so reqwest/hyper redact it from Debug output.
        let authorization = if self.user_session {
            token.expose_secret().to_owned()
        } else {
            format!("Bot {}", token.expose_secret())
        };
        let mut auth = reqwest::header::HeaderValue::from_str(&authorization)
            .map_err(|_| BackendError::Authentication("malformed Discord credential".into()))?;
        auth.set_sensitive(true);
        let mut builder = builder.header(reqwest::header::AUTHORIZATION, auth);
        if let Some(body) = &req.body {
            builder = builder.json(body);
        }
        // Network failures carry no request details (they could include the
        // URL or headers).
        let mut resp = builder
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .map_err(|_| {
                if self.user_session && req.method != Method::Get {
                    BackendError::DeliveryUncertain(
                        "Write outcome is uncertain; refresh the conversation before retrying"
                            .into(),
                    )
                } else {
                    BackendError::Offline
                }
            })?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<f64>().ok());
        let exhausted = resp
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            == Some("0");
        let reset_after = resp
            .headers()
            .get("x-ratelimit-reset-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<f64>().ok());
        let bucket = resp
            .headers()
            .get("x-ratelimit-bucket")
            .and_then(|v| v.to_str().ok())
            .filter(|b| b.len() <= 128)
            .map(str::to_owned);
        let global_header = resp
            .headers()
            .get("x-ratelimit-global")
            .and_then(|v| v.to_str().ok())
            == Some("true");
        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|_| {
            if self.user_session && req.method != Method::Get {
                BackendError::DeliveryUncertain(
                    "Write outcome is uncertain; refresh before retrying".into(),
                )
            } else {
                BackendError::Offline
            }
        })? {
            if bytes.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                return Err(BackendError::Sdk(
                    "Discord response exceeds the byte limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let body: Option<Value> = if bytes.is_empty() {
            None
        } else {
            serde_json::from_slice(&bytes).ok()
        };
        {
            let mut limits = self.limits.lock().await;
            if let Some(bucket) = bucket {
                if limits.buckets.len() >= 4096 {
                    limits.buckets.clear();
                }
                let major = route_key.rsplit(':').next().unwrap_or("");
                limits
                    .buckets
                    .insert(route_key.clone(), format!("bucket:{bucket}:{major}"));
            }
        }
        if exhausted || status == 429 {
            let seconds = retry_after
                .or_else(|| {
                    body.as_ref()
                        .and_then(|v| v.get("retry_after"))
                        .and_then(Value::as_f64)
                })
                .or(reset_after)
                .unwrap_or(1.0);
            let wait = Duration::from_secs_f64(if seconds.is_finite() {
                seconds.clamp(0.001, 86400.0)
            } else {
                1.0
            });
            let now = std::time::Instant::now();
            let mut limits = self.limits.lock().await;
            limits.routes.retain(|_, t| *t > now);
            if global_header
                || body
                    .as_ref()
                    .and_then(|v| v.get("global"))
                    .and_then(Value::as_bool)
                    == Some(true)
            {
                limits.global = Some(now + wait);
            } else {
                let bucket_key = limits.buckets.get(&route_key).cloned().unwrap_or(route_key);
                limits.routes.insert(bucket_key, now + wait);
            }
        }
        if (200..300).contains(&status) {
            Ok(body.unwrap_or(Value::Null))
        } else {
            if self.user_session && status == 403 {
                return Err(BackendError::PermissionDenied {
                    what: "Discord resource is not accessible to this account".into(),
                });
            }
            if self.user_session && req.method != Method::Get && status >= 500 {
                return Err(BackendError::DeliveryUncertain(
                    "Discord could not confirm the write; refresh before retrying".into(),
                ));
            }
            Err(rest::error_for_status(status, body.as_ref(), retry_after))
        }
    }

    async fn connect(&self, url: &str) -> Result<Box<dyn GatewaySocket>, BackendError> {
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(8 * 1024 * 1024))
            .max_frame_size(Some(8 * 1024 * 1024));
        let (ws, _) = tokio_tungstenite::connect_async_with_config(url, Some(config), false)
            .await
            .map_err(|_| BackendError::Offline)?;
        Ok(Box::new(WsSocket { ws }))
    }
}

struct WsSocket {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl std::fmt::Debug for WsSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WsSocket")
    }
}

#[async_trait]
impl GatewaySocket for WsSocket {
    async fn recv(&mut self) -> SocketEvent {
        loop {
            match self.ws.next().await {
                Some(Ok(WsMessage::Text(t))) => return SocketEvent::Text(t.to_string()),
                Some(Ok(WsMessage::Close(frame))) => {
                    return SocketEvent::Closed(frame.map(|f| u16::from(f.code)))
                }
                // Pings are answered by tungstenite; binary frames are not
                // used with JSON encoding.
                Some(Ok(_)) => continue,
                Some(Err(_)) | None => return SocketEvent::Closed(None),
            }
        }
    }

    async fn send(&mut self, text: String) -> Result<(), BackendError> {
        self.ws
            .send(WsMessage::Text(text.into()))
            .await
            .map_err(|_| BackendError::NotConnected)
    }

    async fn close(&mut self) {
        let _ = self.ws.close(None).await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn account_transport_rejects_write_before_network_or_credentials() {
        let transport = HttpTransport::user_session().unwrap();
        let request = RestRequest {
            method: Method::Post,
            path: "/channels/2/messages".into(),
            route: "POST /channels/{channel.id}/messages",
            body: None,
        };
        let error = transport
            .request(&Secret::new("test-secret".into()), &request)
            .await
            .unwrap_err();
        assert!(matches!(error, BackendError::Unsupported { .. }));
        assert!(!format!("{error:?}").contains("test-secret"));
    }

    #[test]
    fn shared_bucket_applies_to_routes_with_the_same_major_id() {
        let mut limits = RateLimits::default();
        limits
            .buckets
            .insert("GET messages:2".into(), "bucket:x:2".into());
        limits
            .buckets
            .insert("GET message:2".into(), "bucket:x:2".into());
        limits.routes.insert(
            "bucket:x:2".into(),
            std::time::Instant::now() + Duration::from_secs(5),
        );
        assert!(limits.remaining("GET messages:2").is_some());
        assert!(limits.remaining("GET message:2").is_some());
        assert!(limits.remaining("GET messages:3").is_none());
        limits.global = Some(std::time::Instant::now() + Duration::from_secs(1));
        assert!(limits.remaining("GET messages:3").is_some());
    }
}
