//! Stage C: resumable, user-selected history sync (docs/ROADMAP.md §3).
//!
//! One conversation at a time, one page at a time, at background pace.
//! Every page goes through the ingest queue and the reducer, which stores
//! the messages and advances the conversation's checkpoint in the same
//! transaction, so a sync resumes exactly where it stopped after a restart.
//! Sync pauses while the UI is in use, under memory pressure, when the
//! database reaches its size quota, and when the source rate-limits.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use litecord_core::bus::IngestSender;
use litecord_core::config::LitecordConfig;
use litecord_core::error::{Error, ErrorKind, Result};
use litecord_core::events::{DiscordEvent, SourceEnvelope};
use litecord_core::ports::{BackendError, HistoryPageRequest, SocialBackend};
use litecord_store::repos;
use litecord_store::repos::history_sync::HistorySyncRecord;
use litecord_store::Database;
use litecord_types::social::ConversationKind;
use litecord_types::{ConversationId, Timestamp};

use crate::app::LitecordApp;

/// One conversation's sync state, for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistorySyncRow {
    pub conversation_id: ConversationId,
    pub title: String,
    pub enabled: bool,
    pub complete: bool,
    pub pages: u64,
    pub messages: u64,
    pub paused_reason: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistorySyncViewModel {
    pub rows: Vec<HistorySyncRow>,
    pub db_bytes: u64,
    /// 0 = history sync is disabled by configuration.
    pub quota_bytes: u64,
}

impl HistorySyncViewModel {
    pub fn row(&self, id: ConversationId) -> Option<&HistorySyncRow> {
        self.rows.iter().find(|r| r.conversation_id == id)
    }
}

fn quota_bytes(cfg: &LitecordConfig) -> u64 {
    u64::from(cfg.retention.max_database_mb) * 1024 * 1024
}

impl LitecordApp {
    /// Start (or resume) syncing a conversation's full accessible history.
    pub fn request_history_sync(&self, id: ConversationId) -> Result<()> {
        if quota_bytes(&self.inner.cfg) == 0 {
            return Err(Error::new(
                ErrorKind::Configuration,
                "history sync needs a database size limit (retention.max_database_mb)",
            ));
        }
        let exists = self
            .inner
            .db
            .read(|r| repos::conversations::get(r, id))?
            .is_some();
        if !exists {
            return Err(Error::not_found("conversation"));
        }
        self.inner
            .db
            .write(|tx| repos::history_sync::request(tx, id))?;
        Ok(())
    }

    /// Stop syncing a conversation. Messages already stored are kept.
    pub fn stop_history_sync(&self, id: ConversationId) -> Result<()> {
        self.inner
            .db
            .write(|tx| repos::history_sync::disable(tx, id))?;
        Ok(())
    }

    pub fn history_sync_view(&self) -> Result<HistorySyncViewModel> {
        let quota = quota_bytes(&self.inner.cfg);
        self.inner.db.read(|r| -> Result<HistorySyncViewModel> {
            let mut rows = Vec::new();
            for rec in repos::history_sync::list(r)? {
                let title = repos::conversations::get(r, rec.conversation_id)?
                    .and_then(|c| c.conversation.title.map(|t| t.to_string()))
                    .unwrap_or_else(|| format!("Conversation {}", rec.conversation_id));
                rows.push(HistorySyncRow {
                    conversation_id: rec.conversation_id,
                    title,
                    enabled: rec.enabled,
                    complete: rec.complete,
                    pages: rec.pages,
                    messages: rec.messages,
                    paused_reason: rec.paused_reason,
                    last_error: rec.last_error,
                });
            }
            Ok(HistorySyncViewModel {
                rows,
                db_bytes: litecord_store::db_size_bytes(r)?,
                quota_bytes: quota,
            })
        })
    }

    /// Record user input (navigation, commands) so background history sync
    /// yields while the user is actively using the app. Background view
    /// refreshes must not call this, or sync would starve itself.
    pub fn note_user_activity(&self) {
        self.inner
            .ui_activity
            .store(self.inner.db.now().as_millis(), Ordering::Relaxed);
    }
}

pub(crate) struct HistorySyncCtx {
    pub db: Database,
    pub cfg: LitecordConfig,
    pub backend: Arc<dyn SocialBackend>,
    pub bot: Option<Arc<dyn SocialBackend>>,
    pub ingest: IngestSender,
    pub ui_activity: Arc<AtomicI64>,
}

/// Why sync is waiting right now, if it is.
pub(crate) fn pause_reason(ctx: &HistorySyncCtx) -> Result<Option<String>> {
    let quota = quota_bytes(&ctx.cfg);
    let size = ctx.db.read(|r| litecord_store::db_size_bytes(r))?;
    if size >= quota {
        return Ok(Some(format!(
            "database reached its {} MiB limit",
            ctx.cfg.retention.max_database_mb
        )));
    }
    if let Some(rss) = litecord_core::metrics::resident_set_size() {
        if rss > u64::from(ctx.cfg.runtime.memory_soft_limit_mb) * 1024 * 1024 {
            return Ok(Some("memory is busy".into()));
        }
    }
    let idle_ms = ctx.db.now().as_millis() - ctx.ui_activity.load(Ordering::Relaxed);
    if idle_ms < ctx.cfg.hydration.history_idle_after_ms as i64 {
        return Ok(Some("waiting while you use the app".into()));
    }
    Ok(None)
}

