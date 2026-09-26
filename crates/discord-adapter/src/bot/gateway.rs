//! Discord Gateway v10 session state machine for a bot connection
//! ([`DiscordSource::BotGateway`] in `litecord_types::provenance`).
//!
//! This module is deliberately **pure**: it holds no socket, spawns no
//! timers, and reads no clock or randomness of its own. [`GatewaySession`]
//! consumes text frames and tick/close notifications supplied by a driver
//! (websocket I/O, sleeping, RNG for heartbeat jitter) and returns
//! [`Output`] values describing what the driver should do next — send a
//! frame, hand a dispatch to the translator, reconnect, or give up. That
//! makes the whole protocol dance deterministic and unit-testable without a
//! network.
//!
//! ## Protocol decisions encoded here
//!
//! * **Heartbeating.** Discord asks for the first heartbeat at
//!   `heartbeat_interval * jitter` after Hello (`jitter` in `[0, 1)`, chosen
//!   by the caller so this module stays deterministic), then every
//!   `heartbeat_interval`. If we are still waiting on an ACK for the
//!   *previous* heartbeat when the next one comes due, the connection is
//!   considered zombied and we ask the driver to reconnect (resuming, since
//!   we still have a session). The heartbeat payload's `d` is the last
//!   sequence number we saw, or `null` before the first dispatch.
//! * **Identify vs. Resume.** Whenever Hello arrives (first connect *or*
//!   after a reconnect), we resume if and only if we still hold a
//!   `session_id` and a sequence number; otherwise we identify fresh. A
//!   close/Invalid-Session that Discord says is not resumable clears both,
//!   so the next Hello naturally re-identifies.
//! * **Close code handling.** 4004/4010/4011/4012/4013/4014 are
//!   unrecoverable (bad auth, bad shard config, disallowed intents, …) and
//!   surface as [`Output::Fatal`] — the driver must not retry. 4007
//!   ("already authenticated") and 4009 ("session timed out") mean the
//!   session is dead: we clear it and reconnect with a fresh Identify.
//!   Every other close (including a raw transport error/1006, which the
//!   driver reports as `None`) reconnects, resuming if a session survives.
//! * **Never logging the token.** The token lives only in
//!   [`litecord_core::secrets::Secret`] and is read exactly twice, to build
//!   the Identify/Resume payload. `Debug` for [`GatewaySession`] and
//!   [`GatewayConfig`] never prints it (`Secret`'s own `Debug` redacts it
//!   unconditionally), and no [`Output::Fatal`] message is ever built from
//!   token bytes.
//!
//! Discord additionally recommends waiting 1-5 seconds before reconnecting
//! after a non-resumable Invalid Session (op 9, `d: false`); this module has
//! no clock of its own, so that wait is the driver's responsibility before
//! it opens the new socket and calls [`GatewaySession::on_reconnected`].

use std::fmt;

use serde::Deserialize;
use serde_json::{json, Value};

use litecord_core::secrets::Secret;
use litecord_types::{DurationMs, Timestamp};

/// Discord Gateway intent bits (only the ones this adapter cares about;
/// see the Discord docs for the full list).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Intents(pub u64);

impl Intents {
    pub const GUILDS: u64 = 1 << 0;
    pub const GUILD_MEMBERS: u64 = 1 << 1;
    pub const GUILD_PRESENCES: u64 = 1 << 8;
    pub const GUILD_MESSAGES: u64 = 1 << 9;
    pub const DIRECT_MESSAGES: u64 = 1 << 12;
    pub const MESSAGE_CONTENT: u64 = 1 << 15;

    /// A reasonable default for a bot that reads and reacts to guild
    /// messages: `GUILDS | GUILD_MESSAGES | MESSAGE_CONTENT`.
    pub fn default_bot() -> Self {
        Intents(Self::GUILDS | Self::GUILD_MESSAGES | Self::MESSAGE_CONTENT)
    }
}

/// Static configuration for one gateway session. Cheap to construct; the
/// token is the only sensitive field and is never exposed by `Debug`.
#[derive(Debug)]
pub struct GatewayConfig {
    pub token: Secret<String>,
    pub intents: Intents,
    /// `properties.os` sent with Identify, e.g. `"linux"`.
    pub properties_os: String,
}

