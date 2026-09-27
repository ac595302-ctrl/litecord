//! The canonical state reducer.
//!
//! This module is the **single canonical write path** from a
//! [`SourceEnvelope`] (a normalized [`DiscordEvent`] plus provenance) to
//! Litecord's canonical tables. Nothing else in the codebase is allowed to
//! write `users`, `messages`, `guilds`, etc. directly: the ingest queue feeds
//! every observed Discord event through [`apply`] (or, for tests and
//! synchronous callers, [`reduce`] directly against an open [`WriteTx`]),
//! which:
//!
//! * runs inside one [`Database::write`] transaction, so the whole event
//!   either advances the global revision by exactly one or (if it turned out
//!   to be a pure re-observation) is a no-op;
//! * delegates every actual write to a `repos::*` function, which owns the
//!   upsert semantics and decides whether a [`UnifiedEvent`] is warranted;
//! * returns [`Followup`] work items — hydration requests, memory
//!   extraction, session broadcasts — for the caller to schedule *after* the
//!   transaction has committed.
//!
//! Hydration results flow back through the same [`DiscordEvent`] vocabulary
//! (e.g. as `*Snapshot` variants), so they are reduced exactly the same way
//! as live gateway/SDK events.

use litecord_core::events::{DiscordEvent, HydrationKey, SourceEnvelope, UnifiedEvent};
use litecord_types::provenance::DiscordIdentity;
use litecord_types::social::SessionState;
use litecord_types::{ConversationId, DurationMs, MessageId};

use crate::db::{Committed, Database, WriteTx};
use crate::error::StoreResult;
use crate::repos::{
    accounts, app_state, channels, conversations, guilds, history_sync, lobbies, messages,
    relationships, users, voice,
};

/// History pages only request memory extraction for messages sent within
/// this window before the envelope's `observed_at`: backfilled history must
/// not flood memory with stale pending replies.
pub const HISTORY_EXTRACTION_WINDOW: DurationMs = DurationMs::from_days(7);

// Note: `sync_state`/`hydration_jobs` are deliberately not written here. The
// reducer only ever produces `Followup::Hydrate` requests; the hydrator owns
// turning those into `sync_state`/`hydration_jobs` bookkeeping once it acts
// on them. The one exception is `history_sync`: a `MessagesPage` advances
// that conversation's backfill cursor in the same transaction as the page,
// so a page and its checkpoint commit (or roll back) together.

/// Reducer-wide behavior knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReducerConfig {
    /// Whether `MessageDeleted` also clears message content/extras
    /// (`true`), or only flips the `deleted` flag, keeping content around
    /// for local history (`false`).
    pub purge_deleted_messages: bool,
}

impl Default for ReducerConfig {
    fn default() -> Self {
        Self {
            purge_deleted_messages: true,
        }
    }
}

/// Work the reducer discovered while applying one event, to be scheduled by
/// the caller once the transaction has committed.
#[derive(Debug, Clone, PartialEq)]
pub enum Followup {
    /// Something should be (re)fetched from the backend.
    Hydrate(HydrationKey),
    /// A newly created message is a candidate for memory extraction.
    ExtractMemory {
        message_id: MessageId,
        conversation_id: ConversationId,
    },
    /// The session lifecycle state changed.
    SessionChanged(SessionState),
}

/// `app_state` key holding the last session state of an identity.
pub fn session_state_key(identity: DiscordIdentity) -> &'static str {
    match identity {
        DiscordIdentity::UserSocialSdk => "session_state",
        DiscordIdentity::UserSession => "user_session_state",
        DiscordIdentity::ApplicationBot => "bot_session_state",
    }
}

/// Everything [`reduce`] learned while applying one event.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReduceOutcome {
    pub followups: Vec<Followup>,
}