fn backend_for(ctx: &HistorySyncCtx, id: ConversationId) -> Result<Arc<dyn SocialBackend>> {
    let kind = ctx
        .db
        .read(|r| repos::conversations::get(r, id))?
        .map(|c| c.conversation.kind);
    Ok(match (kind, &ctx.bot) {
        _ if ctx.backend.mode() == litecord_types::capability::BackendMode::UserSession => {
            ctx.backend.clone()
        }
        (Some(ConversationKind::GuildChannel), Some(bot)) => bot.clone(),
        _ => ctx.backend.clone(),
    })
}

/// Fetch and ingest one page for the next pending conversation. Returns how
/// long to wait before the next step.
pub(crate) async fn sync_step(ctx: &HistorySyncCtx) -> Result<Duration> {
    let idle = Duration::from_secs(5);
    let Some(rec) = ctx.db.read(|r| repos::history_sync::next_pending(r))? else {
        return Ok(idle);
    };
    if let Some(reason) = pause_reason(ctx)? {
        set_paused(ctx, &rec, Some(&reason))?;
        return Ok(Duration::from_secs(2));
    }
    set_paused(ctx, &rec, None)?;
    let backend = backend_for(ctx, rec.conversation_id)?;
    let sink = match backend.session_generation() {
        Some((generation, epoch)) => ctx.ingest.with_session_guard(generation, epoch),
        None => ctx.ingest.clone(),
    };
    let limit = ctx.cfg.hydration.history_page_size.clamp(1, 100);
    let req = match rec.oldest_message_id {
        Some(before) => HistoryPageRequest::before(rec.conversation_id, before, limit),
        None => HistoryPageRequest::latest(rec.conversation_id, limit),
    };
    match backend.history_page(&req).await {
        Ok(page) => {
            let done = !page.has_more || page.messages.is_empty();
            let source = backend.source();
            let now = ctx.db.now();
            send_page(&sink, source, now, rec.conversation_id, page.messages).await?;
            if done {
                // An empty page marks the conversation complete (same
                // reducer transaction as the checkpoint).
                send_page(&sink, source, now, rec.conversation_id, Vec::new()).await?;
            }
            Ok(Duration::from_millis(
                ctx.cfg.hydration.history_page_interval_ms,
            ))
        }
        Err(BackendError::RateLimited { retry_after }) => {
            set_paused(ctx, &rec, Some("Discord asked to slow down"))?;
            Ok(Duration::from_millis(retry_after.as_millis().max(1_000)))
        }
        Err(e @ (BackendError::Unsupported { .. } | BackendError::PermissionDenied { .. })) => {
            // This source can't page history: stop instead of spinning.
            let msg = format!("this source can't load older history ({e})");
            ctx.db.write(|tx| -> litecord_store::StoreResult<()> {
                repos::history_sync::set_error(tx, rec.conversation_id, Some(&msg))?;
                repos::history_sync::disable(tx, rec.conversation_id)?;
                Ok(())
            })?;
            Ok(Duration::from_millis(100))
        }
        Err(e) => {
            ctx.db.write(|tx| {
                repos::history_sync::set_error(tx, rec.conversation_id, Some(&e.to_string()))
            })?;
            Ok(Duration::from_secs(30))
        }
    }
}

async fn send_page(
    sink: &IngestSender,
    source: litecord_types::provenance::DiscordSource,
    now: Timestamp,
    conversation_id: ConversationId,
    messages: Vec<litecord_types::social::Message>,
) -> Result<()> {
    sink.send_committed(SourceEnvelope::new(
        source,
        now,
        DiscordEvent::MessagesPage {
            conversation_id,
            messages,
        },
    ))
    .await
    .map(|_| ())
    .map_err(|_| {
        Error::new(
            ErrorKind::Storage,
            "history page did not commit; checkpoint retained",
        )
    })
}

fn set_paused(ctx: &HistorySyncCtx, rec: &HistorySyncRecord, reason: Option<&str>) -> Result<()> {
    if rec.paused_reason.as_deref() != reason {
        ctx.db
            .write(|tx| repos::history_sync::set_paused(tx, rec.conversation_id, reason))?;
    }
    Ok(())
}

pub(crate) async fn history_sync_task(ctx: HistorySyncCtx, token: CancellationToken) -> Result<()> {
    if quota_bytes(&ctx.cfg) == 0 {
        return Ok(());
    }
    loop {
        let wait = match sync_step(&ctx).await {
            Ok(w) => w,
            Err(e) if e.kind() == ErrorKind::Shutdown => break,
            Err(e) => {
                tracing::warn!(error = %e, "history sync step failed");
                Duration::from_secs(30)
            }
        };
        tokio::select! {
            _ = token.cancelled() => break,
            _ = tokio::time::sleep(wait) => {}
        }
    }
    Ok(())
}
