//! [`BotBackend`]: an application-bot `SocialBackend` over any
//! [`BotTransport`].
//!
//! * Live events: a gateway driver task (owned by the backend, started by
//!   `connect`, cancelled and joined by `disconnect`) runs the pure
//!   [`GatewaySession`] state machine, translates dispatches with
//!   [`translate::dispatch`] and delivers them with `try_send` (the driver
//!   must keep heartbeating, so it never blocks on a full queue; overflow is
//!   handled by the app's resync path).
//! * Reads/writes: REST via [`rest`] request builders.
//! * Everything is stamped `DiscordSource::BotGateway` (identity
//!   `application_bot`); the bot never impersonates the user.
//! * The token lives in this struct as a [`Secret`] and is only passed to
//!   the transport; `Debug` redacts it.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lru::LruCache;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use litecord_core::bus::IngestSender;
use litecord_core::clock::SharedClock;
use litecord_core::events::{DiscordEvent, SourceEnvelope};
use litecord_core::ports::{BackendError, BackendResult, SocialBackend};
use litecord_core::secrets::Secret;
use litecord_types::actions::MessageTarget;
use litecord_types::capability::{BackendMode, Capability, CapabilitySet, SupportLevel};
use litecord_types::ids::*;
use litecord_types::provenance::DiscordSource;
use litecord_types::social::*;

use super::gateway::{GatewayConfig, GatewaySession, Intents, Output};
use super::rest::{self, RestRequest};
use super::translate;
use super::transport::{BotTransport, GatewaySocket, SocketEvent};

#[derive(Debug, Clone)]
pub struct BotConfig {
    pub intents: Intents,
    pub reconnect_min: Duration,
    pub reconnect_max: Duration,
    /// How many message→channel mappings to remember (edits/deletes need
    /// the channel; the REST API is channel-scoped).
    pub message_index_capacity: usize,
}

impl Default for BotConfig {
    fn default() -> Self {
        Self {
            intents: Intents::default_bot(),
            reconnect_min: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(60),
            message_index_capacity: 10_000,
        }
    }
}

#[derive(Debug)]
struct Driver {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

struct Shared {
    transport: Arc<dyn BotTransport>,
    token: Secret<String>,
    cfg: BotConfig,
    clock: SharedClock,
    message_channels: Mutex<LruCache<MessageId, ChannelId>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BotBackend")
            .field("transport", &self.transport)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl Shared {
    async fn call(&self, req: RestRequest) -> BackendResult<serde_json::Value> {
        self.transport.request(&self.token, &req).await
    }

    fn remember(&self, message: MessageId, channel: ChannelId) {
        if let Ok(mut idx) = self.message_channels.lock() {
            idx.put(message, channel);
        }
    }

    fn channel_of(&self, message: MessageId) -> Option<ChannelId> {
        self.message_channels
            .lock()
            .ok()
            .and_then(|mut idx| idx.get(&message).copied())
    }

    fn envelope(&self, event: DiscordEvent) -> SourceEnvelope {
        SourceEnvelope::new(DiscordSource::BotGateway, self.clock.now(), event)
    }

    fn emit(&self, sink: &IngestSender, event: DiscordEvent) {
        if let DiscordEvent::MessageCreated { message } | DiscordEvent::MessageUpdated { message } =
            &event
        {
            self.remember(message.id, ChannelId(message.conversation_id.get()));
        }
        let _ = sink.try_send(self.envelope(event));
    }
}

fn bad_payload(e: translate::TranslateError) -> BackendError {
    BackendError::Sdk(format!("unexpected discord payload: {e}"))
}

#[derive(Debug)]
pub struct BotBackend {
    shared: Arc<Shared>,
    driver: Mutex<Option<Driver>>,
}

impl BotBackend {
    pub fn new(
        transport: Arc<dyn BotTransport>,
        token: Secret<String>,
        cfg: BotConfig,
        clock: SharedClock,
    ) -> Self {
        let cap = NonZeroUsize::new(cfg.message_index_capacity.max(1)).unwrap_or(NonZeroUsize::MIN);
        Self {
            shared: Arc::new(Shared {
                transport,
                token,
                cfg,
                clock,
                message_channels: Mutex::new(LruCache::new(cap)),
            }),
            driver: Mutex::new(None),
        }
    }