/// Apply one normalized Discord event to canonical state inside an already
/// open write transaction. See the module docs for the contract.
pub fn reduce(
    tx: &WriteTx<'_>,
    env: &SourceEnvelope,
    cfg: &ReducerConfig,
) -> StoreResult<ReduceOutcome> {
    let origin = env.source.origin();
    let observed_at = env.observed_at;
    let mut followups = Vec::new();

    match &env.event {
        DiscordEvent::SessionChanged { state } => {
            if env.source == litecord_types::provenance::DiscordSource::UserSession
                && *state == SessionState::Ready
            {
                crate::repos::account_catchup::reconnect(tx)?;
            }
            let json = serde_json::to_string(state)?;
            // User and bot sessions are tracked separately: a bot outage
            // must never look like the user being signed out.
            let key = session_state_key(env.source.identity());
            if app_state::set(tx, key, &json)? {
                tx.emit(UnifiedEvent::SessionChanged, origin)?;
            }
            followups.push(Followup::SessionChanged(state.clone()));
        }

        DiscordEvent::CurrentUser { user } => {
            let changed = users::upsert(tx, user, origin, observed_at)?;
            accounts::upsert_current(tx, user.id, env.source.identity(), origin)?;
            if changed {
                tx.emit(
                    UnifiedEvent::CurrentUserChanged { user_id: user.id },
                    origin,
                )?;
            }
        }

        DiscordEvent::UserUpserted { user } => {
            users::upsert(tx, user, origin, observed_at)?;
        }

        DiscordEvent::PresenceChanged { user_id, presence } => {
            let created_stub = users::ensure_stub(tx, *user_id, origin, observed_at)?;
            users::set_presence(tx, *user_id, presence, origin, observed_at)?;
            if created_stub {
                followups.push(Followup::Hydrate(HydrationKey::User { user_id: *user_id }));
            }
        }

        DiscordEvent::RelationshipUpserted { relationship, user } => {
            match user {
                Some(u) => {
                    users::upsert(tx, u, origin, observed_at)?;
                }
                None => {
                    let created =
                        users::ensure_stub(tx, relationship.user_id, origin, observed_at)?;
                    if created {
                        followups.push(Followup::Hydrate(HydrationKey::User {
                            user_id: relationship.user_id,
                        }));
                    }
                }
            }
            relationships::upsert(tx, relationship, origin, observed_at)?;
            if env.source == litecord_types::provenance::DiscordSource::UserSession {
                crate::repos::outbound::observe_relationships(tx)?;
            }
        }

        DiscordEvent::RelationshipRemoved { user_id } => {
            relationships::remove(tx, *user_id, origin)?;
            if env.source == litecord_types::provenance::DiscordSource::UserSession {
                crate::repos::outbound::observe_relationships(tx)?;
            }
        }

        DiscordEvent::GuildUpserted { guild } => {
            guilds::upsert_from_source(tx, guild, origin, origin.as_str(), observed_at)?;
        }

        DiscordEvent::GuildRemoved { guild_id } => {
            guilds::mark_departed_from_source(tx, *guild_id, origin, origin.as_str(), observed_at)?;
        }

        DiscordEvent::ChannelUpserted { channel } => {
            channels::upsert_from_source(tx, channel, origin, origin.as_str(), observed_at)?;
            if env.source == litecord_types::provenance::DiscordSource::UserSession {
                account_channel_conversation(tx, channel, origin, observed_at)?;
            }
        }

        DiscordEvent::ConversationUpserted { conversation } => {
            conversations::upsert(tx, conversation, origin, observed_at)?;
        }

        DiscordEvent::MessageWriteObserved {
            message,
            nonce,
            imported,
        } => {
            if env.source != litecord_types::provenance::DiscordSource::UserSession {
                return Err(crate::error::StoreError::Invariant(
                    "account receipt requires account source".into(),
                ));
            }
            apply_message(
                tx,
                message,
                origin,
                observed_at,
                *imported,
                true,
                &mut followups,
            )?;
            crate::repos::outbound::observe_send(tx, nonce, message)?;
        }
        DiscordEvent::MessageCreated { message } | DiscordEvent::MessageUpdated { message } => {
            let created_conversation_stub = apply_message(
                tx,
                message,
                origin,
                observed_at,
                false,
                true,
                &mut followups,
            )?;
            if env.source == litecord_types::provenance::DiscordSource::UserSession {
                crate::repos::outbound::observe_edit(tx, message)?;
            }
            if created_conversation_stub {
                followups.push(Followup::Hydrate(HydrationKey::DmSummaries));
            }
        }

        DiscordEvent::MessageDeleted {
            message_id,
            conversation_id,
        } => {
            if env.source == litecord_types::provenance::DiscordSource::UserSession {
                crate::repos::outbound::observe_delete(tx, *message_id, *conversation_id)?;
            }
            messages::mark_deleted(
                tx,
                *message_id,
                *conversation_id,
                cfg.purge_deleted_messages,
                origin,
            )?;
        }

        DiscordEvent::LobbyUpserted { lobby } => {
            lobbies::upsert(tx, lobby, origin, observed_at)?;
        }

        DiscordEvent::VoiceStateChanged { voice: voice_state } => {
            voice::set(tx, voice_state, origin, observed_at)?;
        }

        DiscordEvent::RelationshipsSnapshot { entries } => {
            relationships::replace_all(tx, entries, origin, observed_at)?;
            if env.source == litecord_types::provenance::DiscordSource::UserSession {
                crate::repos::outbound::observe_relationships(tx)?;
            }
        }

        DiscordEvent::GuildsSnapshot { guilds: list } => {
            guilds::replace_all_from_source(tx, list, origin, origin.as_str(), observed_at)?;
        }

        DiscordEvent::GuildChannelsSnapshot {
            guild_id,
            channels: list,
        } => {
            channels::replace_for_guild_from_source(
                tx,
                *guild_id,
                list,
                origin,
                origin.as_str(),
                observed_at,
            )?;
            if env.source == litecord_types::provenance::DiscordSource::UserSession {
                for channel in list {
                    account_channel_conversation(tx, channel, origin, observed_at)?;
                }
            }
        }

        DiscordEvent::ConversationsSnapshot {
            conversations: list,
        } => {
            for c in list {
                conversations::upsert(tx, c, origin, observed_at)?;
            }
        }

        DiscordEvent::MessagesSnapshot {
            conversation_id,
            messages: list,
        } => {
            let mut conversation_stub_created = false;
            for m in list {
                if env.source == litecord_types::provenance::DiscordSource::UserSession {
                    crate::repos::outbound::observe_edit(tx, m)?;
                }
                conversation_stub_created |=
                    apply_message(tx, m, origin, observed_at, true, true, &mut followups)?;
            }
            if conversation_stub_created {
                followups.push(Followup::Hydrate(HydrationKey::DmSummaries));
            }
            if env.source == litecord_types::provenance::DiscordSource::UserSession {
                crate::repos::account_catchup::track(
                    tx,
                    *conversation_id,
                    list.iter().map(|m| m.id).max(),
                )?;
            }
        }

        DiscordEvent::MessagesCatchupPage {
            conversation_id,
            messages: list,
            has_more,
        } => {
            if env.source != litecord_types::provenance::DiscordSource::UserSession {
                return Err(crate::error::StoreError::Invariant(
                    "account catch-up requires the account source".into(),
                ));
            }
            let extract_since = observed_at.saturating_sub(HISTORY_EXTRACTION_WINDOW);
            for message in list {
                crate::repos::outbound::observe_edit(tx, message)?;
                if message.conversation_id != *conversation_id {
                    return Err(crate::error::StoreError::Invariant(
                        "catch-up channel mismatch".into(),
                    ));
                }
                apply_message(
                    tx,
                    message,
                    origin,
                    observed_at,
                    true,
                    message.sent_at >= extract_since,
                    &mut followups,
                )?;
            }
            crate::repos::account_catchup::track(tx, *conversation_id, None)?;
            crate::repos::account_catchup::advance(
                tx,
                *conversation_id,
                list.iter().map(|m| m.id).max(),
                *has_more,
            )?;
        }

        DiscordEvent::MessagesPage {
            conversation_id,
            messages: list,
        } => {
            // Upsert only: a page never deletes what it does not contain.
            let extract_since = observed_at.saturating_sub(HISTORY_EXTRACTION_WINDOW);
            let mut conversation_stub_created = false;
            for m in list {
                let extract = m.sent_at >= extract_since;
                if env.source == litecord_types::provenance::DiscordSource::UserSession {
                    crate::repos::outbound::observe_edit(tx, m)?;
                }
                conversation_stub_created |=
                    apply_message(tx, m, origin, observed_at, true, extract, &mut followups)?;
            }
            if conversation_stub_created {
                followups.push(Followup::Hydrate(HydrationKey::DmSummaries));
            }
            let page_oldest = list
                .iter()
                .filter(|m| m.conversation_id == *conversation_id)
                .map(|m| m.id)
                .min();
            history_sync::advance(tx, *conversation_id, page_oldest, list.len())?;
        }

        DiscordEvent::Invalidated { key } => {
            followups.push(Followup::Hydrate(*key));
        }
    }

    Ok(ReduceOutcome {
        followups: dedupe(followups),
    })
}

