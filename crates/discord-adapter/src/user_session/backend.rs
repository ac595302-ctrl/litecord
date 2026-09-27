use super::{translate, SessionTransport, SocketEvent};
use crate::bot::{
    gateway::{GatewaySession, Output},
    rest::{self, Method, RestRequest},
    translate as common,
};
use async_trait::async_trait;
use litecord_core::bus::IngestSender;
use litecord_core::clock::SharedClock;
use litecord_core::events::{DiscordEvent, SourceEnvelope};
use litecord_core::ports::{
    BackendError, BackendResult, HistoryPage, HistoryPageRequest, SocialBackend,
};
use litecord_core::secrets::{Secret, SecretKey, SecretStore};
use litecord_types::actions::MessageTarget;
use litecord_types::capability::{
    AuthStep, BackendMode, Capability, CapabilitySet, HistoryCapability, SessionAccessMode,
    SupportLevel,
};
use litecord_types::ids::*;
use litecord_types::provenance::DiscordSource;
use litecord_types::social::*;
use serde_json::json;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Default)]
struct Metadata {
    user: Option<User>,
    guilds: BTreeMap<GuildId, Guild>,
    channels: BTreeMap<ChannelId, Channel>,
    conversations: BTreeMap<ConversationId, Conversation>,
    relationships: BTreeMap<UserId, (Relationship, Option<User>)>,
}

struct Shared {
    access: SessionAccessMode,
    transport: Arc<dyn SessionTransport>,
    secrets: Arc<dyn SecretStore>,
    credential: Mutex<Option<Secret<String>>>,
    pinned_account: Mutex<Option<UserId>>,
    metadata: Mutex<Metadata>,
    sink: Mutex<Option<IngestSender>>,
    generation: Arc<AtomicU64>,
    clock: SharedClock,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserSession")
            .field("transport", &self.transport)
            .finish_non_exhaustive()
    }
}

impl Shared {
    fn token(&self) -> BackendResult<Secret<String>> {
        self.credential
            .lock()
            .map_err(|_| BackendError::Offline)?
            .as_ref()
            .map(|s| Secret::new(s.expose_secret().clone()))
            .ok_or(BackendError::NotConnected)
    }
    async fn call(&self, req: RestRequest) -> BackendResult<Value> {
        let (generation, token) = {
            let credential = self.credential.lock().map_err(|_| BackendError::Offline)?;
            let token = credential
                .as_ref()
                .map(|s| Secret::new(s.expose_secret().clone()))
                .ok_or(BackendError::NotConnected)?;
            (self.generation.load(Ordering::Acquire), token)
        };
        let value = self.transport.request(&token, &req).await.map_err(|e| {
            if req.method != Method::Get
                && matches!(e, BackendError::Offline | BackendError::Sdk(_))
            {
                BackendError::DeliveryUncertain(
                    "Discord write outcome is unknown; wait for reconciliation before resending"
                        .into(),
                )
            } else {
                e
            }
        })?;
        if generation != self.generation.load(Ordering::Acquire) {
            return Err(BackendError::NotConnected);
        }
        Ok(value)
    }
    fn emit(&self, event: DiscordEvent) {
        if let Ok(sink) = self.sink.lock() {
            if let Some(sink) = sink.as_ref() {
                let _ = sink.try_send(SourceEnvelope::new(
                    DiscordSource::UserSession,
                    self.clock.now(),
                    event,
                ));
            }
        }
    }
    fn check_account(&self, user: &User) -> BackendResult<()> {
        if user.is_bot {
            return Err(BackendError::Authentication(
                "this connection requires a user account, not a bot".into(),
            ));
        }
        let pin = self
            .pinned_account
            .lock()
            .map_err(|_| BackendError::Offline)?;
        if pin.is_some_and(|id| id != user.id) {
            return Err(BackendError::Authentication(
                "this database belongs to a different account; use a separate data directory"
                    .into(),
            ));
        }
        Ok(())
    }
    fn remember(&self, event: &DiscordEvent) {
        if let Ok(mut state) = self.metadata.lock() {
            match event {
                DiscordEvent::CurrentUser { user } => state.user = Some(user.clone()),
                DiscordEvent::GuildUpserted { guild } => {
                    state.guilds.insert(guild.id, guild.clone());
                }
                DiscordEvent::GuildRemoved { guild_id } => {
                    state.guilds.remove(guild_id);
                    state.channels.retain(|_, c| c.guild_id != *guild_id);
                    state
                        .conversations
                        .retain(|_, c| c.guild_id != Some(*guild_id));
                }
                DiscordEvent::ChannelUpserted { channel } => {
                    state.channels.insert(channel.id, channel.clone());
                }
                DiscordEvent::ConversationUpserted { conversation } => {
                    state
                        .conversations
                        .insert(conversation.id, conversation.clone());
                }
                DiscordEvent::RelationshipUpserted { relationship, user } => {
                    state
                        .relationships
                        .insert(relationship.user_id, (relationship.clone(), user.clone()));
                }
                DiscordEvent::RelationshipRemoved { user_id } => {
                    state.relationships.remove(user_id);
                }
                _ => {}
            }
        }
    }
}

