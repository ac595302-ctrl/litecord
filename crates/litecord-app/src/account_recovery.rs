//! Bounded forward recovery for conversations already cached by the account.
//! Its REST watermark is separate from live arrival order, so dropped events
//! cannot be hidden by later live messages with higher IDs.
use crate::history::HistorySyncCtx;
use litecord_core::events::{DiscordEvent, SourceEnvelope};
use litecord_core::ports::{BackendError, HistoryPageRequest};
use litecord_core::{Error, ErrorKind, Result};
use litecord_store::repos;
use litecord_types::provenance::{DiscordIdentity, DiscordSource};
use litecord_types::social::SessionState;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(crate) async fn step(ctx: &HistorySyncCtx) -> Result<Duration> {
    let idle = Duration::from_secs(2);
    let ready = ctx
        .db
        .read(|r| {
            repos::app_state::get(
                r,
                litecord_store::reducer::session_state_key(DiscordIdentity::UserSession),
            )
        })?
        .and_then(|s| serde_json::from_str::<SessionState>(&s).ok())
        == Some(SessionState::Ready);
    if !ready || crate::history::pause_reason(ctx)?.is_some() {
        return Ok(idle);
    }
    let Some(cursor) = ctx
        .db
        .read(|r| repos::account_catchup::next_due(r, ctx.db.now()))?
    else {
        return Ok(idle);
    };
    let sink = match ctx.backend.session_generation() {
        Some((generation, epoch)) => ctx.ingest.with_session_guard(generation, epoch),
        None => return Ok(idle),
    };
    let limit = ctx.cfg.hydration.history_page_size.clamp(1, 100);
    let req = match cursor.newest_message_id {
        Some(after) => HistoryPageRequest::after(cursor.conversation_id, after, limit),
        None => HistoryPageRequest::latest(cursor.conversation_id, limit),
    };
    match ctx.backend.history_page(&req).await {
        Ok(page) => {
            let more =
                cursor.newest_message_id.is_some() && page.has_more && !page.messages.is_empty();
            sink.send_committed(SourceEnvelope::new(
                DiscordSource::UserSession,
                ctx.db.now(),
                DiscordEvent::MessagesCatchupPage {
                    conversation_id: cursor.conversation_id,
                    messages: page.messages,
                    has_more: more,
                },
            ))
            .await
            .map_err(|_| Error::new(ErrorKind::Storage, "account recovery page did not commit"))?;
            Ok(Duration::from_millis(
                ctx.cfg.hydration.history_page_interval_ms.max(250),
            ))
        }
        Err(error) => {
            let (ms, reason) = match error {
                BackendError::RateLimited { retry_after } => (
                    retry_after.as_millis().max(1000),
                    "Discord asked to slow down",
                ),
                BackendError::PermissionDenied { .. } | BackendError::NotFound { .. } => {
                    (3_600_000, "conversation is unavailable to this account")
                }
                BackendError::NotConnected => (1_000, "account is disconnected"),
                _ => (30_000, "account history request failed"),
            };
            ctx.db.write(|tx| {
                repos::account_catchup::retry(tx, cursor.conversation_id, ms, reason)
            })?;
            Ok(Duration::from_millis(250))
        }
    }
}

pub(crate) async fn run(ctx: HistorySyncCtx, token: CancellationToken) -> Result<()> {
    loop {
        let wait = tokio::select! {
            _=token.cancelled()=>break,
            result=step(&ctx)=>match result {Ok(wait)=>wait,Err(error)=>{tracing::warn!(error=%error,"account recovery deferred");Duration::from_secs(30)}}
        };
        tokio::select! {_=token.cancelled()=>break,_=tokio::time::sleep(wait)=>{}}
    }
    Ok(())
}