/// Shared by `MessageCreated`/`MessageUpdated`, `MessagesSnapshot` and
/// `MessagesPage`. Returns whether a conversation stub was created (so the
/// batch variants can raise a single `Hydrate(DmSummaries)`). A newly
/// created message is queued for memory extraction when `extract` is set;
/// historical insertions carry a distinct event from live creations.
fn apply_message(
    tx: &WriteTx<'_>,
    message: &litecord_types::social::Message,
    origin: litecord_types::provenance::Origin,
    observed_at: litecord_types::Timestamp,
    historical: bool,
    extract: bool,
    followups: &mut Vec<Followup>,
) -> StoreResult<bool> {
    let outcome = if historical {
        messages::upsert_historical(tx, message, origin, observed_at)?
    } else {
        messages::upsert(tx, message, origin, observed_at)?
    };
    if outcome.created_author_stub {
        followups.push(Followup::Hydrate(HydrationKey::User {
            user_id: message.author_id,
        }));
    }
    if extract && matches!(outcome.write, messages::MessageWrite::Created) {
        followups.push(Followup::ExtractMemory {
            message_id: message.id,
            conversation_id: message.conversation_id,
        });
    }
    Ok(outcome.created_conversation_stub)
}

fn account_channel_conversation(
    tx: &WriteTx<'_>,
    channel: &litecord_types::social::Channel,
    origin: litecord_types::provenance::Origin,
    observed_at: litecord_types::Timestamp,
) -> StoreResult<()> {
    use litecord_types::social::{ChannelCapabilities, Conversation, ConversationKind};
    if channel.capabilities.contains(ChannelCapabilities::READABLE) {
        conversations::upsert(
            tx,
            &Conversation {
                id: ConversationId(channel.id.get()),
                kind: ConversationKind::GuildChannel,
                recipient_id: None,
                guild_id: Some(channel.guild_id),
                lobby_id: None,
                title: Some(channel.name.clone()),
                last_message_id: None,
                last_activity_at: None,
            },
            origin,
            observed_at,
        )?;
    }
    Ok(())
}

fn dedupe(followups: Vec<Followup>) -> Vec<Followup> {
    let mut out: Vec<Followup> = Vec::with_capacity(followups.len());
    for f in followups {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

/// Apply one event in its own committed write transaction. See the module
/// docs.
pub fn apply(
    db: &Database,
    env: &SourceEnvelope,
    cfg: &ReducerConfig,
) -> StoreResult<Committed<ReduceOutcome>> {
    let _span = tracing::info_span!("reduce", kind = env.event.kind()).entered();
    db.write(|tx| reduce(tx, env, cfg))
}
