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
use litecord_types::social::SessionState;
use litecord_types::{ConversationId, MessageId};

use crate::db::{Committed, Database, WriteTx};
use crate::error::StoreResult;
use crate::repos::{
    accounts, app_state, channels, conversations, guilds, lobbies, messages, relationships, users,
    voice,
};

// Note: `sync_state`/`hydration_jobs` are deliberately not written here. The
// reducer only ever produces `Followup::Hydrate` requests; the hydrator owns
// turning those into `sync_state`/`hydration_jobs` bookkeeping once it acts
// on them.

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
            let json = serde_json::to_string(state)?;
            if app_state::set(tx, "session_state", &json)? {
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
        }

        DiscordEvent::RelationshipRemoved { user_id } => {
            relationships::remove(tx, *user_id, origin)?;
        }

        DiscordEvent::GuildUpserted { guild } => {
            guilds::upsert(tx, guild, origin, observed_at)?;
        }

        DiscordEvent::GuildRemoved { guild_id } => {
            guilds::mark_departed(tx, *guild_id, origin, observed_at)?;
        }

        DiscordEvent::ChannelUpserted { channel } => {
            channels::upsert(tx, channel, origin, observed_at)?;
        }

        DiscordEvent::ConversationUpserted { conversation } => {
            conversations::upsert(tx, conversation, origin, observed_at)?;
        }

        DiscordEvent::MessageCreated { message } | DiscordEvent::MessageUpdated { message } => {
            let created_conversation_stub =
                apply_message(tx, message, origin, observed_at, &mut followups)?;
            if created_conversation_stub {
                followups.push(Followup::Hydrate(HydrationKey::DmSummaries));
            }
        }

        DiscordEvent::MessageDeleted {
            message_id,
            conversation_id,
        } => {
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
        }

        DiscordEvent::GuildsSnapshot { guilds: list } => {
            guilds::replace_all(tx, list, origin, observed_at)?;
        }

        DiscordEvent::GuildChannelsSnapshot {
            guild_id,
            channels: list,
        } => {
            channels::replace_for_guild(tx, *guild_id, list, origin, observed_at)?;
        }

        DiscordEvent::ConversationsSnapshot {
            conversations: list,
        } => {
            for c in list {
                conversations::upsert(tx, c, origin, observed_at)?;
            }
        }

        DiscordEvent::MessagesSnapshot { messages: list, .. } => {
            let mut conversation_stub_created = false;
            for m in list {
                conversation_stub_created |=
                    apply_message(tx, m, origin, observed_at, &mut followups)?;
            }
            if conversation_stub_created {
                followups.push(Followup::Hydrate(HydrationKey::DmSummaries));
            }
        }

        DiscordEvent::Invalidated { key } => {
            followups.push(Followup::Hydrate(*key));
        }
    }

    Ok(ReduceOutcome {
        followups: dedupe(followups),
    })
}

/// Shared by `MessageCreated`/`MessageUpdated` and `MessagesSnapshot`.
/// Returns whether a conversation stub was created (so `MessagesSnapshot`
/// can raise a single `Hydrate(DmSummaries)` for the whole batch).
fn apply_message(
    tx: &WriteTx<'_>,
    message: &litecord_types::social::Message,
    origin: litecord_types::provenance::Origin,
    observed_at: litecord_types::Timestamp,
    followups: &mut Vec<Followup>,
) -> StoreResult<bool> {
    let outcome = messages::upsert(tx, message, origin, observed_at)?;
    if outcome.created_author_stub {
        followups.push(Followup::Hydrate(HydrationKey::User {
            user_id: message.author_id,
        }));
    }
    if matches!(outcome.write, messages::MessageWrite::Created) {
        followups.push(Followup::ExtractMemory {
            message_id: message.id,
            conversation_id: message.conversation_id,
        });
    }
    Ok(outcome.created_conversation_stub)
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
