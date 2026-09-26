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
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use litecord_core::ports::BackendError;
use litecord_core::secrets::Secret;

use super::rest::{self, Method, RestRequest, API_BASE};
use super::transport::{BotTransport, GatewaySocket, SocketEvent};

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
}

impl HttpTransport {
    pub fn new() -> Result<Self, BackendError> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| BackendError::Sdk("could not initialise HTTP client".into()))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl BotTransport for HttpTransport {
    async fn request(
        &self,
        token: &Secret<String>,
        req: &RestRequest,
    ) -> Result<Value, BackendError> {
        let url = format!("{API_BASE}{}", req.path);
        let builder = match req.method {
            Method::Get => self.client.get(&url),
            Method::Post => self.client.post(&url),
            Method::Patch => self.client.patch(&url),
            Method::Delete => self.client.delete(&url),
        };
        // Marked sensitive so reqwest/hyper redact it from Debug output.
        let mut auth =
            reqwest::header::HeaderValue::from_str(&format!("Bot {}", token.expose_secret()))
                .map_err(|_| BackendError::Authentication("malformed bot token".into()))?;
        auth.set_sensitive(true);
        let mut builder = builder.header(reqwest::header::AUTHORIZATION, auth);
        if let Some(body) = &req.body {
            builder = builder.json(body);
        }
        // Network failures carry no request details (they could include the
        // URL or headers).
        let resp = builder.send().await.map_err(|_| BackendError::Offline)?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<f64>().ok());
        let bytes = resp.bytes().await.map_err(|_| BackendError::Offline)?;
        let body: Option<Value> = if bytes.is_empty() {
            None
        } else {
            serde_json::from_slice(&bytes).ok()
        };
        if (200..300).contains(&status) {
            Ok(body.unwrap_or(Value::Null))
        } else {
            Err(rest::error_for_status(status, body.as_ref(), retry_after))
        }
    }

    async fn connect(&self, url: &str) -> Result<Box<dyn GatewaySocket>, BackendError> {
        let (ws, _) = tokio_tungstenite::connect_async(url)
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