#[derive(Debug)]
struct Driver {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

/// Account-owner supplied experimental connection with typed access gates.
#[derive(Debug)]
pub struct UserSessionBackend {
    shared: Arc<Shared>,
    driver: tokio::sync::Mutex<Option<Driver>>,
    lifecycle: tokio::sync::Mutex<()>,
}

fn payload_error(_: common::TranslateError) -> BackendError {
    BackendError::Sdk("Discord returned an unsupported account payload".into())
}

impl UserSessionBackend {
    pub fn new(
        transport: Arc<dyn SessionTransport>,
        secrets: Arc<dyn SecretStore>,
        clock: SharedClock,
    ) -> Self {
        Self::with_access(transport, secrets, clock, SessionAccessMode::ReadOnly)
    }
    pub fn with_access(
        transport: Arc<dyn SessionTransport>,
        secrets: Arc<dyn SecretStore>,
        clock: SharedClock,
        access: SessionAccessMode,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                access,
                transport,
                secrets,
                credential: Mutex::new(None),
                pinned_account: Mutex::new(None),
                metadata: Mutex::new(Metadata::default()),
                sink: Mutex::new(None),
                generation: Arc::new(AtomicU64::new(0)),
                clock,
            }),
            driver: tokio::sync::Mutex::new(None),
            lifecycle: tokio::sync::Mutex::new(()),
        }
    }
    fn require_write(&self, capability: Capability) -> BackendResult<()> {
        if self.shared.access != SessionAccessMode::ReadWrite {
            return Err(BackendError::Unsupported { capability });
        }
        self.shared.token().map(|_| ())
    }
    async fn confirm(&self, event: DiscordEvent) -> BackendResult<()> {
        self.confirm_at(event, self.shared.generation.load(Ordering::Acquire))
            .await
    }
    async fn confirm_at(&self, event: DiscordEvent, generation: u64) -> BackendResult<()> {
        let sink = self
            .shared
            .sink
            .lock()
            .map_err(|_| BackendError::Offline)?
            .clone()
            .ok_or(BackendError::NotConnected)?;
        let sink = sink.with_session_guard(self.shared.generation.clone(), generation);
        tokio::time::timeout(Duration::from_secs(5), sink.send_committed(SourceEnvelope::new(DiscordSource::UserSession, self.shared.clock.now(), event)))
            .await.map_err(|_| BackendError::DeliveryUncertain("Discord accepted the write, but local confirmation timed out; refresh before retrying".into()))?
            .map_err(|_| BackendError::DeliveryUncertain("Discord accepted the write, but local confirmation failed; refresh before retrying".into()))?;
        Ok(())
    }
    async fn target_channel(&self, target: &MessageTarget) -> BackendResult<ConversationId> {
        match target {
            MessageTarget::Conversation { conversation_id } => Ok(*conversation_id),
            MessageTarget::User { user_id } => {
                let raw = self
                    .shared
                    .call(RestRequest {
                        method: Method::Post,
                        path: "/users/@me/channels".into(),
                        route: "POST /users/@me/channels",
                        body: Some(json!({"recipient_id": user_id.to_string()})),
                    })
                    .await?;
                let conversation = translate::conversation(&raw).map_err(payload_error)?;
                if conversation.recipient_id != Some(*user_id) {
                    return Err(BackendError::Sdk("unexpected DM recipient".into()));
                }
                let id = conversation.id;
                self.shared.remember(&DiscordEvent::ConversationUpserted {
                    conversation: conversation.clone(),
                });
                self.confirm(DiscordEvent::ConversationUpserted { conversation })
                    .await?;
                Ok(id)
            }
        }
    }
    async fn send(
        &self,
        target: &MessageTarget,
        content: &str,
        reply: Option<MessageId>,
        operation_nonce: Option<&str>,
    ) -> BackendResult<Message> {
        let _lifecycle = self.lifecycle.lock().await;
        self.require_write(if reply.is_some() {
            Capability::Replies
        } else {
            Capability::DmSend
        })?;
        let channel = self.target_channel(target).await?;
        let mut req = match reply {
            Some(id) => rest::create_reply(ChannelId(channel.get()), content, id),
            None => rest::create_message(ChannelId(channel.get()), content),
        };
        // A single transmission: never automatically retry an ambiguous send.
        let mut bytes = [0u8; 8];
        getrandom::fill(&mut bytes).map_err(|_| BackendError::Offline)?;
        let generated = u64::from_le_bytes(bytes).to_string();
        let nonce = operation_nonce.unwrap_or(&generated);
        if let Some(body) = req.body.as_mut() {
            body["nonce"] = json!(nonce);
            body["enforce_nonce"] = json!(true);
        }
        let raw = self.shared.call(req).await?;
        let message = common::message(&raw).map_err(|_| {
            BackendError::DeliveryUncertain(
                "Discord returned an unexpected write confirmation; refresh before retrying".into(),
            )
        })?;
        self.verify_own_message(&message, channel).map_err(|_| {
            BackendError::DeliveryUncertain(
                "Discord returned a mismatched write confirmation; refresh before retrying".into(),
            )
        })?;
        self.confirm(DiscordEvent::MessageWriteObserved {
            message: message.clone(),
            nonce: nonce.to_owned(),
            imported: false,
        })
        .await?;
        Ok(message)
    }
    fn verify_own_message(
        &self,
        message: &Message,
        conversation: ConversationId,
    ) -> BackendResult<()> {
        let account = *self
            .shared
            .pinned_account
            .lock()
            .map_err(|_| BackendError::Offline)?;
        if account != Some(message.author_id) || conversation != message.conversation_id {
            return Err(BackendError::PermissionDenied {
                what: "only this account's messages in the selected channel may be changed".into(),
            });
        }
        Ok(())
    }
    async fn own_message(&self, conversation: ConversationId, id: MessageId) -> BackendResult<()> {
        let raw = self
            .shared
            .call(RestRequest {
                method: Method::Get,
                path: format!("/channels/{conversation}/messages/{id}"),
                route: "GET /channels/{channel.id}/messages/{message.id}",
                body: None,
            })
            .await?;
        let message = common::message(&raw).map_err(payload_error)?;
        self.verify_own_message(&message, conversation)?;
        if message.id != id {
            return Err(BackendError::Sdk("unexpected message identity".into()));
        }
        Ok(())
    }
    async fn stop(&self) {
        // Reads capture the credential and epoch under this same mutex.
        // No request can observe a new epoch paired with an old credential.
        if let Ok(mut credential) = self.shared.credential.lock() {
            *credential = None;
            self.shared.generation.fetch_add(1, Ordering::AcqRel);
        }
        if let Some(mut driver) = self.driver.lock().await.take() {
            driver.cancel.cancel();
            if tokio::time::timeout(Duration::from_secs(3), &mut driver.handle)
                .await
                .is_err()
            {
                driver.handle.abort();
                let _ = driver.handle.await;
            }
        }
    }
    async fn start(&self) -> BackendResult<()> {
        let Some(sink) = self
            .shared
            .sink
            .lock()
            .map_err(|_| BackendError::Offline)?
            .clone()
        else {
            return Err(BackendError::NotConnected);
        };
        let mut driver = self.driver.lock().await;
        if driver.as_ref().is_some_and(|d| !d.handle.is_finished()) {
            return Ok(());
        }
        let generation = self.shared.generation.load(Ordering::Acquire);
        let sink = sink.with_session_guard(self.shared.generation.clone(), generation);
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(drive(self.shared.clone(), sink, cancel.clone()));
        *driver = Some(Driver { cancel, handle });
        Ok(())
    }
}