    fn take_driver(&self) -> Option<Driver> {
        match self.driver.lock() {
            Ok(mut d) => d.take(),
            Err(p) => p.into_inner().take(),
        }
    }
}

/// Deterministic-enough jitter for the first heartbeat, from OS randomness.
fn jitter() -> f64 {
    let mut b = [0u8; 2];
    if getrandom::fill(&mut b).is_err() {
        return 0.5;
    }
    f64::from(u16::from_le_bytes(b)) / 65_536.0
}

enum Next {
    Reconnect {
        resume_url: Option<String>,
        wait: bool,
    },
    Stop,
}

async fn run_socket(
    shared: &Shared,
    session: &mut GatewaySession,
    socket: &mut dyn GatewaySocket,
    sink: &IngestSender,
    cancel: &CancellationToken,
    backoff: &mut Duration,
) -> Next {
    loop {
        let now = shared.clock.now();
        let sleep = session
            .next_deadline()
            .map(|d| Duration::from_millis(d.since(now).as_millis().max(1)))
            .unwrap_or(Duration::from_secs(3_600));
        let outputs = tokio::select! {
            _ = cancel.cancelled() => {
                socket.close().await;
                return Next::Stop;
            }
            ev = socket.recv() => match ev {
                SocketEvent::Text(t) => session.on_frame(&t, shared.clock.now(), jitter()),
                SocketEvent::Closed(code) => session.on_close(code),
            },
            _ = tokio::time::sleep(sleep) => session.on_tick(shared.clock.now()),
        };
        for out in outputs {
            match out {
                Output::Send(text) => {
                    if socket.send(text).await.is_err() {
                        let _ = session.on_close(None);
                        return Next::Reconnect {
                            resume_url: None,
                            wait: false,
                        };
                    }
                }
                Output::Dispatch { event, data } => match translate::dispatch(&event, &data) {
                    Ok(events) => {
                        for e in events {
                            shared.emit(sink, e);
                        }
                    }
                    // Event name only: payloads may contain message content.
                    Err(e) => tracing::warn!(event = %event, error = %e, "untranslatable dispatch"),
                },
                Output::Ready => *backoff = shared.cfg.reconnect_min,
                Output::Resumed => {
                    *backoff = shared.cfg.reconnect_min;
                    shared.emit(
                        sink,
                        DiscordEvent::SessionChanged {
                            state: SessionState::Ready,
                        },
                    );
                }
                Output::Reconnect { resume, url } => {
                    socket.close().await;
                    return Next::Reconnect {
                        resume_url: if resume { url } else { None },
                        // Discord asks for a 1–5 s pause before a fresh identify.
                        wait: !resume,
                    };
                }
                Output::Fatal(message) => {
                    socket.close().await;
                    shared.emit(
                        sink,
                        DiscordEvent::SessionChanged {
                            state: SessionState::Error { message },
                        },
                    );
                    return Next::Stop;
                }
            }
        }
    }
}

async fn drive(shared: Arc<Shared>, sink: IngestSender, cancel: CancellationToken) {
    let mut session = GatewaySession::new(GatewayConfig {
        token: Secret::new(shared.token.expose_secret().clone()),
        intents: shared.cfg.intents,
        properties_os: std::env::consts::OS.to_owned(),
    });
    let mut backoff = shared.cfg.reconnect_min;
    let mut resume_url: Option<String> = None;
    shared.emit(
        &sink,
        DiscordEvent::SessionChanged {
            state: SessionState::Connecting,
        },
    );
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let url = match resume_url.take() {
            Some(u) => Ok(rest::gateway_connect_url(&u)),
            None => match shared.call(rest::gateway_bot()).await {
                Ok(v) => rest::parse_gateway_url(&v).map_err(bad_payload),
                Err(e) => Err(e),
            },
        };
        let socket = match url {
            Ok(u) => shared.transport.connect(&u).await,
            Err(e) => Err(e),
        };
        let mut socket = match socket {
            Ok(s) => s,
            Err(BackendError::Authentication(m)) => {
                shared.emit(
                    &sink,
                    DiscordEvent::SessionChanged {
                        state: SessionState::Error { message: m },
                    },
                );
                return;
            }
            Err(e) => {
                tracing::debug!(error = %e, "bot gateway connect failed; backing off");
                shared.emit(
                    &sink,
                    DiscordEvent::SessionChanged {
                        state: SessionState::Reconnecting,
                    },
                );
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(shared.cfg.reconnect_max);
                continue;
            }
        };
        session.on_reconnected();
        match run_socket(
            &shared,
            &mut session,
            socket.as_mut(),
            &sink,
            &cancel,
            &mut backoff,
        )
        .await
        {
            Next::Stop => return,
            Next::Reconnect {
                resume_url: url,
                wait,
            } => {
                shared.emit(
                    &sink,
                    DiscordEvent::SessionChanged {
                        state: SessionState::Reconnecting,
                    },
                );
                resume_url = url;
                // Always pause before reconnecting. The delay doubles while
                // sessions keep dropping before READY/RESUMED (which reset
                // it), so a flapping gateway is never hammered. Discord asks
                // for at least 1 s before a fresh identify.
                let pause = if wait {
                    backoff.max(Duration::from_secs(1))
                } else {
                    backoff
                };
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(pause) => {}
                }
                backoff = (backoff * 2).min(shared.cfg.reconnect_max);
            }
        }
    }
}