/// Where a [`GatewaySession`] currently is in the connection lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Socket is open; waiting for the initial Hello.
    AwaitingHello,
    /// Hello received, Identify sent, waiting for READY.
    Identifying,
    /// Hello received, Resume sent, waiting for RESUMED.
    Resuming,
    /// Session established (READY or RESUMED seen).
    Ready,
    /// The session has ended; the driver must open a new socket and call
    /// [`GatewaySession::on_reconnected`] (or stop, for a fatal end).
    Closed,
}

/// One instruction for the driver to carry out.
#[derive(Clone, PartialEq)]
pub enum Output {
    /// Send this JSON text frame on the socket.
    Send(String),
    /// A Dispatch (op 0) event, decoded for the translator layer.
    Dispatch { event: String, data: Value },
    /// Close the socket and reconnect. `resume` says whether the next
    /// Identify-or-Resume choice should resume this session; `url` is the
    /// resume gateway URL to reconnect to when it is known.
    Reconnect { resume: bool, url: Option<String> },
    /// Unrecoverable: stop retrying and surface this message to the user.
    /// Always safe to display (never contains the token).
    Fatal(String),
    /// The session just became ready (READY dispatch handled).
    Ready,
    /// The session was just resumed (RESUMED dispatch handled).
    Resumed,
}

impl fmt::Debug for Output {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Send(_) => f.write_str("Send([REDACTED])"),
            Self::Dispatch { event, .. } => f
                .debug_struct("Dispatch")
                .field("event", event)
                .finish_non_exhaustive(),
            Self::Reconnect { resume, url } => f
                .debug_struct("Reconnect")
                .field("resume", resume)
                .field("url", url)
                .finish(),
            Self::Fatal(message) => f.debug_tuple("Fatal").field(message).finish(),
            Self::Ready => f.write_str("Ready"),
            Self::Resumed => f.write_str("Resumed"),
        }
    }
}

/// Raw shape of every gateway frame: `{"op", "d", "s", "t"}`.
#[derive(Deserialize)]
struct RawFrame {
    op: u8,
    #[serde(default)]
    d: Value,
    #[serde(default)]
    s: Option<u64>,
    #[serde(default)]
    t: Option<String>,
}

const OP_DISPATCH: u8 = 0;
const OP_HEARTBEAT: u8 = 1;
const OP_IDENTIFY: u8 = 2;
const OP_RESUME: u8 = 6;
const OP_RECONNECT: u8 = 7;
const OP_INVALID_SESSION: u8 = 9;
const OP_HELLO: u8 = 10;
const OP_HEARTBEAT_ACK: u8 = 11;

/// A Discord Gateway v10 session, driven entirely by [`GatewaySession::on_frame`],
/// [`GatewaySession::on_tick`] and [`GatewaySession::on_close`]. See the
/// module docs for the protocol decisions this encodes.
pub struct GatewaySession {
    cfg: GatewayConfig,
    state: State,
    sequence: Option<u64>,
    session_id: Option<String>,
    resume_url: Option<String>,
    heartbeat_interval: Option<DurationMs>,
    next_heartbeat_at: Option<Timestamp>,
    /// Set when we've sent a heartbeat and are still waiting for op 11.
    awaiting_ack: bool,
    user_session: bool,
}

impl fmt::Debug for GatewaySession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GatewaySession")
            .field("state", &self.state)
            .field("sequence", &self.sequence)
            .field("session_id", &self.session_id)
            .field("intents", &self.cfg.intents)
            .field("awaiting_ack", &self.awaiting_ack)
            .finish_non_exhaustive()
    }
}

impl GatewaySession {
    pub fn new(cfg: GatewayConfig) -> Self {
        Self {
            cfg,
            state: State::AwaitingHello,
            sequence: None,
            session_id: None,
            resume_url: None,
            heartbeat_interval: None,
            next_heartbeat_at: None,
            awaiting_ack: false,
            user_session: false,
        }
    }

