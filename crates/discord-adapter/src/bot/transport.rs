//! Transport abstraction for the bot adapter.
//!
//! The backend never touches sockets or HTTP directly: it talks to a
//! [`BotTransport`]. The real implementation (`discord-bot` feature) uses
//! reqwest + tokio-tungstenite; tests use [`fake::FakeTransport`]. The bot
//! token is passed per request and never stored in the transport's `Debug`.

use async_trait::async_trait;
use serde_json::Value;

use litecord_core::ports::BackendError;
use litecord_core::secrets::Secret;

use super::rest::RestRequest;

/// Something received on the gateway socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketEvent {
    Text(String),
    /// The socket closed; `None` means a transport error without a close code.
    Closed(Option<u16>),
}

#[async_trait]
pub trait GatewaySocket: Send + std::fmt::Debug {
    async fn recv(&mut self) -> SocketEvent;
    async fn send(&mut self, text: String) -> Result<(), BackendError>;
    async fn close(&mut self);
}

#[async_trait]
pub trait BotTransport: Send + Sync + std::fmt::Debug {
    /// Perform one REST request against the Discord API and return the JSON
    /// body (Null for 204). Errors are already mapped with
    /// [`super::rest::error_for_status`].
    async fn request(
        &self,
        token: &Secret<String>,
        req: &RestRequest,
    ) -> Result<Value, BackendError>;

    /// Open a gateway websocket.
    async fn connect(&self, url: &str) -> Result<Box<dyn GatewaySocket>, BackendError>;
}

/// Scripted in-memory transport for tests and offline development.
pub mod fake {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use tokio::sync::mpsc;

    use super::*;
    use crate::bot::rest::Method;

    #[derive(Debug, Default)]
    struct Inner {
        responses: HashMap<String, Value>,
        requests: Vec<RestRequest>,
        /// Frames to feed each newly opened socket; the test keeps the sender.
        pending_socket: Option<(mpsc::Receiver<SocketEvent>, mpsc::UnboundedSender<String>)>,
        connects: usize,
    }

    /// A transport whose REST answers and gateway frames are scripted.
    #[derive(Debug, Clone, Default)]
    pub struct FakeTransport {
        inner: Arc<Mutex<Inner>>,
    }

    /// Test-side handle of a scripted socket.
    #[derive(Debug)]
    pub struct SocketHandle {
        pub to_client: mpsc::Sender<SocketEvent>,
        pub from_client: mpsc::UnboundedReceiver<String>,
    }

    impl FakeTransport {
        pub fn new() -> Self {
            Self::default()
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
            match self.inner.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            }
        }

        /// Respond to `METHOD path` (e.g. `"GET /users/@me"`) with `body`.
        pub fn respond(&self, method_and_path: &str, body: Value) {
            self.lock()
                .responses
                .insert(method_and_path.to_owned(), body);
        }

        /// Prepare the next socket `connect` will return.
        pub fn next_socket(&self) -> SocketHandle {
            let (to_client, rx) = mpsc::channel(64);
            let (tx, from_client) = mpsc::unbounded_channel();
            self.lock().pending_socket = Some((rx, tx));
            SocketHandle {
                to_client,
                from_client,
            }
        }

        pub fn requests(&self) -> Vec<RestRequest> {
            self.lock().requests.clone()
        }

        pub fn connects(&self) -> usize {
            self.lock().connects
        }
    }

    fn key(req: &RestRequest) -> String {
        let m = match req.method {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        };
        let path = req.path.split('?').next().unwrap_or(&req.path);
        format!("{m} {path}")
    }

    #[async_trait]
    impl BotTransport for FakeTransport {
        async fn request(
            &self,
            _token: &Secret<String>,
            req: &RestRequest,
        ) -> Result<Value, BackendError> {
            let mut g = self.lock();
            g.requests.push(req.clone());
            g.responses
                .get(&key(req))
                .cloned()
                .ok_or_else(|| BackendError::NotFound { what: key(req) })
        }

        async fn connect(&self, _url: &str) -> Result<Box<dyn GatewaySocket>, BackendError> {
            let mut g = self.lock();
            g.connects += 1;
            match g.pending_socket.take() {
                Some((rx, tx)) => Ok(Box::new(FakeSocket { rx, tx })),
                None => Err(BackendError::Offline),
            }
        }
    }

    #[derive(Debug)]
    struct FakeSocket {
        rx: mpsc::Receiver<SocketEvent>,
        tx: mpsc::UnboundedSender<String>,
    }

    #[async_trait]
    impl GatewaySocket for FakeSocket {
        async fn recv(&mut self) -> SocketEvent {
            self.rx.recv().await.unwrap_or(SocketEvent::Closed(None))
        }

        async fn send(&mut self, text: String) -> Result<(), BackendError> {
            self.tx.send(text).map_err(|_| BackendError::NotConnected)
        }

        async fn close(&mut self) {
            self.rx.close();
        }
    }
}