async fn drive(shared: Arc<Shared>, sink: IngestSender, cancel: CancellationToken) {
    let Ok(token) = shared.token() else {
        return;
    };
    let mut session = GatewaySession::new_user_session(token, std::env::consts::OS.to_owned());
    let (refresh_tx, mut refresh_rx) =
        tokio::sync::mpsc::channel::<(ConversationId, MessageId)>(64);
    let worker_shared = shared.clone();
    let worker_sink = sink.clone();
    let worker_cancel = cancel.clone();
    let mut refresh_worker = tokio::spawn(async move {
        loop {
            let job =
                tokio::select! {_=worker_cancel.cancelled()=>break,job=refresh_rx.recv()=>job};
            let Some((conversation_id, message_id)) = job else {
                break;
            };
            let request = RestRequest {
                method: Method::Get,
                path: format!("/channels/{conversation_id}/messages/{message_id}"),
                body: None,
                route: "GET /channels/{channel.id}/messages/{message.id}",
            };
            let result = tokio::select! {_=worker_cancel.cancelled()=>break,r=worker_shared.call(request)=>r};
            if let Ok(raw) = result {
                if let Ok(message) = common::message(&raw) {
                    let env = SourceEnvelope::new(
                        DiscordSource::UserSession,
                        worker_shared.clock.now(),
                        DiscordEvent::MessageUpdated { message },
                    );
                    tokio::select! {_=worker_cancel.cancelled()=>break,_=worker_sink.send_committed(env)=>{}};
                }
            }
        }
    });
    let mut url = "wss://gateway.discord.gg/?v=10&encoding=json".to_owned();
    let mut backoff = Duration::from_secs(1);
    'connections: loop {
        if cancel.is_cancelled() {
            break;
        }
        let state = if session.session_id().is_some() {
            SessionState::Reconnecting
        } else {
            SessionState::Connecting
        };
        let _ = sink.try_send(SourceEnvelope::new(
            DiscordSource::UserSession,
            shared.clock.now(),
            DiscordEvent::SessionChanged { state },
        ));
        let connection = tokio::select! {_=cancel.cancelled()=>break,r=tokio::time::timeout(Duration::from_secs(20),shared.transport.connect(&url))=>r.unwrap_or(Err(BackendError::Offline))};
        let mut socket = match connection {
            Ok(s) => s,
            Err(_) => {
                tokio::select! {_=cancel.cancelled()=>break,_=tokio::time::sleep(backoff)=>{}};
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        session.on_reconnected();
        let handshake_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let delay = session
                .next_deadline()
                .map(|t| Duration::from_millis(t.since(shared.clock.now()).as_millis().max(1)))
                .unwrap_or(Duration::from_secs(30));
            let outputs = tokio::select! {
                _=cancel.cancelled()=>{let _=tokio::time::timeout(Duration::from_secs(1),socket.close()).await;break 'connections;},
                _=tokio::time::sleep_until(handshake_deadline), if session.state()!=crate::bot::gateway::State::Ready => session.on_close(None),
                frame=socket.recv()=>match frame {SocketEvent::Text(t)=>session.on_frame(&t,shared.clock.now(),0.5),SocketEvent::Closed(code)=>session.on_close(code)},
                _=tokio::time::sleep(delay)=>session.on_tick(shared.clock.now()),
            };
            for output in outputs {
                match output {
                    Output::Send(text) => {
                        let sent = tokio::select! {_=cancel.cancelled()=>break 'connections,r=tokio::time::timeout(Duration::from_secs(5),socket.send(text))=>matches!(r,Ok(Ok(())))};
                        if !sent {
                            let _ = session.on_close(None);
                            let _ =
                                tokio::time::timeout(Duration::from_secs(1), socket.close()).await;
                            tokio::select! {_=cancel.cancelled()=>break 'connections,_=tokio::time::sleep(backoff)=>{}};
                            backoff = (backoff * 2).min(Duration::from_secs(60));
                            continue 'connections;
                        }
                    }
                    Output::Dispatch { event, data } => {
                        if event == "MESSAGE_UPDATE" {
                            let ids = data
                                .get("channel_id")
                                .and_then(Value::as_str)
                                .and_then(|v| v.parse().ok())
                                .zip(
                                    data.get("id")
                                        .and_then(Value::as_str)
                                        .and_then(|v| v.parse().ok()),
                                );
                            if let Some(ids) = ids {
                                let _ = refresh_tx.try_send(ids);
                            }
                        }
                        if let Ok(events) = translate::dispatch(&event, &data) {
                            for event in events {
                                if let DiscordEvent::CurrentUser { user } = &event {
                                    if shared.check_account(user).is_err() {
                                        let _=sink.send(SourceEnvelope::new(DiscordSource::UserSession,shared.clock.now(),DiscordEvent::SessionChanged{state:SessionState::Error{message:"connected account does not match this database".into()}})).await;
                                        break 'connections;
                                    }
                                }
                                shared.remember(&event);
                                let reliable = matches!(
                                    event,
                                    DiscordEvent::MessageCreated { .. }
                                        | DiscordEvent::MessageWriteObserved { .. }
                                        | DiscordEvent::MessageUpdated { .. }
                                        | DiscordEvent::MessageDeleted { .. }
                                        | DiscordEvent::CurrentUser { .. }
                                        | DiscordEvent::SessionChanged { .. }
                                );
                                let envelope = SourceEnvelope::new(
                                    DiscordSource::UserSession,
                                    shared.clock.now(),
                                    event,
                                );
                                if reliable {
                                    let committed = tokio::select! {_=cancel.cancelled()=>break 'connections,r=tokio::time::timeout(Duration::from_secs(5),sink.send_committed(envelope))=>matches!(r,Ok(Ok(_)))};
                                    if !committed {
                                        tracing::error!("account live event could not commit; session stopped for reconciliation");
                                        let _=sink.try_send(SourceEnvelope::new(DiscordSource::UserSession,shared.clock.now(),DiscordEvent::SessionChanged{state:SessionState::Error{message:"live event could not commit; reconnect to reconcile".into()}}));
                                        break 'connections;
                                    }
                                } else {
                                    let _ = sink.try_send(envelope);
                                }
                            }
                        } else {
                            let _ = sink.try_send(SourceEnvelope::new(
                                DiscordSource::UserSession,
                                shared.clock.now(),
                                DiscordEvent::SessionChanged {
                                    state: SessionState::Error {
                                        message:
                                            "Discord account payload changed; reconnect required"
                                                .into(),
                                    },
                                },
                            ));
                            if event == "READY" {
                                break 'connections;
                            }
                        }
                    }
                    Output::Ready | Output::Resumed => {
                        backoff = Duration::from_secs(1);
                        if matches!(output, Output::Resumed) {
                            let _ = sink.try_send(SourceEnvelope::new(
                                DiscordSource::UserSession,
                                shared.clock.now(),
                                DiscordEvent::SessionChanged {
                                    state: SessionState::Ready,
                                },
                            ));
                        }
                    }
                    Output::Reconnect { url: resume, .. } => {
                        if let Some(resume) = resume.filter(|u| {
                            u.starts_with("wss://gateway.discord.gg/")
                                || u == "wss://gateway.discord.gg"
                        }) {
                            url = if resume.contains('?') {
                                resume
                            } else {
                                format!("{}/?v=10&encoding=json", resume.trim_end_matches('/'))
                            };
                        }
                        let _ = tokio::time::timeout(Duration::from_secs(1), socket.close()).await;
                        tokio::select! {_=cancel.cancelled()=>break 'connections,_=tokio::time::sleep(backoff)=>{}};
                        backoff = (backoff * 2).min(Duration::from_secs(60));
                        continue 'connections;
                    }
                    Output::Fatal(message) => {
                        let _ = sink.try_send(SourceEnvelope::new(
                            DiscordSource::UserSession,
                            shared.clock.now(),
                            DiscordEvent::SessionChanged {
                                state: SessionState::Error { message },
                            },
                        ));
                        let _ = tokio::time::timeout(Duration::from_secs(1), socket.close()).await;
                        break 'connections;
                    }
                }
            }
        }
    }
    cancel.cancel();
    if tokio::time::timeout(Duration::from_secs(1), &mut refresh_worker)
        .await
        .is_err()
    {
        refresh_worker.abort();
        let _ = refresh_worker.await;
    }
}

#[async_trait]
impl SocialBackend for UserSessionBackend {
    fn source(&self) -> DiscordSource {
        DiscordSource::UserSession
    }
    fn mode(&self) -> BackendMode {
        BackendMode::UserSession
    }
    fn session_generation(&self) -> Option<(Arc<AtomicU64>, u64)> {
        Some((
            self.shared.generation.clone(),
            self.shared.generation.load(Ordering::Acquire),
        ))
    }
    fn capabilities(&self) -> CapabilitySet {
        let partial=SupportLevel::Partial{note:"Read-only experimental account access; visibility depends on the account and current protocol".into()};
        let mut caps = CapabilitySet::default()
            .with(Capability::CurrentUser, partial.clone())
            .with(Capability::Friends, partial.clone())
            .with(Capability::DmList, partial.clone())
            .with(Capability::DmHistory, partial.clone())
            .with(Capability::GuildListing, partial.clone())
            .with(Capability::GuildChannels, partial.clone())
            .with(Capability::GuildMessages, partial);
        if self.shared.access == SessionAccessMode::ReadWrite && self.shared.token().is_ok() {
            for cap in [
                Capability::DmSend,
                Capability::DmEdit,
                Capability::DmDelete,
                Capability::Replies,
            ] {
                caps = caps.with(
                    cap,
                    SupportLevel::Partial {
                        note: "Experimental account writes; Discord permissions still apply".into(),
                    },
                );
            }
        }
        caps.dm_history = Some(HistoryCapability::Full);
        caps
    }
    fn bind_account(&self, account: UserId) -> BackendResult<()> {
        *self
            .shared
            .pinned_account
            .lock()
            .map_err(|_| BackendError::Offline)? = Some(account);
        Ok(())
    }
    async fn connect(&self, sink: IngestSender) -> BackendResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        *self.shared.sink.lock().map_err(|_| BackendError::Offline)? = Some(sink);
        let store = self.shared.secrets.clone();
        let token =
            tokio::task::spawn_blocking(move || store.try_get(SecretKey::DiscordUserSessionToken))
                .await
                .map_err(|_| BackendError::Offline)?
                .map_err(|_| {
                    BackendError::Authentication("OS credential storage is unavailable".into())
                })?;
        let Some(token) = token else {
            self.shared.emit(DiscordEvent::SessionChanged {
                state: SessionState::LoggedOut,
            });
            return Ok(());
        };
        // Verify the saved credential before any parallel hydration can read
        // account-scoped data. Failed validation leaves the UI repairable.
        let verified = match self
            .shared
            .transport
            .request(&token, &rest::current_user())
            .await
        {
            Ok(raw) => common::user(&raw).map_err(payload_error).and_then(|user| {
                self.shared.check_account(&user)?;
                Ok(user)
            }),
            Err(error) => Err(error),
        };
        let user = match verified {
            Ok(user) => user,
            Err(_) => {
                self.shared.emit(DiscordEvent::SessionChanged {
                    state: SessionState::Error {
                        message:
                            "saved account credential could not be verified; reconnect in Settings"
                                .into(),
                    },
                });
                return Ok(());
            }
        };
        *self
            .shared
            .pinned_account
            .lock()
            .map_err(|_| BackendError::Offline)? = Some(user.id);
        *self
            .shared
            .credential
            .lock()
            .map_err(|_| BackendError::Offline)? = Some(token);
        self.shared
            .remember(&DiscordEvent::CurrentUser { user: user.clone() });
        self.shared.emit(DiscordEvent::CurrentUser { user });
        self.start().await
    }
    async fn disconnect(&self) -> BackendResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop().await;
        Ok(())
    }
    async fn begin_sign_in(&self) -> BackendResult<AuthStep> {
        Ok(AuthStep::SessionCredential {
            label: "Account-owner supplied session credential".into(),
        })
    }
    async fn authenticate_session(&self, credential: Secret<String>) -> BackendResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        if credential.expose_secret().trim().is_empty() {
            return Err(BackendError::Authentication(
                "enter an account session credential".into(),
            ));
        }
        let raw = self
            .shared
            .transport
            .request(&credential, &rest::current_user())
            .await?;
        let user = common::user(&raw).map_err(payload_error)?;
        self.shared.check_account(&user)?;
        self.stop().await;
        let store = self.shared.secrets.clone();
        let persisted = Secret::new(credential.expose_secret().clone());
        let saved = tokio::task::spawn_blocking(move || {
            store.try_set(SecretKey::DiscordUserSessionToken, persisted)
        })
        .await;
        if !matches!(saved, Ok(Ok(()))) {
            *self
                .shared
                .credential
                .lock()
                .map_err(|_| BackendError::Offline)? = None;
            self.shared.emit(DiscordEvent::SessionChanged {
                state: SessionState::Error {
                    message: "could not save the credential in OS storage; reconnect required"
                        .into(),
                },
            });
            return Err(BackendError::Authentication(
                "could not save the credential in OS storage".into(),
            ));
        }
        *self
            .shared
            .credential
            .lock()
            .map_err(|_| BackendError::Offline)? = Some(credential);
        *self
            .shared
            .pinned_account
            .lock()
            .map_err(|_| BackendError::Offline)? = Some(user.id);
        *self
            .shared
            .metadata
            .lock()
            .map_err(|_| BackendError::Offline)? = Metadata::default();
        self.shared
            .remember(&DiscordEvent::CurrentUser { user: user.clone() });
        self.shared.emit(DiscordEvent::CurrentUser { user });
        self.start().await
    }
    async fn sign_out(&self) -> BackendResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop().await;
        *self
            .shared
            .credential
            .lock()
            .map_err(|_| BackendError::Offline)? = None;
        *self
            .shared
            .metadata
            .lock()
            .map_err(|_| BackendError::Offline)? = Metadata::default();
        let store = self.shared.secrets.clone();
        let removed = tokio::task::spawn_blocking(move || {
            store.try_remove(SecretKey::DiscordUserSessionToken)
        })
        .await;
        if !matches!(removed, Ok(Ok(()))) {
            let message = "Credential removal failed: the connection is stopped, but the saved credential may remain. Retry Log out before restarting.";
            self.shared.emit(DiscordEvent::SessionChanged {
                state: SessionState::Error {
                    message: message.into(),
                },
            });
            return Err(BackendError::Authentication(message.into()));
        }
        self.shared.emit(DiscordEvent::SessionChanged {
            state: SessionState::LoggedOut,
        });
        Ok(())
    }
    async fn current_user(&self) -> BackendResult<User> {
        let raw = self.shared.call(rest::current_user()).await?;
        let user = common::user(&raw).map_err(payload_error)?;
        self.shared.check_account(&user)?;
        Ok(user)
    }
    async fn user(&self, user_id: UserId) -> BackendResult<User> {
        let raw = self
            .shared
            .call(RestRequest {
                method: Method::Get,
                path: format!("/users/{user_id}"),
                body: None,
                route: "GET /users/{user.id}",
            })
            .await?;
        common::user(&raw).map_err(payload_error)
    }
    async fn relationships(&self) -> BackendResult<Vec<(Relationship, Option<User>)>> {
        let raw = self
            .shared
            .call(RestRequest {
                method: Method::Get,
                path: "/users/@me/relationships".into(),
                body: None,
                route: "GET /users/@me/relationships",
            })
            .await?;
        raw.as_array()
            .ok_or_else(|| BackendError::Sdk("unexpected relationship list".into()))?
            .iter()
            .map(|r| translate::relationship(r).map_err(payload_error))
            .collect()
    }
    async fn guilds(&self) -> BackendResult<Vec<Guild>> {
        let mut out = Vec::new();
        let mut after = None;
        loop {
            let mut req = rest::current_user_guilds();
            req.path = format!(
                "/users/@me/guilds?limit=200{}",
                after
                    .map(|id: GuildId| format!("&after={id}"))
                    .unwrap_or_default()
            );
            let raw = self.shared.call(req).await?;
            let page = rest::parse_guilds(&raw).map_err(payload_error)?;
            let more = page.len() == 200;
            let next = page.iter().map(|g| g.id).max();
            if more && (next.is_none() || next == after) {
                return Err(BackendError::Sdk("guild discovery did not advance".into()));
            }
            out.extend(page);
            if out.len() > 10_000 {
                return Err(BackendError::Sdk(
                    "guild discovery exceeds the limit".into(),
                ));
            }
            if !more {
                break;
            }
            after = next;
        }
        Ok(out)
    }
    async fn guild_channels(&self, guild_id: GuildId) -> BackendResult<Vec<Channel>> {
        let raw = self.shared.call(rest::guild_channels(guild_id)).await?;
        let mut channels = rest::parse_channels(&raw, guild_id).map_err(payload_error)?;
        if self.shared.access == SessionAccessMode::ReadOnly {
            for c in &mut channels {
                c.capabilities.0 &= !ChannelCapabilities::WRITABLE.0;
            }
        }
        for channel in &channels {
            self.shared.remember(&DiscordEvent::ChannelUpserted {
                channel: channel.clone(),
            });
            if let Some(conversation) = common::channel_conversation(channel) {
                self.shared
                    .remember(&DiscordEvent::ConversationUpserted { conversation });
            }
        }
        Ok(channels)
    }
    async fn conversations(&self) -> BackendResult<Vec<Conversation>> {
        let raw = self
            .shared
            .call(RestRequest {
                method: Method::Get,
                path: "/users/@me/channels".into(),
                body: None,
                route: "GET /users/@me/channels",
            })
            .await?;
        let mut out = raw
            .as_array()
            .ok_or_else(|| BackendError::Sdk("unexpected private channel list".into()))?
            .iter()
            .map(|r| translate::conversation(r).map_err(payload_error))
            .collect::<BackendResult<Vec<_>>>()?;
        out.extend(
            self.shared
                .metadata
                .lock()
                .map_err(|_| BackendError::Offline)?
                .conversations
                .values()
                .filter(|c| c.kind == ConversationKind::GuildChannel)
                .cloned(),
        );
        Ok(out)
    }
    async fn messages(
        &self,
        conversation_id: ConversationId,
        limit: u32,
    ) -> BackendResult<Vec<Message>> {
        Ok(self
            .history_page(&HistoryPageRequest::latest(conversation_id, limit))
            .await?
            .messages)
    }
    async fn send_message(&self, target: &MessageTarget, content: &str) -> BackendResult<Message> {
        self.send(target, content, None, None).await
    }
    async fn send_reply(
        &self,
        target: &MessageTarget,
        content: &str,
        reply_to: MessageId,
    ) -> BackendResult<Message> {
        self.send(target, content, Some(reply_to), None).await
    }
    async fn send_message_operation(
        &self,
        target: &MessageTarget,
        content: &str,
        reply: Option<MessageId>,
        nonce: &str,
    ) -> BackendResult<Message> {
        self.send(target, content, reply, Some(nonce)).await
    }
    async fn edit_message_in(
        &self,
        conversation: ConversationId,
        id: MessageId,
        content: &str,
    ) -> BackendResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.require_write(Capability::DmEdit)?;
        self.own_message(conversation, id).await?;
        let raw = self
            .shared
            .call(rest::edit_message(
                ChannelId(conversation.get()),
                id,
                content,
            ))
            .await?;
        let message = common::message(&raw).map_err(payload_error)?;
        self.verify_own_message(&message, conversation)?;
        if message.id != id {
            return Err(BackendError::Sdk(
                "unexpected edited message identity".into(),
            ));
        }
        self.confirm(DiscordEvent::MessageUpdated { message }).await
    }
    async fn delete_message_in(
        &self,
        conversation: ConversationId,
        id: MessageId,
    ) -> BackendResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.require_write(Capability::DmDelete)?;
        self.own_message(conversation, id).await?;
        self.shared
            .call(rest::delete_message(ChannelId(conversation.get()), id))
            .await?;
        self.confirm(DiscordEvent::MessageDeleted {
            message_id: id,
            conversation_id: conversation,
        })
        .await
    }
    async fn history_page(&self, req: &HistoryPageRequest) -> BackendResult<HistoryPage> {
        let generation = self.shared.generation.load(Ordering::Acquire);
        let raw = self
            .shared
            .call(rest::channel_messages_page(
                ChannelId(req.conversation_id.get()),
                req.effective_limit(),
                req.before,
                req.after,
            ))
            .await?;
        let mut messages = rest::parse_messages(&raw).map_err(payload_error)?;
        if let Some(rows) = raw.as_array() {
            for row in rows {
                if let Some(nonce) = translate::message_nonce(row) {
                    let message = common::message(row).map_err(payload_error)?;
                    if message.conversation_id != req.conversation_id {
                        return Err(BackendError::Sdk("history receipt channel mismatch".into()));
                    }
                    self.confirm_at(
                        DiscordEvent::MessageWriteObserved {
                            message,
                            nonce,
                            imported: true,
                        },
                        generation,
                    )
                    .await?;
                }
            }
        }
        let full = messages.len() == req.effective_limit() as usize;
        let n = messages.len();
        if messages
            .iter()
            .any(|m| m.conversation_id != req.conversation_id)
        {
            return Err(BackendError::Sdk(
                "history contains an unexpected channel".into(),
            ));
        }
        messages.retain(|m| {
            req.before.is_none_or(|id| m.id < id) && req.after.is_none_or(|id| m.id > id)
        });
        Ok(HistoryPage {
            has_more: full && messages.len() == n,
            messages,
        })
    }
}
