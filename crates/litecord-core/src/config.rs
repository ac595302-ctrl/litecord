//! Centralized configuration.
//!
//! Precedence (lowest → highest): built-in defaults → TOML config file →
//! environment (`LITECORD_*`) → CLI overrides. This module is the **only**
//! place that reads environment variables; it takes the environment as an
//! iterator so it is testable.
//!
//! Secrets (tokens, keys) are *not* configuration and never appear here; see
//! [`crate::secrets`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use litecord_types::trust::AgentVisibility;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Deterministic synthetic data; all content labelled `Synthetic`.
    #[default]
    Demo,
    /// Official Discord Social SDK (requires the `discord-social-sdk` feature
    /// and SDK binaries).
    SocialSdk,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LitecordConfig {
    /// Directory for the database and caches.
    pub data_dir: PathBuf,
    pub database: DatabaseConfig,
    pub backend: BackendConfig,
    pub runtime: RuntimeConfig,
    pub hydration: HydrationConfig,
    pub cache: CacheConfig,
    pub retention: RetentionConfig,
    pub agent: AgentConfig,
    pub omni: OmniConfig,
    pub logging: LoggingConfig,
}

impl Default for LitecordConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(".litecord"),
            database: DatabaseConfig::default(),
            backend: BackendConfig::default(),
            runtime: RuntimeConfig::default(),
            hydration: HydrationConfig::default(),
            cache: CacheConfig::default(),
            retention: RetentionConfig::default(),
            agent: AgentConfig::default(),
            omni: OmniConfig::default(),
            logging: LoggingConfig::default(),
        }
    }
}