#[async_trait]
impl SocialBackend for BotBackend {
    fn source(&self) -> DiscordSource {
        DiscordSource::BotGateway
    }

    fn mode(&self) -> BackendMode {
        BackendMode::BotBridge
    }

    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet::default()
            .with(Capability::CurrentUser, SupportLevel::Full)
            .with(Capability::GuildListing, SupportLevel::Full)
            .with(Capability::GuildChannels, SupportLevel::Full)
            .with(Capability::GuildMessages, SupportLevel::Full)
    }

    async fn connect(&self, sink: IngestSender) -> BackendResult<()> {
        let mut guard = self
            .driver
            .lock()
            .map_err(|_| BackendError::Sdk("lock poisoned".into()))?;
        if guard.as_ref().is_some_and(|d| !d.handle.is_finished()) {
            return Ok(());
        }
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(drive(self.shared.clone(), sink, cancel.clone()));
        *guard = Some(Driver { cancel, handle });
        Ok(())
    }

    async fn disconnect(&self) -> BackendResult<()> {
        if let Some(d) = self.take_driver() {
            d.cancel.cancel();
            if tokio::time::timeout(Duration::from_secs(3), d.handle)
                .await
                .is_err()
            {
                tracing::warn!("bot gateway driver did not stop in time");
            }
        }
        Ok(())
    }

    async fn current_user(&self) -> BackendResult<User> {
        let v = self.shared.call(rest::current_user()).await?;
        translate::user(&v).map_err(bad_payload)
    }

    async fn guilds(&self) -> BackendResult<Vec<Guild>> {
        let v = self.shared.call(rest::current_user_guilds()).await?;
        rest::parse_guilds(&v).map_err(bad_payload)
    }

    async fn guild_channels(&self, guild_id: GuildId) -> BackendResult<Vec<Channel>> {
        let v = self.shared.call(rest::guild_channels(guild_id)).await?;
        rest::parse_channels(&v, guild_id).map_err(bad_payload)
    }

    /// Readable guild channels as conversations (one request per guild).
    async fn conversations(&self) -> BackendResult<Vec<Conversation>> {
        let mut out = Vec::new();
        for g in self.guilds().await? {
            for c in self.guild_channels(g.id).await? {
                out.extend(translate::channel_conversation(&c));
            }
        }
        Ok(out)
    }

    async fn messages(
        &self,
        conversation_id: ConversationId,
        limit: u32,
    ) -> BackendResult<Vec<Message>> {
        let channel = ChannelId(conversation_id.get());
        let v = self
            .shared
            .call(rest::channel_messages(channel, limit, None))
            .await?;
        let messages = rest::parse_messages(&v).map_err(bad_payload)?;
        for m in &messages {
            self.shared.remember(m.id, channel);
        }
        Ok(messages)
    }

    async fn send_message(&self, target: &MessageTarget, content: &str) -> BackendResult<Message> {
        let MessageTarget::Conversation { conversation_id } = target else {
            return Err(BackendError::Unsupported {
                capability: Capability::DmSend,
            });
        };
        let channel = ChannelId(conversation_id.get());
        let v = self
            .shared
            .call(rest::create_message(channel, content))
            .await?;
        let m = translate::message(&v).map_err(bad_payload)?;
        self.shared.remember(m.id, channel);
        Ok(m)
    }

    async fn edit_message(&self, message_id: MessageId, content: &str) -> BackendResult<()> {
        let channel = self
            .shared
            .channel_of(message_id)
            .ok_or_else(|| BackendError::NotFound {
                what: format!("channel of message {message_id}"),
            })?;
        self.shared
            .call(rest::edit_message(channel, message_id, content))
            .await?;
        Ok(())
    }

    async fn delete_message(&self, message_id: MessageId) -> BackendResult<()> {
        let channel = self
            .shared
            .channel_of(message_id)
            .ok_or_else(|| BackendError::NotFound {
                what: format!("channel of message {message_id}"),
            })?;
        self.shared
            .call(rest::delete_message(channel, message_id))
            .await?;
        Ok(())
    }
}
