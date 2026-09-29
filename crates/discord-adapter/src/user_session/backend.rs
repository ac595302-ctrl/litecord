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
#[cfg(feature = "discord-user-session")]
use litecord_core::ports::AccountLoginStep;
use litecord_core::ports::{
    BackendError, BackendResult, HistoryPage, HistoryPageRequest, SocialBackend,
};
use litecord_core::secrets::{Secret, SecretKey, SecretStore};
use litecord_types::actions::{MessageTarget, PresenceDraft, RelationshipAction};
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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    presences: BTreeMap<UserId, Presence>,
}

#[derive(Debug)]
struct GatewayWrite {
    payload: String,
    generation: u64,
    response: tokio::sync::oneshot::Sender<BackendResult<()>>,
    expires: tokio::time::Instant,
}

struct Shared {
    access: SessionAccessMode,
    writes_enabled: AtomicBool,
    gateway_writes: Mutex<Option<tokio::sync::mpsc::Sender<GatewayWrite>>>,
    last_presence_write: Mutex<Option<std::time::Instant>>,
    transport: Arc<dyn SessionTransport>,
    secrets: Arc<dyn SecretStore>,
    credential: Mutex<Option<Secret<String>>>,
    pending_mfa_ticket: Mutex<Option<Secret<String>>>,
    pinned_account: Mutex<Option<UserId>>,
    metadata: Mutex<Metadata>,
    sink: Mutex<Option<IngestSender>>,
    generation: Arc<AtomicU64>,
    session_cancel: Mutex<CancellationToken>,
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
    fn access(&self) -> SessionAccessMode {
        if self.access == SessionAccessMode::ReadWrite
            && self.writes_enabled.load(Ordering::Acquire)
        {
            SessionAccessMode::ReadWrite
        } else {
            SessionAccessMode::ReadOnly
        }
    }
    fn token(&self) -> BackendResult<Secret<String>> {
        self.credential
            .lock()
            .map_err(|_| BackendError::Offline)?
            .as_ref()
            .map(|s| Secret::new(s.expose_secret().clone()))
            .ok_or(BackendError::NotConnected)
    }
    async fn call(&self, req: RestRequest) -> BackendResult<Value> {
        if !super::allows_request(self.access(), &req) {
            return Err(BackendError::Unsupported {
                capability: Capability::DmSend,
            });
        }
        let (generation, token) = {
            let credential = self.credential.lock().map_err(|_| BackendError::Offline)?;
            let token = credential
                .as_ref()
                .map(|s| Secret::new(s.expose_secret().clone()))
                .ok_or(BackendError::NotConnected)?;
            (self.generation.load(Ordering::Acquire), token)
        };
        let value = self.transport.request(&token, &req).await.map_err(|e| {
            if matches!(e, BackendError::Authentication(_)) {
                self.suspend(generation, "Account authentication was rejected; connection stopped. Review the account in Discord before reconnecting.");
            }
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
    /// Stop future reads and Gateway work for this epoch. An old response must
    /// never invalidate a newer, explicitly established session.
    fn suspend(&self, generation: u64, message: &str) {
        let Ok(mut credential) = self.credential.lock() else {
            return;
        };
        if self.generation.load(Ordering::Acquire) != generation {
            return;
        }
        *credential = None;
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.writes_enabled.store(false, Ordering::Release);
        if let Ok(cancel) = self.session_cancel.lock() {
            cancel.cancel();
        }
        // Emit on the unguarded sink after advancing the epoch, so the error is
        // visible while late events from the stopped session are discarded.
        self.emit(DiscordEvent::SessionChanged {
            state: SessionState::Error {
                message: message.into(),
            },
        });
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
                DiscordEvent::PresenceChanged { user_id, presence } => {
                    state.presences.insert(*user_id, presence.clone());
                }
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
/// How long a connection must stay ready before its reconnect budget resets.
const STABLE_CONNECTION: Duration = Duration::from_secs(120);

/// Waits between attempts to verify the saved credential at launch.
const VERIFY_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];

#[derive(Debug)]
pub struct UserSessionBackend {
    shared: Arc<Shared>,
    driver: tokio::sync::Mutex<Option<Driver>>,
    lifecycle: tokio::sync::Mutex<()>,
}

fn payload_error(_: common::TranslateError) -> BackendError {
    BackendError::Sdk("Discord returned an unsupported account payload".into())
}

/// Discord's account-password exchange is not part of its public OAuth API.
/// Keep this isolated from the ordinary REST transport and never log the
/// request, response, password, MFA ticket, or returned session token.
#[cfg(feature = "discord-user-session")]
async fn password_auth_request(path: &str, body: Value) -> BackendResult<Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| BackendError::Offline)?;
    let response = client
        .post(format!("https://discord.com/api/v9/auth/{path}"))
        .json(&body)
        .send()
        .await
        .map_err(|_| BackendError::Offline)?;
    let status = response.status();
    let payload: Value = response.json().await.map_err(|_| {
        BackendError::Authentication("Discord returned an unreadable login response".into())
    })?;
    if payload.get("captcha_key").is_some() || payload.get("captcha_sitekey").is_some() {
        return Err(BackendError::Authentication(
            "Discord requires a CAPTCHA for this login; Litecord cannot complete that challenge"
                .into(),
        ));
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(BackendError::Authentication(
            "Discord rate limited this login; wait before trying again".into(),
        ));
    }
    if !status.is_success() {
        return Err(BackendError::Authentication(if path == "login" {
            "Discord rejected the email or password, or requires an unsupported challenge".into()
        } else {
            "Discord rejected the authentication code".into()
        }));
    }
    Ok(payload)
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
                writes_enabled: AtomicBool::new(access == SessionAccessMode::ReadWrite),
                gateway_writes: Mutex::new(None),
                last_presence_write: Mutex::new(None),
                transport,
                secrets,
                credential: Mutex::new(None),
                pending_mfa_ticket: Mutex::new(None),
                pinned_account: Mutex::new(None),
                metadata: Mutex::new(Metadata::default()),
                sink: Mutex::new(None),
                generation: Arc::new(AtomicU64::new(0)),
                session_cancel: Mutex::new(CancellationToken::new()),
                clock,
            }),
            driver: tokio::sync::Mutex::new(None),
            lifecycle: tokio::sync::Mutex::new(()),
        }
    }
    fn require_write(&self, capability: Capability) -> BackendResult<()> {
        if self.shared.access() != SessionAccessMode::ReadWrite {
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
        *self
            .shared
            .session_cancel
            .lock()
            .map_err(|_| BackendError::Offline)? = cancel.clone();
        let (writes, rx) = tokio::sync::mpsc::channel(8);
        *self
            .shared
            .gateway_writes
            .lock()
            .map_err(|_| BackendError::Offline)? = Some(writes);
        let handle = tokio::spawn(drive(
            self.shared.clone(),
            sink,
            cancel.clone(),
            rx,
            generation,
        ));
        *driver = Some(Driver { cancel, handle });
        Ok(())
    }
}