impl LitecordConfig {
    /// Resolved database path (relative paths are under `data_dir`).
    pub fn database_path(&self) -> PathBuf {
        match &self.database.path {
            Some(p) if p.is_absolute() => p.clone(),
            Some(p) => self.data_dir.join(p),
            None => self.data_dir.join("litecord.db"),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.runtime.event_queue_capacity == 0 {
            return Err(Error::config("runtime.event_queue_capacity must be > 0"));
        }
        if self.hydration.max_concurrent_jobs == 0 {
            return Err(Error::config("hydration.max_concurrent_jobs must be > 0"));
        }
        if self.hydration.backoff_max_ms < self.hydration.backoff_base_ms {
            return Err(Error::config(
                "hydration.backoff_max_ms must be >= hydration.backoff_base_ms",
            ));
        }
        if self.agent.approval_ttl_secs == 0 {
            return Err(Error::config("agent.approval_ttl_secs must be > 0"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Defaults to `<data_dir>/litecord.db`.
    pub path: Option<PathBuf>,
    pub busy_timeout_ms: u64,
    /// Number of read-only connections (WAL allows concurrent readers).
    pub read_pool_size: usize,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: None,
            busy_timeout_ms: 5_000,
            read_pool_size: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BackendConfig {
    pub kind: BackendKind,
    /// Discord application id (public, not a secret).
    pub application_id: Option<u64>,
    /// Seed for the demo backend's synthetic data.
    pub demo_seed: u64,
    /// Also run a synthetic application-bot source (demo of the optional
    /// bot adapter: guild channels readable/writable as the bot identity).
    pub demo_bot: bool,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            kind: BackendKind::Demo,
            application_id: None,
            demo_seed: 42,
            demo_bot: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    pub event_queue_capacity: usize,
    /// Approximate byte budget of the ingest queue (rounded up to KiB).
    pub event_queue_max_bytes: usize,
    pub broadcast_capacity: usize,
    pub shutdown_grace_ms: u64,
    /// Background work (history sync) pauses above this resident size.
    pub memory_soft_limit_mb: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            event_queue_capacity: 1_024,
            event_queue_max_bytes: 8 * 1024 * 1024,
            broadcast_capacity: 256,
            shutdown_grace_ms: 3_000,
            memory_soft_limit_mb: 256,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HydrationConfig {
    pub max_concurrent_jobs: usize,
    pub backoff_base_ms: u64,
    pub backoff_max_ms: u64,
    /// How often the staleness sweep runs.
    pub reconcile_interval_secs: u64,
    pub stale_after_current_user_secs: u64,
    pub stale_after_relationships_secs: u64,
    pub stale_after_guilds_secs: u64,
    pub stale_after_channels_secs: u64,
    pub stale_after_conversations_secs: u64,
    pub recent_messages_limit: u32,
    /// History sync: messages per page (1..=100).
    pub history_page_size: u32,
    /// History sync: pause between pages.
    pub history_page_interval_ms: u64,
    /// History sync waits until the UI has been idle this long.
    pub history_idle_after_ms: u64,
}

impl Default for HydrationConfig {
    fn default() -> Self {
        Self {
            max_concurrent_jobs: 2,
            backoff_base_ms: 1_000,
            backoff_max_ms: 5 * 60_000,
            reconcile_interval_secs: 60,
            stale_after_current_user_secs: 60 * 60,
            stale_after_relationships_secs: 15 * 60,
            stale_after_guilds_secs: 60 * 60,
            stale_after_channels_secs: 60 * 60,
            stale_after_conversations_secs: 10 * 60,
            recent_messages_limit: 50,
            history_page_size: 100,
            history_page_interval_ms: 750,
            history_idle_after_ms: 5_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    pub max_users: usize,
    pub max_messages: usize,
    /// Approximate byte ceiling of the message hot cache.
    pub max_message_cache_bytes: usize,
    pub max_hot_conversations: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_users: 1_000,
            max_messages: 5_000,
            max_message_cache_bytes: 4 * 1024 * 1024,
            max_hot_conversations: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionConfig {
    /// Raw message retention; `None` keeps everything the SDK gave us.
    pub raw_messages_days: Option<u32>,
    pub agent_runs_days: u32,
    pub event_log_days: u32,
    /// Delete locally when Discord reports a deletion (default: yes).
    pub purge_deleted_messages: bool,
    /// Messages newer than this are HOT, newer than `warm_days` WARM.
    pub hot_hours: u32,
    pub warm_days: u32,
    /// Database size ceiling in MiB. History sync stops at this size; it is
    /// required before history sync is offered (0 = history sync disabled).
    pub max_database_mb: u32,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            raw_messages_days: None,
            agent_runs_days: 14,
            event_log_days: 30,
            purge_deleted_messages: true,
            hot_hours: 24,
            warm_days: 14,
            max_database_mb: 2048,
        }
    }
}

/// Omni on an external harness (docs/AGENT_HARNESS.md). Which harness is
/// selected is a user preference stored by the app, not configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OmniConfig {
    /// Explicit harness binaries; unset = search `PATH`.
    pub codex_path: Option<PathBuf>,
    pub opencode_path: Option<PathBuf>,
    /// Model passed to the harness; unset = the harness's default.
    pub model: Option<String>,
    /// Stop the sidecar after this long without use.
    pub idle_shutdown_secs: u64,
    /// Unanswered harness approval requests are declined after this long.
    pub approval_timeout_secs: u64,
    /// Archived sessions kept (with transcripts); older ones are deleted.
    pub keep_archived_sessions: u32,
    /// Automation runs allowed per hour across all automations.
    pub max_automation_runs_per_hour: u32,
    pub heartbeat: HeartbeatConfig,
}

impl Default for OmniConfig {
    fn default() -> Self {
        Self {
            codex_path: None,
            opencode_path: None,
            model: None,
            idle_shutdown_secs: 600,
            approval_timeout_secs: 120,
            keep_archived_sessions: 50,
            max_automation_runs_per_hour: 6,
            heartbeat: HeartbeatConfig::default(),
        }
    }
}

/// Scheduled Omni check-ins (docs/AGENT_HARNESS.md §8). Off by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HeartbeatConfig {
    pub enabled: bool,
    /// Minimum minutes between check-ins.
    pub min_interval_mins: u32,
    pub max_per_hour: u32,
    /// Local writes (tasks, reminders, drafts) allowed per check-in.
    pub max_actions: u32,
    /// Let check-ins propose Discord actions (still approved by the user).
    pub allow_proposals: bool,
    /// No check-ins from `quiet_start_hour` to `quiet_end_hour` (UTC hours,
    /// 0-23). Equal values disable quiet hours.
    pub quiet_start_hour: u8,
    pub quiet_end_hour: u8,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_interval_mins: 60,
            max_per_hour: 2,
            max_actions: 3,
            allow_proposals: false,
            quiet_start_hour: 22,
            quiet_end_hour: 7,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentConfig {
    /// Approval tokens expire after this many seconds.
    pub approval_ttl_secs: u64,
    /// Visibility for conversations without an explicit setting.
    pub default_visibility: AgentVisibility,
    pub allow_message_proposals: bool,
    pub allow_presence_proposals: bool,
    pub allow_relationship_proposals: bool,
    /// Local writes (tasks, reminders, notes, drafts) by agents execute
    /// without approval when true. They are always audited.
    pub auto_approve_local_writes: bool,
    /// Default context budget in (estimated) tokens.
    pub default_token_budget: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            approval_ttl_secs: 10 * 60,
            default_visibility: AgentVisibility::Allowed,
            allow_message_proposals: true,
            allow_presence_proposals: true,
            allow_relationship_proposals: false,
            auto_approve_local_writes: true,
            default_token_budget: 8_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// `tracing` env-filter directive.
    pub filter: String,
    /// Include message content in logs. Off by default; never enable in
    /// production builds.
    pub log_message_content: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            filter: "info".into(),
            log_message_content: false,
        }
    }
}

/// Values supplied on the command line (highest precedence).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigOverrides {
    pub data_dir: Option<PathBuf>,
    pub database_path: Option<PathBuf>,
    pub backend: Option<BackendKind>,
    pub log_filter: Option<String>,
}

/// Builder implementing the precedence chain.
#[derive(Debug, Default)]
pub struct ConfigLoader {
    file: Option<PathBuf>,
    env: Vec<(String, String)>,
    overrides: ConfigOverrides,
}

impl ConfigLoader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn file(mut self, path: impl Into<PathBuf>) -> Self {
        self.file = Some(path.into());
        self
    }

    /// Supply environment variables (typically `std::env::vars()`); only
    /// `LITECORD_*` keys are considered.
    pub fn env(mut self, vars: impl IntoIterator<Item = (String, String)>) -> Self {
        self.env = vars
            .into_iter()
            .filter(|(k, _)| k.starts_with("LITECORD_"))
            .collect();
        self
    }

    pub fn overrides(mut self, o: ConfigOverrides) -> Self {
        self.overrides = o;
        self
    }

    pub fn load(self) -> Result<LitecordConfig> {
        let mut cfg = match &self.file {
            Some(path) => parse_file(path)?,
            None => LitecordConfig::default(),
        };
        apply_env(&mut cfg, &self.env)?;
        let o = self.overrides;
        if let Some(v) = o.data_dir {
            cfg.data_dir = v;
        }
        if let Some(v) = o.database_path {
            cfg.database.path = Some(v);
        }
        if let Some(v) = o.backend {
            cfg.backend.kind = v;
        }
        if let Some(v) = o.log_filter {
            cfg.logging.filter = v;
        }
        cfg.validate()?;
        Ok(cfg)
    }
}

fn parse_file(path: &Path) -> Result<LitecordConfig> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::with_source(
            crate::ErrorKind::Configuration,
            format!("cannot read config file {}", path.display()),
            e,
        )
    })?;
    toml::from_str(&text).map_err(|e| {
        Error::with_source(
            crate::ErrorKind::Configuration,
            format!("invalid config file {}", path.display()),
            e,
        )
    })
}