    /// Reuse transport lifecycle only; user Identify has no bot intent bits.
    /// Account protocol compatibility remains experimental.
    pub fn new_user_session(token: Secret<String>, properties_os: String) -> Self {
        let mut session = Self::new(GatewayConfig {
            token,
            intents: Intents(0),
            properties_os,
        });
        session.user_session = true;
        session
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn sequence(&self) -> Option<u64> {
        self.sequence
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// True if we hold enough state (`session_id` + last sequence) to
    /// attempt a Resume on the next Hello.
    fn can_resume(&self) -> bool {
        self.session_id.is_some() && self.sequence.is_some()
    }

    fn identify_payload(&self) -> String {
        let mut payload = json!({
            "op": OP_IDENTIFY,
            "d": {
                "token": self.cfg.token.expose_secret(),
                "intents": self.cfg.intents.0,
                "properties": {
                    "os": self.cfg.properties_os,
                    "browser": "litecord",
                    "device": "litecord",
                },
            },
        });
        if self.user_session {
            if let Some(data) = payload.get_mut("d").and_then(Value::as_object_mut) {
                data.remove("intents");
                data.insert("compress".into(), Value::Bool(false));
            }
        }
        payload.to_string()
    }

    fn resume_payload(&self) -> String {
        let payload = json!({
            "op": OP_RESUME,
            "d": {
                "token": self.cfg.token.expose_secret(),
                "session_id": self.session_id,
                "seq": self.sequence,
            },
        });
        payload.to_string()
    }

    fn heartbeat_payload(&self) -> String {
        json!({ "op": OP_HEARTBEAT, "d": self.sequence }).to_string()
    }

    /// Clears session identity so the next Hello identifies fresh.
    fn clear_session(&mut self) {
        self.session_id = None;
        self.sequence = None;
        self.resume_url = None;
    }

    fn enter_closed(&mut self) {
        self.state = State::Closed;
        self.next_heartbeat_at = None;
        self.heartbeat_interval = None;
        self.awaiting_ack = false;
    }

    /// Handle one incoming text frame at time `now`. `jitter` in `[0, 1)`
    /// is consulted only when this frame is a Hello, to place the first
    /// heartbeat. Invalid JSON is ignored (returns no output).
    pub fn on_frame(&mut self, text: &str, now: Timestamp, jitter: f64) -> Vec<Output> {
        let Ok(frame) = serde_json::from_str::<RawFrame>(text) else {
            return Vec::new();
        };
        if let Some(s) = frame.s {
            self.sequence = Some(s);
        }

        match frame.op {
            OP_DISPATCH => self.handle_dispatch(frame.t, frame.d),
            OP_HEARTBEAT => {
                // Server asked for an immediate heartbeat: send one now and
                // reset the schedule as if it were our own regular beat.
                self.awaiting_ack = true;
                if let Some(interval) = self.heartbeat_interval {
                    self.next_heartbeat_at = Some(now.saturating_add(interval));
                }
                vec![Output::Send(self.heartbeat_payload())]
            }
            OP_RECONNECT => {
                let url = self.resume_url.clone();
                self.enter_closed();
                vec![Output::Reconnect { resume: true, url }]
            }
            OP_INVALID_SESSION => {
                let resumable = frame.d.as_bool().unwrap_or(false);
                if resumable {
                    let url = self.resume_url.clone();
                    self.enter_closed();
                    vec![Output::Reconnect { resume: true, url }]
                } else {
                    self.clear_session();
                    self.enter_closed();
                    vec![Output::Reconnect {
                        resume: false,
                        url: None,
                    }]
                }
            }
            OP_HELLO => self.handle_hello(frame.d, now, jitter),
            OP_HEARTBEAT_ACK => {
                self.awaiting_ack = false;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn handle_hello(&mut self, d: Value, now: Timestamp, jitter: f64) -> Vec<Output> {
        let interval_ms = d
            .get("heartbeat_interval")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let interval = DurationMs::from_millis(interval_ms);
        self.heartbeat_interval = Some(interval);
        let jitter = jitter.clamp(0.0, f64::from(u32::MAX));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let first_delay_ms = (interval_ms as f64 * jitter).floor() as u64;
        self.next_heartbeat_at = Some(now.saturating_add(DurationMs::from_millis(first_delay_ms)));
        self.awaiting_ack = false;

        if self.can_resume() {
            self.state = State::Resuming;
            vec![Output::Send(self.resume_payload())]
        } else {
            self.state = State::Identifying;
            vec![Output::Send(self.identify_payload())]
        }
    }

    fn handle_dispatch(&mut self, t: Option<String>, d: Value) -> Vec<Output> {
        let Some(event) = t else {
            // Malformed dispatch (no event name); the sequence bump above
            // already happened, nothing else to do with it.
            return Vec::new();
        };
        match event.as_str() {
            "READY" => {
                self.session_id = d
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.resume_url = d
                    .get("resume_gateway_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.state = State::Ready;
                vec![Output::Ready, Output::Dispatch { event, data: d }]
            }
            "RESUMED" => {
                self.state = State::Ready;
                vec![Output::Resumed, Output::Dispatch { event, data: d }]
            }
            _ => vec![Output::Dispatch { event, data: d }],
        }
    }

    /// Poll timers at time `now`. Returns a heartbeat [`Output::Send`] if
    /// one is due, or an [`Output::Reconnect`] (resuming) if the previous
    /// heartbeat was never ACKed (zombied connection). Empty if no timer is
    /// due yet or none is scheduled (e.g. before Hello, or once closed).
    pub fn on_tick(&mut self, now: Timestamp) -> Vec<Output> {
        if self.state == State::Closed {
            return Vec::new();
        }
        let Some(due) = self.next_heartbeat_at else {
            return Vec::new();
        };
        if now < due {
            return Vec::new();
        }
        if self.awaiting_ack {
            // We reached the next beat without an ACK for the last one.
            let url = self.resume_url.clone();
            let resume = self.session_id.is_some();
            self.enter_closed();
            return vec![Output::Reconnect { resume, url }];
        }
        let interval = self.heartbeat_interval.unwrap_or(DurationMs::ZERO);
        self.awaiting_ack = true;
        self.next_heartbeat_at = Some(now.saturating_add(interval));
        vec![Output::Send(self.heartbeat_payload())]
    }

    /// When the next timer fires, if any (for the driver's sleep). `None`
    /// once the session is closed.
    pub fn next_deadline(&self) -> Option<Timestamp> {
        if self.state == State::Closed {
            return None;
        }
        self.next_heartbeat_at
    }

    /// The socket closed with `code` (`None` for a transport-level error,
    /// e.g. a raw disconnect with no close frame).
    pub fn on_close(&mut self, code: Option<u16>) -> Vec<Output> {
        let output = match code {
            Some(4004) => Output::Fatal("authentication failed: invalid token".to_string()),
            Some(4010) => Output::Fatal("invalid shard".to_string()),
            Some(4011) => Output::Fatal("sharding required".to_string()),
            Some(4012) => Output::Fatal("invalid gateway API version".to_string()),
            Some(4013) => Output::Fatal("invalid intents".to_string()),
            Some(4014) => Output::Fatal("disallowed intents".to_string()),
            Some(4007) | Some(4009) => {
                self.clear_session();
                Output::Reconnect {
                    resume: false,
                    url: None,
                }
            }
            _ => Output::Reconnect {
                resume: self.session_id.is_some(),
                url: self.resume_url.clone(),
            },
        };
        self.enter_closed();
        vec![output]
    }

    /// Called by the driver once a new socket is open after a
    /// [`Output::Reconnect`]. The next Hello will Identify or Resume
    /// depending on whether a session (`session_id` + sequence) survived.
    pub fn on_reconnected(&mut self) {
        self.state = State::AwaitingHello;
        self.heartbeat_interval = None;
        self.next_heartbeat_at = None;
        self.awaiting_ack = false;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn cfg(token: &str) -> GatewayConfig {
        GatewayConfig {
            token: Secret::new(token.to_string()),
            intents: Intents::default_bot(),
            properties_os: "linux".to_string(),
        }
    }

    fn hello(interval_ms: u64) -> String {
        json!({"op": 10, "d": {"heartbeat_interval": interval_ms}, "s": null, "t": null})
            .to_string()
    }

    fn dispatch(t: &str, s: u64, d: Value) -> String {
        json!({"op": 0, "d": d, "s": s, "t": t}).to_string()
    }

    fn ready_data(session_id: &str, resume_url: &str) -> Value {
        json!({"session_id": session_id, "resume_gateway_url": resume_url, "user": {"id": "1"}})
    }

    fn send_payload(outputs: &[Output]) -> &str {
        match outputs.first() {
            Some(Output::Send(s)) => s.as_str(),
            other => panic!("expected Output::Send, got {other:?}"),
        }
    }

    #[test]
    fn hello_sends_identify_with_intents_and_properties() {
        let mut s = GatewaySession::new(cfg("tok-123"));
        let out = s.on_frame(&hello(45_000), Timestamp::from_millis(0), 0.5);
        assert_eq!(s.state(), State::Identifying);
        let payload: Value = serde_json::from_str(send_payload(&out)).unwrap();
        assert_eq!(payload["op"], 2);
        assert_eq!(payload["d"]["token"], "tok-123");
        assert_eq!(payload["d"]["intents"], Intents::default_bot().0);
        assert_eq!(payload["d"]["properties"]["os"], "linux");
        assert_eq!(payload["d"]["properties"]["browser"], "litecord");
        assert_eq!(payload["d"]["properties"]["device"], "litecord");
    }

    #[test]
    fn default_bot_intents_are_guilds_messages_and_content() {
        let want = Intents::GUILDS | Intents::GUILD_MESSAGES | Intents::MESSAGE_CONTENT;
        assert_eq!(Intents::default_bot().0, want);
    }

    #[test]
    fn heartbeat_carries_null_before_first_dispatch() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        assert_eq!(s.sequence(), None);
        let out = s.on_tick(Timestamp::from_millis(0));
        let payload: Value = serde_json::from_str(send_payload(&out)).unwrap();
        assert_eq!(payload["op"], 1);
        assert!(payload["d"].is_null());
    }

    #[test]
    fn heartbeat_timing_first_jitter_then_interval() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(10_000), Timestamp::from_millis(0), 0.3);
        // First heartbeat at interval * jitter.
        assert_eq!(s.next_deadline(), Some(Timestamp::from_millis(3_000)));
        assert!(s.on_tick(Timestamp::from_millis(2_999)).is_empty());
        let out = s.on_tick(Timestamp::from_millis(3_000));
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Output::Send(p) if p.contains("\"op\":1")));
        // Next one a full interval later.
        assert_eq!(s.next_deadline(), Some(Timestamp::from_millis(13_000)));
    }

    #[test]
    fn ack_prevents_zombie_reconnect() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        assert!(!s.on_tick(Timestamp::from_millis(0)).is_empty()); // sends heartbeat
        let ack = json!({"op": 11, "d": null, "s": null, "t": null}).to_string();
        assert!(s.on_frame(&ack, Timestamp::from_millis(10), 0.0).is_empty());
        // Next beat at +1000ms should send normally, not reconnect.
        let out = s.on_tick(Timestamp::from_millis(1_000));
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Output::Send(_)));
    }

    #[test]
    fn missing_ack_causes_zombie_reconnect() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        assert!(!s.on_tick(Timestamp::from_millis(0)).is_empty()); // sent, no ack
        let out = s.on_tick(Timestamp::from_millis(1_000));
        assert_eq!(
            out,
            vec![Output::Reconnect {
                resume: false,
                url: None
            }]
        );
        assert_eq!(s.state(), State::Closed);
    }

    #[test]
    fn zombie_reconnect_resumes_when_session_established() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        let ready = dispatch("READY", 1, ready_data("sess-1", "wss://resume.example"));
        s.on_frame(&ready, Timestamp::from_millis(1), 0.0);
        s.on_tick(Timestamp::from_millis(1_000)); // sent, no ack
        let out = s.on_tick(Timestamp::from_millis(2_000));
        assert_eq!(
            out,
            vec![Output::Reconnect {
                resume: true,
                url: Some("wss://resume.example".to_string())
            }]
        );
    }

    #[test]
    fn ready_stores_session_and_tracks_sequence() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        let ready = dispatch("READY", 7, ready_data("sess-abc", "wss://resume.example/"));
        let out = s.on_frame(&ready, Timestamp::from_millis(0), 0.0);
        assert_eq!(out[0], Output::Ready);
        assert!(matches!(&out[1], Output::Dispatch { event, .. } if event == "READY"));
        assert_eq!(s.session_id(), Some("sess-abc"));
        assert_eq!(s.sequence(), Some(7));
        assert_eq!(s.state(), State::Ready);

        let msg = dispatch("MESSAGE_CREATE", 8, json!({"id": "42"}));
        let out = s.on_frame(&msg, Timestamp::from_millis(0), 0.0);
        assert_eq!(
            out,
            vec![Output::Dispatch {
                event: "MESSAGE_CREATE".to_string(),
                data: json!({"id": "42"})
            }]
        );
        assert_eq!(s.sequence(), Some(8));
    }

    #[test]
    fn reconnect_then_hello_sends_resume_with_last_seq() {
        let mut s = GatewaySession::new(cfg("tok-xyz"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        let ready = dispatch("READY", 3, ready_data("sess-1", "wss://resume.example"));
        s.on_frame(&ready, Timestamp::from_millis(0), 0.0);

        // Simulate a drop and the driver reopening the socket.
        s.on_close(None);
        s.on_reconnected();
        assert_eq!(s.state(), State::AwaitingHello);

        let out = s.on_frame(&hello(1_000), Timestamp::from_millis(5_000), 0.0);
        assert_eq!(s.state(), State::Resuming);
        let payload: Value = serde_json::from_str(send_payload(&out)).unwrap();
        assert_eq!(payload["op"], 6);
        assert_eq!(payload["d"]["token"], "tok-xyz");
        assert_eq!(payload["d"]["session_id"], "sess-1");
        assert_eq!(payload["d"]["seq"], 3);
    }

    #[test]
    fn resumed_dispatch_reports_resumed_and_ready_state() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        let ready = dispatch("READY", 1, ready_data("sess-1", "wss://resume.example"));
        s.on_frame(&ready, Timestamp::from_millis(0), 0.0);
        s.on_close(None);
        s.on_reconnected();
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);

        let resumed = dispatch("RESUMED", 2, json!({}));
        let out = s.on_frame(&resumed, Timestamp::from_millis(0), 0.0);
        assert_eq!(out[0], Output::Resumed);
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn op7_reconnect_requests_resume_with_url() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        let ready = dispatch("READY", 1, ready_data("sess-1", "wss://resume.example"));
        s.on_frame(&ready, Timestamp::from_millis(0), 0.0);

        let reconnect = json!({"op": 7, "d": null, "s": null, "t": null}).to_string();
        let out = s.on_frame(&reconnect, Timestamp::from_millis(0), 0.0);
        assert_eq!(
            out,
            vec![Output::Reconnect {
                resume: true,
                url: Some("wss://resume.example".to_string())
            }]
        );
        assert_eq!(s.state(), State::Closed);
    }

    #[test]
    fn op9_resumable_true_reconnects_with_resume() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        let ready = dispatch("READY", 1, ready_data("sess-1", "wss://resume.example"));
        s.on_frame(&ready, Timestamp::from_millis(0), 0.0);

        let invalid = json!({"op": 9, "d": true, "s": null, "t": null}).to_string();
        let out = s.on_frame(&invalid, Timestamp::from_millis(0), 0.0);
        assert_eq!(
            out,
            vec![Output::Reconnect {
                resume: true,
                url: Some("wss://resume.example".to_string())
            }]
        );
        assert_eq!(s.session_id(), Some("sess-1"));
    }

    #[test]
    fn op9_resumable_false_clears_session_and_reconnects_fresh() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        let ready = dispatch("READY", 1, ready_data("sess-1", "wss://resume.example"));
        s.on_frame(&ready, Timestamp::from_millis(0), 0.0);

        let invalid = json!({"op": 9, "d": false, "s": null, "t": null}).to_string();
        let out = s.on_frame(&invalid, Timestamp::from_millis(0), 0.0);
        assert_eq!(
            out,
            vec![Output::Reconnect {
                resume: false,
                url: None
            }]
        );
        assert_eq!(s.session_id(), None);
        assert_eq!(s.sequence(), None);
    }

    #[test]
    fn close_4004_is_fatal_and_never_contains_token() {
        let mut s = GatewaySession::new(cfg("super-secret-token"));
        let out = s.on_close(Some(4004));
        match &out[0] {
            Output::Fatal(msg) => assert!(!msg.contains("super-secret-token")),
            other => panic!("expected Fatal, got {other:?}"),
        }
        assert_eq!(s.state(), State::Closed);
    }

    #[test]
    fn other_disallowed_close_codes_are_fatal() {
        for code in [4010, 4011, 4012, 4013, 4014] {
            let mut s = GatewaySession::new(cfg("tok"));
            let out = s.on_close(Some(code));
            assert!(matches!(&out[0], Output::Fatal(_)), "code {code}");
        }
    }

    #[test]
    fn close_4007_reconnects_without_resume() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        let ready = dispatch("READY", 1, ready_data("sess-1", "wss://resume.example"));
        s.on_frame(&ready, Timestamp::from_millis(0), 0.0);

        let out = s.on_close(Some(4007));
        assert_eq!(
            out,
            vec![Output::Reconnect {
                resume: false,
                url: None
            }]
        );
        assert_eq!(s.session_id(), None);
    }

    #[test]
    fn close_4009_reconnects_without_resume() {
        let mut s = GatewaySession::new(cfg("tok"));
        let out = s.on_close(Some(4009));
        assert_eq!(
            out,
            vec![Output::Reconnect {
                resume: false,
                url: None
            }]
        );
    }

    #[test]
    fn transport_close_resumes_when_session_present() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        let ready = dispatch("READY", 5, ready_data("sess-9", "wss://resume.example"));
        s.on_frame(&ready, Timestamp::from_millis(0), 0.0);

        let out = s.on_close(None);
        assert_eq!(
            out,
            vec![Output::Reconnect {
                resume: true,
                url: Some("wss://resume.example".to_string())
            }]
        );

        // 1006 behaves the same as a bare transport error.
        let mut s2 = GatewaySession::new(cfg("tok"));
        s2.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        s2.on_frame(
            &dispatch("READY", 5, ready_data("sess-9", "wss://resume.example")),
            Timestamp::from_millis(0),
            0.0,
        );
        let out2 = s2.on_close(Some(1006));
        assert_eq!(out, out2);
    }

    #[test]
    fn transport_close_without_session_does_not_resume() {
        let mut s = GatewaySession::new(cfg("tok"));
        let out = s.on_close(None);
        assert_eq!(
            out,
            vec![Output::Reconnect {
                resume: false,
                url: None
            }]
        );
    }

    #[test]
    fn invalid_json_frame_is_ignored() {
        let mut s = GatewaySession::new(cfg("tok"));
        assert!(s
            .on_frame("not json", Timestamp::from_millis(0), 0.0)
            .is_empty());
        assert_eq!(s.state(), State::AwaitingHello);
    }

    #[test]
    fn debug_never_contains_token() {
        let cfg = cfg("super-secret-token");
        let cfg_debug = format!("{cfg:?}");
        assert!(!cfg_debug.contains("super-secret-token"));

        let mut s = GatewaySession::new(cfg);
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.0);
        let session_debug = format!("{s:?}");
        assert!(!session_debug.contains("super-secret-token"));
    }

    #[test]
    fn heartbeat_immediate_request_sends_and_reschedules() {
        let mut s = GatewaySession::new(cfg("tok"));
        s.on_frame(&hello(1_000), Timestamp::from_millis(0), 0.9);
        // Server asks for an immediate heartbeat before our jittered one is due.
        let hb_req = json!({"op": 1, "d": null, "s": null, "t": null}).to_string();
        let out = s.on_frame(&hb_req, Timestamp::from_millis(100), 0.0);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Output::Send(p) if p.contains("\"op\":1")));
        assert_eq!(s.next_deadline(), Some(Timestamp::from_millis(1_100)));
    }
}