async fn drive(
    shared: Arc<Shared>,
    sink: IngestSender,
    cancel: CancellationToken,
    mut writes: tokio::sync::mpsc::Receiver<GatewayWrite>,
    generation: u64,
) {
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
    // Six connection attempts in a row, then stop. A connection that stayed
    // ready for a while restores the budget: Discord routinely asks clients
    // to reconnect every few hours, and counting those made a long-running
    // app give up for good. Rapid flapping still stops.
    let mut attempts = 0;
    let mut ready_since: Option<tokio::time::Instant> = None;
    'connections: loop {
        if cancel.is_cancelled() {
            break;
        }
        if ready_since
            .take()
            .is_some_and(|t| t.elapsed() >= STABLE_CONNECTION)
        {
            attempts = 0;
            backoff = Duration::from_secs(1);
        }
        if attempts >= 6 {
            shared.suspend(generation, "Automatic reconnect limit reached; connection stopped. Reconnect in Settings when ready.");
            break;
        }
        attempts += 1;
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
            Err(BackendError::Authentication(_)) => {
                shared.suspend(generation, "Account authentication was rejected; connection stopped. Review the account in Discord before reconnecting.");
                break;
            }
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
                Some(command)=writes.recv()=>{
                    let result=if command.response.is_closed() || command.expires <= tokio::time::Instant::now() || shared.access()!=SessionAccessMode::ReadWrite || command.generation != shared.generation.load(Ordering::Acquire) || session.state()!=crate::bot::gateway::State::Ready {
                        Err(BackendError::NotConnected)
                    } else {
                        tokio::time::timeout(Duration::from_secs(5),socket.send(command.payload)).await
                            .map_err(|_|BackendError::DeliveryUncertain("Gateway submission timed out".into()))
                            .and_then(|r|r.map_err(|_|BackendError::DeliveryUncertain("Gateway submission outcome unknown".into())))
                    };
                    let _=command.response.send(result);
                    Vec::new()
                },
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
                        let translated = translate::dispatch(&event, &data);
                        if let Err(error) = &translated {
                            tracing::warn!(event = %event, error = %error, "skipped an account event Litecord could not read");
                        }
                        if let Ok(events) = translated {
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
                                    let committed = tokio::select! {_=cancel.cancelled()=>break 'connections,r=tokio::time::timeout(Duration::from_secs(15),sink.send_committed(envelope))=>matches!(r,Ok(Ok(_)))};
                                    if !committed {
                                        // Resuming would skip the lost event. A fresh
                                        // session sends a full READY, which reconciles.
                                        tracing::error!("account live event could not commit; reconnecting with a fresh session");
                                        session.invalidate_session();
                                        let _ = tokio::time::timeout(
                                            Duration::from_secs(1),
                                            socket.close(),
                                        )
                                        .await;
                                        tokio::select! {_=cancel.cancelled()=>break 'connections,_=tokio::time::sleep(backoff)=>{}};
                                        backoff = (backoff * 2).min(Duration::from_secs(60));
                                        continue 'connections;
                                    }
                                } else {
                                    let _ = sink.try_send(envelope);
                                }
                            }
                        } else if event == "READY" {
                            // Without READY there is no account snapshot to show.
                            // Other unreadable events are skipped (logged above):
                            // flagging the whole session as failed for one odd
                            // presence or message made the app look signed out.
                            let _ = sink.try_send(SourceEnvelope::new(
                                DiscordSource::UserSession,
                                shared.clock.now(),
                                DiscordEvent::SessionChanged {
                                    state: SessionState::Error {
                                        message:
                                            "Discord sent account data Litecord could not read; reconnect in Settings"
                                                .into(),
                                    },
                                },
                            ));
                            break 'connections;
                        }
                    }
                    Output::Ready | Output::Resumed => {
                        backoff = Duration::from_secs(1);
                        ready_since = Some(tokio::time::Instant::now());
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
                        shared.suspend(generation, &message);
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
        let partial=SupportLevel::Partial{note:"Experimental account access; visibility depends on the account and current protocol".into()};
        let mut caps = CapabilitySet::default()
            .with(Capability::CurrentUser, partial.clone())
            .with(Capability::Friends, partial.clone())
            .with(Capability::Presence, partial.clone())
            .with(Capability::DmList, partial.clone())
            .with(Capability::DmHistory, partial.clone())
            .with(Capability::GuildListing, partial.clone())
            .with(Capability::GuildChannels, partial.clone())
            .with(Capability::GuildMessages, partial);
        if self.shared.access() == SessionAccessMode::ReadWrite && self.shared.token().is_ok() {
            for cap in [
                Capability::DmSend,
                Capability::DmEdit,
                Capability::DmDelete,
                Capability::Replies,
                Capability::FriendRequests,
                Capability::Blocking,
                Capability::RichPresence,
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
    fn session_access(&self) -> Option<SessionAccessMode> {
        Some(self.shared.access())
    }
    fn can_enable_session_writes(&self) -> bool {
        self.shared.access == SessionAccessMode::ReadWrite
    }
    async fn set_session_access(&self, access: SessionAccessMode) -> BackendResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        if access == SessionAccessMode::ReadWrite && !self.can_enable_session_writes() {
            return Err(BackendError::PermissionDenied {
                what: "read-only access is required by configuration".into(),
            });
        }
        self.shared
            .writes_enabled
            .store(access == SessionAccessMode::ReadWrite, Ordering::Release);
        Ok(())
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
        // A network blip at launch (Wi-Fi still joining, a Discord 5xx, a
        // rate limit) is retried; only a rejected credential asks the user
        // to sign in again.
        self.shared.emit(DiscordEvent::SessionChanged {
            state: SessionState::Connecting,
        });
        let mut delays = VERIFY_RETRY_DELAYS.iter();
        let verified = loop {
            let attempt = match self
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
            match (attempt, delays.next()) {
                (Err(error), Some(delay)) if error.is_retryable() => {
                    let wait = match &error {
                        BackendError::RateLimited { retry_after } => {
                            Duration::from_millis(retry_after.0).max(*delay)
                        }
                        _ => *delay,
                    };
                    tracing::info!(error = %error, ?wait, "could not verify the saved account; retrying");
                    tokio::time::sleep(wait.min(Duration::from_secs(30))).await;
                }
                (attempt, _) => break attempt,
            }
        };
        let user = match verified {
            Ok(user) => user,
            Err(error) => {
                let message = if error.is_retryable() {
                    "Couldn't reach Discord. Check your internet connection, then reconnect in Settings."
                } else if let BackendError::Authentication(reason) = &error {
                    reason.as_str()
                } else {
                    "saved account credential could not be verified; reconnect in Settings"
                };
                self.shared.emit(DiscordEvent::SessionChanged {
                    state: SessionState::Error {
                        message: message.into(),
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
        *self
            .shared
            .pending_mfa_ticket
            .lock()
            .map_err(|_| BackendError::Offline)? = None;
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
    #[cfg(feature = "discord-user-session")]
    async fn login_with_password(
        &self,
        login: Secret<String>,
        password: Secret<String>,
    ) -> BackendResult<AccountLoginStep> {
        *self
            .shared
            .pending_mfa_ticket
            .lock()
            .map_err(|_| BackendError::Offline)? = None;
        if login.expose_secret().trim().is_empty() || password.expose_secret().is_empty() {
            return Err(BackendError::Authentication(
                "enter your Discord email and password".into(),
            ));
        }
        let payload = password_auth_request(
            "login",
            json!({
                "login": login.expose_secret().trim(),
                "password": password.expose_secret(),
                "undelete": false,
            }),
        )
        .await?;
        if let Some(token) = payload.get("token").and_then(Value::as_str) {
            self.authenticate_session(Secret::new(token.to_owned()))
                .await?;
            return Ok(AccountLoginStep::Connected);
        }
        if payload.get("mfa").and_then(Value::as_bool) == Some(true) {
            if payload.get("totp").and_then(Value::as_bool) == Some(false) {
                return Err(BackendError::Authentication(
                    "This account requires an MFA method other than an authenticator code".into(),
                ));
            }
            let ticket = payload
                .get("ticket")
                .and_then(Value::as_str)
                .filter(|ticket| !ticket.is_empty())
                .ok_or_else(|| {
                    BackendError::Authentication("Discord did not provide an MFA ticket".into())
                })?;
            *self
                .shared
                .pending_mfa_ticket
                .lock()
                .map_err(|_| BackendError::Offline)? = Some(Secret::new(ticket.to_owned()));
            return Ok(AccountLoginStep::TotpRequired);
        }
        Err(BackendError::Authentication(
            "Discord did not complete sign-in or identify an MFA step".into(),
        ))
    }
    #[cfg(feature = "discord-user-session")]
    async fn complete_totp(&self, code: Secret<String>) -> BackendResult<()> {
        let ticket = self
            .shared
            .pending_mfa_ticket
            .lock()
            .map_err(|_| BackendError::Offline)?
            .as_ref()
            .map(|ticket| ticket.expose_secret().clone())
            .ok_or_else(|| BackendError::Authentication("Start sign-in again".into()))?;
        if code.expose_secret().trim().is_empty() {
            return Err(BackendError::Authentication(
                "enter your authenticator code".into(),
            ));
        }
        let payload = password_auth_request(
            "mfa/totp",
            json!({
                "ticket": ticket,
                "code": code.expose_secret().trim(),
            }),
        )
        .await?;
        let token = payload
            .get("token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                BackendError::Authentication("Discord did not return a session".into())
            })?;
        self.authenticate_session(Secret::new(token.to_owned()))
            .await
    }
    async fn sign_out(&self) -> BackendResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.stop().await;
        *self
            .shared
            .pending_mfa_ticket
            .lock()
            .map_err(|_| BackendError::Offline)? = None;
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
    async fn presence(&self, user_id: UserId) -> BackendResult<Presence> {
        self.shared.token()?;
        self.shared
            .metadata
            .lock()
            .map_err(|_| BackendError::Offline)?
            .presences
            .get(&user_id)
            .cloned()
            .ok_or(BackendError::NotFound {
                what: "cached presence".into(),
            })
    }
    async fn set_presence(&self, presence: &PresenceDraft) -> BackendResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.require_write(Capability::RichPresence)?;
        let now = std::time::Instant::now();
        if let Some(last) = *self
            .shared
            .last_presence_write
            .lock()
            .map_err(|_| BackendError::Offline)?
        {
            let elapsed = now.saturating_duration_since(last);
            if elapsed < Duration::from_secs(5) {
                return Err(BackendError::RateLimited {
                    retry_after: litecord_types::DurationMs::from_millis(
                        (Duration::from_secs(5) - elapsed).as_millis() as u64,
                    ),
                });
            }
        }
        let status = match presence.status {
            PresenceStatus::Offline | PresenceStatus::Invisible => "invisible",
            PresenceStatus::Unknown => {
                return Err(BackendError::PermissionDenied {
                    what: "choose Online, Idle, Do not disturb or Invisible".into(),
                })
            }
            other => other.as_str(),
        };
        let activities = presence
            .activity
            .as_ref()
            .map(|a| vec![json!({"name":a.name,"type":0,"details":a.details,"state":a.state})])
            .unwrap_or_default();
        let payload=json!({"op":3,"d":{"since":null,"activities":activities,"status":status,"afk":presence.status==PresenceStatus::Idle}}).to_string();
        let sender = self
            .shared
            .gateway_writes
            .lock()
            .map_err(|_| BackendError::Offline)?
            .clone()
            .ok_or(BackendError::NotConnected)?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        sender
            .try_send(GatewayWrite {
                payload,
                generation: self.shared.generation.load(Ordering::Acquire),
                response: tx,
                expires: tokio::time::Instant::now() + Duration::from_secs(7),
            })
            .map_err(|_| BackendError::NotConnected)?;
        tokio::time::timeout(Duration::from_secs(7), rx)
            .await
            .map_err(|_| BackendError::DeliveryUncertain("Presence submission timed out".into()))?
            .map_err(|_| {
                BackendError::DeliveryUncertain("Presence submission interrupted".into())
            })??;
        *self
            .shared
            .last_presence_write
            .lock()
            .map_err(|_| BackendError::Offline)? = Some(now);
        // Gateway supplies no separate acknowledgement for opcode 3. This
        // records the submitted state; a subsequent presence event may refine it.
        let user_id = self
            .shared
            .pinned_account
            .lock()
            .map_err(|_| BackendError::Offline)?
            .ok_or(BackendError::NotConnected)?;
        let event = DiscordEvent::PresenceChanged {
            user_id,
            presence: Presence {
                status: presence.status,
                activity: presence.activity.clone(),
            },
        };
        self.shared.remember(&event);
        self.confirm(event).await
    }
    async fn relationship_action(
        &self,
        user_id: UserId,
        action: RelationshipAction,
    ) -> BackendResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        let capability = if matches!(
            action,
            RelationshipAction::Block | RelationshipAction::Unblock
        ) {
            Capability::Blocking
        } else {
            Capability::FriendRequests
        };
        self.require_write(capability)?;
        if *self
            .shared
            .pinned_account
            .lock()
            .map_err(|_| BackendError::Offline)?
            == Some(user_id)
        {
            return Err(BackendError::PermissionDenied {
                what: "cannot change your own relationship".into(),
            });
        }
        let before = self.relationships().await?;
        let kind = before
            .iter()
            .find(|(r, _)| r.user_id == user_id)
            .map(|(r, _)| r.discord)
            .unwrap_or(RelationshipKind::None);
        let allowed = match action {
            RelationshipAction::AcceptFriendRequest | RelationshipAction::RejectFriendRequest => {
                kind == RelationshipKind::PendingIncoming
            }
            RelationshipAction::RemoveFriend => kind == RelationshipKind::Friend,
            RelationshipAction::Unblock => kind == RelationshipKind::Blocked,
            RelationshipAction::SendFriendRequest => {
                matches!(kind, RelationshipKind::None | RelationshipKind::Implicit)
            }
            RelationshipAction::Block => kind != RelationshipKind::Blocked,
        };
        if !allowed {
            return Err(BackendError::PermissionDenied {
                what: "relationship changed; refresh before acting".into(),
            });
        }
        let remove = matches!(
            action,
            RelationshipAction::RejectFriendRequest
                | RelationshipAction::RemoveFriend
                | RelationshipAction::Unblock
        );
        self.shared
            .call(RestRequest {
                method: if remove { Method::Delete } else { Method::Put },
                path: format!("/users/@me/relationships/{user_id}"),
                route: if remove {
                    "DELETE /users/@me/relationships/{user.id}"
                } else {
                    "PUT /users/@me/relationships/{user.id}"
                },
                body: if remove {
                    None
                } else {
                    Some(json!({"type":if action==RelationshipAction::Block {2} else {1}}))
                },
            })
            .await?;
        let entries = self.relationships().await.map_err(|_| {
            BackendError::DeliveryUncertain("Relationship write accepted but refresh failed".into())
        })?;
        let observed = entries
            .iter()
            .find(|(r, _)| r.user_id == user_id)
            .map(|(r, _)| r.discord)
            .unwrap_or(RelationshipKind::None);
        let confirmed = match action {
            RelationshipAction::Block => observed == RelationshipKind::Blocked,
            RelationshipAction::AcceptFriendRequest => observed == RelationshipKind::Friend,
            RelationshipAction::SendFriendRequest => matches!(
                observed,
                RelationshipKind::Friend | RelationshipKind::PendingOutgoing
            ),
            _ => matches!(
                observed,
                RelationshipKind::None | RelationshipKind::Implicit
            ),
        };
        self.confirm(DiscordEvent::RelationshipsSnapshot { entries })
            .await?;
        if !confirmed {
            return Err(BackendError::DeliveryUncertain(
                "Relationship result not yet observed; refresh before retrying".into(),
            ));
        }
        Ok(())
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
        let rows = raw
            .as_array()
            .ok_or_else(|| BackendError::Sdk("unexpected relationship list".into()))?;
        Ok(rest::lenient(rows, "friend entry", translate::relationship))
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
        if self.shared.access() == SessionAccessMode::ReadOnly {
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
        let rows = raw
            .as_array()
            .ok_or_else(|| BackendError::Sdk("unexpected private channel list".into()))?;
        let mut out = rest::lenient(rows, "DM", translate::conversation);
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
        let message = common::message(&raw).map_err(|_| {
            BackendError::DeliveryUncertain(
                "Discord returned an unexpected edit confirmation; refresh before retrying".into(),
            )
        })?;
        self.verify_own_message(&message, conversation)
            .map_err(|_| {
                BackendError::DeliveryUncertain(
                    "Discord returned a mismatched edit confirmation; refresh before retrying"
                        .into(),
                )
            })?;
        if message.id != id {
            return Err(BackendError::DeliveryUncertain(
                "Discord returned a mismatched edit identity; refresh before retrying".into(),
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
        // Paging depends on how many rows Discord returned, including any
        // that were skipped as unreadable.
        let returned = raw.as_array().map_or(0, Vec::len);
        if let Some(rows) = raw.as_array() {
            for row in rows {
                if let Some(nonce) = translate::message_nonce(row) {
                    let Ok(message) = common::message(row) else {
                        continue;
                    };
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
        let full = returned == req.effective_limit() as usize;
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