fn parse_backend(v: &str) -> Result<BackendKind> {
    match v {
        "demo" => Ok(BackendKind::Demo),
        "social_sdk" | "social-sdk" => Ok(BackendKind::SocialSdk),
        other => Err(Error::config(format!("unknown backend `{other}`"))),
    }
}

fn parse_num<T: std::str::FromStr>(key: &str, v: &str) -> Result<T> {
    v.parse()
        .map_err(|_| Error::config(format!("{key} must be a number")))
}

fn apply_env(cfg: &mut LitecordConfig, env: &[(String, String)]) -> Result<()> {
    for (k, v) in env {
        match k.as_str() {
            "LITECORD_DATA_DIR" => cfg.data_dir = PathBuf::from(v),
            "LITECORD_DATABASE_PATH" => cfg.database.path = Some(PathBuf::from(v)),
            "LITECORD_BACKEND" => cfg.backend.kind = parse_backend(v)?,
            "LITECORD_APPLICATION_ID" => cfg.backend.application_id = Some(parse_num(k, v)?),
            "LITECORD_LOG" => cfg.logging.filter = v.clone(),
            "LITECORD_EVENT_QUEUE_CAPACITY" => cfg.runtime.event_queue_capacity = parse_num(k, v)?,
            "LITECORD_APPROVAL_TTL_SECS" => cfg.agent.approval_ttl_secs = parse_num(k, v)?,
            // Secret-bearing variables are handled by the binary's secret
            // loading, never stored in config. Unknown keys are ignored so
            // that unrelated LITECORD_* variables do not break startup.
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        LitecordConfig::default().validate().unwrap();
    }

    #[test]
    fn precedence_file_then_env_then_cli() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("litecord.toml");
        std::fs::write(
            &path,
            r#"
data_dir = "/from/file"
[backend]
kind = "demo"
demo_seed = 7
[logging]
filter = "debug"
"#,
        )
        .unwrap();

        let cfg = ConfigLoader::new()
            .file(&path)
            .env([
                ("LITECORD_DATA_DIR".to_string(), "/from/env".to_string()),
                ("LITECORD_LOG".to_string(), "trace".to_string()),
                ("UNRELATED".to_string(), "x".to_string()),
            ])
            .overrides(ConfigOverrides {
                log_filter: Some("warn".into()),
                ..Default::default()
            })
            .load()
            .unwrap();

        assert_eq!(cfg.backend.demo_seed, 7, "file value kept");
        assert_eq!(cfg.data_dir, PathBuf::from("/from/env"), "env beats file");
        assert_eq!(cfg.logging.filter, "warn", "cli beats env");
        assert_eq!(cfg.database_path(), PathBuf::from("/from/env/litecord.db"));
    }

    #[test]
    fn invalid_values_are_rejected() {
        let err = ConfigLoader::new()
            .env([("LITECORD_EVENT_QUEUE_CAPACITY".to_string(), "0".to_string())])
            .load()
            .unwrap_err();
        assert_eq!(err.kind(), crate::ErrorKind::Configuration);
        let err = ConfigLoader::new()
            .env([("LITECORD_BACKEND".to_string(), "private_api".to_string())])
            .load()
            .unwrap_err();
        assert!(err.to_string().contains("unknown backend"));
    }

    #[test]
    fn example_config_file_parses() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/litecord.example.toml");
        let cfg = ConfigLoader::new().file(path).load().unwrap();
        assert_eq!(
            cfg,
            LitecordConfig::default(),
            "example documents the defaults"
        );
    }

    #[test]
    fn unknown_file_keys_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "bogus = 1").unwrap();
        assert!(ConfigLoader::new().file(&path).load().is_err());
    }
}
