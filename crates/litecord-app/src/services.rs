//! Application services: the operations a UI invokes.
//!
//! Reads return view models (see [`crate::view`]). Writes go through the
//! proper channel: Discord writes via the Action Engine (the user acting in
//! the UI is `Actor::User`, which executes immediately and is audited),
//! approvals of agent proposals via `approve_action`, local state via the
//! memory/task services or settings. Nothing here exposes the SDK.

use std::collections::BTreeMap;

use litecord_actions::{ExecutionOutcome, ProposeOutcome};
use litecord_core::events::HydrationKey;
use litecord_core::ports::VoiceControl;
use litecord_core::{Error, ErrorKind, Result};
use litecord_features::command::CommandContext;
use litecord_features::feature::{FeatureContext, MessageActionContext, RenderMessage};
use litecord_features::intent::{AppIntent, VoiceIntent};
use litecord_hydrator::{HydrationReason, HydrationRequest, Priority};
use litecord_store::repos::{self, Connection};
use litecord_types::actions::*;
use litecord_types::capability::{Capability, DiscordTarget};
use litecord_types::entity::EntityId;
use litecord_types::ids::*;
use litecord_types::memory::{MemoryKind, MemoryPayload, MemoryStatus};
use litecord_types::notes::UserNote;
use litecord_types::provenance::{DiscordIdentity, Origin};
use litecord_types::social::*;
use litecord_types::tasks::{DraftStatus, ReminderStatus, TaskStatus};
use litecord_types::trust::AgentVisibility;
use litecord_types::{DurationMs, Revision};

use crate::app::LitecordApp;
use crate::view::*;

const PREVIEW_CHARS: usize = 120;

fn preview(s: &str) -> String {
    let mut out: String = s.chars().take(PREVIEW_CHARS).collect();
    if s.chars().count() > PREVIEW_CHARS {
        out.push('…');
    }
    out
}

fn me(conn: &Connection) -> Result<Option<UserId>> {
    Ok(repos::accounts::current(conn, DiscordIdentity::UserSocialSdk)?.map(|a| a.user_id))
}

fn name_of(conn: &Connection, id: UserId) -> Result<String> {
    Ok(repos::users::get(conn, id)?
        .filter(|u| !u.is_stub)
        .map(|u| u.user.display_name().to_owned())
        .unwrap_or_else(|| format!("user {id}")))
}

fn title_of(conn: &Connection, c: &Conversation) -> Result<String> {
    if let Some(t) = &c.title {
        return Ok(t.to_string());
    }
    match c.recipient_id {
        Some(r) => name_of(conn, r),
        None => Ok(format!("Conversation {}", c.id)),
    }
}

fn action_err(e: litecord_actions::ActionError) -> Error {
    e.into()
}

impl LitecordApp {
    fn feature_context(&self, conn: &Connection) -> Result<FeatureContext> {
        let settings: BTreeMap<String, serde_json::Value> =
            repos::settings::all(conn)?.into_iter().collect();
        Ok(FeatureContext { settings })
    }

    fn revision(&self) -> Result<Revision> {
        Ok(self.inner.db.current_revision()?)
    }

    // ------------------------------------------------------------------
    // Views
    // ------------------------------------------------------------------

    pub fn friends_view(&self) -> Result<FriendsViewModel> {
        self.inner.db.read(|r| -> Result<FriendsViewModel> {
            let mut vm = FriendsViewModel {
                as_of_revision: r.revision(),
                online: vec![],
                offline: vec![],
                pending_incoming: vec![],
                pending_outgoing: vec![],
                blocked: vec![],
            };
            for rec in repos::relationships::list(r, None)? {
                let rel = &rec.relationship;
                let note = repos::notes::get_note(r, rel.user_id)?;
                let dm = repos::conversations::find_dm_by_recipient(r, rel.user_id)?;
                let (display_name, username, avatar, presence, origin) = match &rec.user {
                    Some(u) => (
                        u.user.display_name().to_owned(),
                        u.user.username.to_string(),
                        u.user.avatar_url.as_ref().map(|a| a.to_string()),
                        u.presence.clone(),
                        u.origin,
                    ),
                    None => (
                        rel.user_id.to_string(),
                        String::new(),
                        None,
                        Presence::default(),
                        rec.origin,
                    ),
                };
                let kind = if rel.is_friend() {
                    RelationshipKind::Friend
                } else {
                    rel.discord
                };
                let row = FriendRow {
                    user_id: rel.user_id,
                    display_name,
                    username,
                    avatar_url: avatar,
                    status: presence.status,
                    activity: presence.activity,
                    relationship: kind,
                    alias: note.as_ref().and_then(|n| n.alias.clone()),
                    favorite: note.map(|n| n.favorite).unwrap_or(false),
                    dm_conversation_id: dm.map(|d| d.conversation.id),
                    origin,
                };
                match kind {
                    RelationshipKind::Friend => match row.status {
                        PresenceStatus::Online
                        | PresenceStatus::Idle
                        | PresenceStatus::DoNotDisturb => vm.online.push(row),
                        _ => vm.offline.push(row),
                    },
                    RelationshipKind::PendingIncoming => vm.pending_incoming.push(row),
                    RelationshipKind::PendingOutgoing => vm.pending_outgoing.push(row),
                    RelationshipKind::Blocked => vm.blocked.push(row),
                    _ => {}
                }
            }
            Ok(vm)
        })
    }

    pub fn conversations_view(&self, limit: u32) -> Result<ConversationListViewModel> {
        let default_vis = self.inner.cfg.agent.default_visibility;
        let since = self.inner.db.now().saturating_sub(DurationMs::from_days(7));
        self.inner
            .db
            .read(|r| -> Result<ConversationListViewModel> {
                let pending: Vec<ConversationId> = match me(r)? {
                    Some(me) => repos::messages::pending_replies(r, me, since, 50)?
                        .into_iter()
                        .map(|p| p.conversation_id)
                        .collect(),
                    None => vec![],
                };
                let mut rows = Vec::new();
                for c in repos::conversations::list_recent(r, limit, 0)? {
                    let conv = &c.conversation;
                    let last = match conv.last_message_id {
                        Some(id) => repos::messages::get(r, id)?.filter(|m| !m.deleted),
                        None => None,
                    };
                    let recipient_status = match conv.recipient_id {
                        Some(u) => repos::users::get(r, u)?.map(|u| u.presence.status),
                        None => None,
                    };
                    rows.push(ConversationRow {
                        conversation_id: conv.id,
                        kind: conv.kind,
                        title: title_of(r, conv)?,
                        recipient_id: conv.recipient_id,
                        recipient_status,
                        last_activity_at: conv.last_activity_at,
                        last_message_preview: last.map(|m| preview(&m.message.content)),
                        awaiting_reply: pending.contains(&conv.id),
                        agent_visibility: c.visibility.unwrap_or(default_vis),
                        origin: c.origin,
                    });
                }
                Ok(ConversationListViewModel {
                    as_of_revision: r.revision(),
                    conversations: rows,
                })
            })
    }

    /// Open a conversation: returns the latest window of messages and asks
    /// hydration to refresh it if stale (active screen priority).
    pub fn conversation_view(
        &self,
        id: ConversationId,
        limit: u32,
        before: Option<litecord_types::Timestamp>,
    ) -> Result<ConversationViewModel> {
        self.inner.hydrator.request_if_stale(
            HydrationKey::DmConversation {
                conversation_id: id,
            },
            Priority::High,
            HydrationReason::ActiveScreen,
        );
        let caps = self.inner.backend.capabilities();
        let default_vis = self.inner.cfg.agent.default_visibility;
        let features = self
            .inner
            .features
            .read()
            .map_err(|_| Error::internal("feature registry poisoned"))?;
        self.inner.db.read(|r| -> Result<ConversationViewModel> {
            let conv = repos::conversations::get(r, id)?
                .ok_or_else(|| Error::not_found(format!("conversation {id}")))?;
            let me = me(r)?;
            let ctx = self.feature_context(r)?;
            let limit = limit.clamp(1, 200);
            let mut recent = repos::messages::recent(r, id, limit + 1, before)?;
            let has_more = recent.len() > limit as usize;
            recent.truncate(limit as usize);
            recent.reverse();
            let mut messages = Vec::with_capacity(recent.len());
            for m in recent {
                let msg = &m.message;
                let mut render = RenderMessage {
                    message_id: msg.id,
                    author_id: msg.author_id,
                    author_display: name_of(r, msg.author_id)?,
                    content: msg.content.to_string(),
                    timestamp: msg.sent_at,
                    compact: false,
                    highlighted: false,
                    blurred: false,
                    badges: vec![],
                };
                features.render(&mut render, &ctx);
                let actions = features.message_actions(
                    &MessageActionContext {
                        message_id: msg.id,
                        conversation_id: id,
                        author_id: msg.author_id,
                        content: msg.content.to_string(),
                        guild_id: conv.conversation.guild_id,
                        authored_by_me: Some(msg.author_id) == me,
                    },
                    &ctx,
                );
                messages.push(MessageRow {
                    message_id: msg.id,
                    author_id: msg.author_id,
                    is_mine: Some(msg.author_id) == me,
                    sent_at: msg.sent_at,
                    edited: msg.edited_at.is_some(),
                    bookmarked: repos::notes::is_bookmarked(r, msg.id)?,
                    extras: msg.extras.clone(),
                    origin: m.origin,
                    render,
                    actions,
                });
            }
            let open_drafts = repos::drafts::list(r, Some(id), Some(&[DraftStatus::Open]), 10)?;
            Ok(ConversationViewModel {
                as_of_revision: r.revision(),
                conversation_id: id,
                title: title_of(r, &conv.conversation)?,
                messages,
                has_more,
                capabilities: ConversationCapabilities {
                    can_send: caps.is_usable(Capability::DmSend),
                    can_edit: caps.is_usable(Capability::DmEdit),
                    can_delete: caps.is_usable(Capability::DmDelete),
                    history: caps.dm_history.clone(),
                    open_in_discord_url: DiscordTarget::Conversation {
                        conversation_id: id,
                    }
                    .web_url(),
                },
                agent_visibility: conv.visibility.unwrap_or(default_vis),
                open_drafts,
            })
        })
    }

    /// Attachments shared in a conversation ("Files" tab), newest first.
    /// Only metadata the backend reported; content is not downloaded.
    pub fn conversation_files_view(
        &self,
        id: ConversationId,
        limit: u32,
        before: Option<litecord_types::Timestamp>,
    ) -> Result<FilesViewModel> {
        let limit = limit.clamp(1, 500);
        self.inner.db.read(|r| -> Result<FilesViewModel> {
            let mut records = repos::attachments::for_conversation(r, id, before, limit + 1)?;
            let has_more = records.len() > limit as usize;
            records.truncate(limit as usize);
            let mut files = Vec::with_capacity(records.len());
            for a in records {
                files.push(FileRow {
                    author_name: name_of(r, a.author_id)?,
                    open_in_discord_url: DiscordTarget::Message {
                        guild_id: None,
                        channel_id: ChannelId(id.get()),
                        message_id: a.message_id,
                    }
                    .web_url(),
                    message_id: a.message_id,
                    author_id: a.author_id,
                    sent_at: a.sent_at,
                    filename: a.filename,
                    content_type: a.content_type,
                    size_bytes: a.size_bytes,
                    origin: a.origin,
                });
            }
            Ok(FilesViewModel {
                as_of_revision: r.revision(),
                conversation_id: id,
                files,
                has_more,
            })
        })
    }

    pub fn agent_inbox_view(&self) -> Result<AgentInboxViewModel> {
        let now = self.inner.db.now();
        let since = now.saturating_sub(DurationMs::from_days(7));
        let proposals = self.inner.actions.pending(50).map_err(action_err)?;
        self.inner.db.read(|r| -> Result<AgentInboxViewModel> {
            let mut items = Vec::new();
            if let Some(me) = me(r)? {
                for p in repos::messages::pending_replies(r, me, since, 20)? {
                    items.push(InboxItem::PendingReply {
                        conversation_id: p.conversation_id,
                        from: name_of(r, p.last_message.message.author_id)?,
                        preview: preview(&p.last_message.message.content),
                        at: p.last_message.message.sent_at,
                    });
                }
            }
            for rem in repos::reminders::list(
                r,
                &repos::reminders::ReminderFilter {
                    statuses: Some(vec![ReminderStatus::Fired, ReminderStatus::Pending]),
                    due_before: Some(now.saturating_add(DurationMs::from_days(1))),
                    limit: 20,
                    ..Default::default()
                },
            )? {
                items.push(InboxItem::ReminderDue {
                    reminder_id: rem.id,
                    title: rem.title,
                    due_at: rem.trigger.due_at(),
                    conversation_id: rem.conversation_id,
                });
            }
            for t in repos::tasks::list(
                r,
                &repos::tasks::TaskFilter {
                    statuses: Some(vec![TaskStatus::Candidate]),
                    limit: 20,
                    ..Default::default()
                },
            )? {
                items.push(InboxItem::TaskCandidate {
                    task_id: t.id,
                    title: t.title,
                    origin: t.origin,
                });
            }
            for m in repos::memory::list(
                r,
                &repos::memory::MemoryFilter {
                    kinds: Some(vec![MemoryKind::Commitment]),
                    limit: 20,
                    ..Default::default()
                },
            )? {
                let due_at = match &m.payload {
                    Some(MemoryPayload::Commitment { due_at, .. }) => *due_at,
                    _ => None,
                };
                items.push(InboxItem::Commitment {
                    memory_id: m.id,
                    text: m.content.to_string(),
                    due_at,
                });
            }
            let mut pending_actions = Vec::new();
            for p in &proposals {
                pending_actions.push(pending_row(r, p)?);
            }
            Ok(AgentInboxViewModel {
                as_of_revision: r.revision(),
                needs_attention: items,
                pending_actions,
            })
        })
    }

    pub fn tasks_view(&self) -> Result<TasksViewModel> {
        self.inner.db.read(|r| -> Result<TasksViewModel> {
            let list = |s: TaskStatus| {
                repos::tasks::list(
                    r,
                    &repos::tasks::TaskFilter {
                        statuses: Some(vec![s]),
                        limit: 100,
                        ..Default::default()
                    },
                )
            };
            Ok(TasksViewModel {
                as_of_revision: r.revision(),
                open: list(TaskStatus::Open)?,
                candidates: list(TaskStatus::Candidate)?,
                reminders: repos::reminders::list(
                    r,
                    &repos::reminders::ReminderFilter {
                        statuses: Some(vec![ReminderStatus::Pending, ReminderStatus::Fired]),
                        limit: 100,
                        ..Default::default()
                    },
                )?,
            })
        })
    }

    /// Memory inspector. `entity = None` lists recent memories.
    pub fn memory_view(
        &self,
        entity: Option<EntityId>,
        include_history: bool,
    ) -> Result<MemoryViewModel> {
        self.inner.db.read(|r| -> Result<MemoryViewModel> {
            let statuses = if include_history {
                None
            } else {
                Some(vec![
                    MemoryStatus::Candidate,
                    MemoryStatus::Derived,
                    MemoryStatus::UserConfirmed,
                ])
            };
            let memories = repos::memory::list(
                r,
                &repos::memory::MemoryFilter {
                    statuses,
                    entity,
                    limit: 100,
                    ..Default::default()
                },
            )?;
            let edges = match &entity {
                Some(e) => repos::edges::neighbors(r, e, 100)?,
                None => vec![],
            };
            Ok(MemoryViewModel {
                as_of_revision: r.revision(),
                entity,
                memories,
                edges,
                counts_by_status: repos::memory::count_by_status(r)?,
            })
        })
    }

    pub fn voice_view(&self) -> Result<VoiceViewModel> {
        let caps = self.inner.backend.capabilities();
        self.inner.db.read(|r| -> Result<VoiceViewModel> {
            let state = repos::voice::get(r)?.unwrap_or_default();
            let mut names = Vec::new();
            for p in &state.participants {
                names.push((p.user_id, name_of(r, p.user_id)?));
            }
            Ok(VoiceViewModel {
                state,
                participant_names: names,
                voice_supported: caps.is_usable(Capability::Voice),
                devices_supported: caps.is_usable(Capability::VoiceDevices),
            })
        })
    }

    pub fn settings_view(&self) -> Result<SettingsViewModel> {
        let features = self
            .inner
            .features
            .read()
            .map_err(|_| Error::internal("feature registry poisoned"))?;
        let values: BTreeMap<String, serde_json::Value> = self
            .inner
            .db
            .read(|r| repos::settings::all(r))?
            .into_iter()
            .collect();
        let mut sections: BTreeMap<String, Vec<SettingRow>> = BTreeMap::new();
        for d in features.settings_schema() {
            let value = values
                .get(&d.key)
                .cloned()
                .unwrap_or_else(|| default_value(&d.kind));
            sections
                .entry(d.section.clone())
                .or_default()
                .push(SettingRow {
                    descriptor: d,
                    value,
                });
        }
        Ok(SettingsViewModel {
            sections: sections.into_iter().collect(),
            features: features.metadata(),
        })
    }

    pub fn guilds_view(&self) -> Result<GuildsViewModel> {
        self.inner.db.read(|r| -> Result<GuildsViewModel> {
            let mut guilds = Vec::new();
            for g in repos::guilds::list(r, false)? {
                let channels = repos::channels::list_for_guild(r, g.guild.id, false)?
                    .into_iter()
                    .map(|c| ChannelRow {
                        open_in_discord_url: DiscordTarget::Channel {
                            guild_id: c.channel.guild_id,
                            channel_id: c.channel.id,
                        }
                        .web_url(),
                        channel: c.channel,
                    })
                    .collect();
                guilds.push(GuildRow {
                    guild: g.guild,
                    channels,
                });
            }
            Ok(GuildsViewModel {
                as_of_revision: r.revision(),
                guilds,
            })
        })
    }

    pub fn diagnostics_view(&self) -> Result<DiagnosticsViewModel> {
        let (revision, session, counts) = self.inner.db.read(|r| -> Result<_> {
            let session = repos::app_state::get(r, "session_state")?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            let counts = StoreCounts {
                users: repos::users::count(r)?,
                messages: repos::messages::count(r, None)?,
                memories_by_status: repos::memory::count_by_status(r)?,
            };
            Ok((r.revision(), session, counts))
        })?;
        Ok(DiagnosticsViewModel {
            revision,
            session,
            backend_mode: self.inner.backend.mode(),
            capabilities: self.inner.backend.capabilities(),
            hydration_pending: self.inner.hydrator.pending_len(),
            hydration_active: self.inner.hydrator.active_len(),
            counts,
            metrics: self.inner.metrics.snapshot(),
        })
    }

    // ------------------------------------------------------------------
    // Command palette and intents
    // ------------------------------------------------------------------

    fn command_context(&self, scope: CommandScope) -> Result<CommandContext> {
        // A selected message only counts if it exists, is not deleted and
        // belongs to the active conversation (when one is given).
        let selected_message = match scope.selected_message {
            Some(id) => self
                .inner
                .db
                .read(|r| repos::messages::get(r, id))?
                .filter(|m| !m.deleted)
                .filter(|m| {
                    scope
                        .active_conversation
                        .is_none_or(|c| c == m.message.conversation_id)
                })
                .map(|m| m.message.id),
            None => None,
        };
        let settings = self
            .inner
            .db
            .read(|r| repos::settings::all(r))?
            .into_iter()
            .collect();
        Ok(CommandContext {
            capabilities: self.inner.backend.capabilities(),
            online: self
                .diagnostics_view()
                .map(|d| d.session.is_online())
                .unwrap_or(false),
            active_conversation: scope.active_conversation,
            selected_message,
            settings,
        })
    }

    pub fn command_palette(
        &self,
        query: &str,
        active: Option<ConversationId>,
    ) -> Result<CommandPaletteViewModel> {
        self.command_palette_in(
            query,
            CommandScope {
                active_conversation: active,
                selected_message: None,
            },
        )
    }

    /// Palette search with full focus (active conversation + selected
    /// message), enabling message-scoped commands.
    pub fn command_palette_in(
        &self,
        query: &str,
        scope: CommandScope,
    ) -> Result<CommandPaletteViewModel> {
        let ctx = self.command_context(scope)?;
        let commands = self
            .inner
            .commands
            .read()
            .map_err(|_| Error::internal("command registry poisoned"))?;
        Ok(CommandPaletteViewModel {
            query: query.to_owned(),
            matches: commands.search(query, &ctx, 20),
        })
    }

    /// All registered shortcuts, independent of palette query and result limit.
    pub fn command_shortcuts(
        &self,
        active: Option<ConversationId>,
    ) -> Result<Vec<litecord_features::command::CommandMatch>> {
        self.command_shortcuts_in(CommandScope {
            active_conversation: active,
            selected_message: None,
        })
    }

    /// Registered shortcuts evaluated against the full focus scope.
    pub fn command_shortcuts_in(
        &self,
        scope: CommandScope,
    ) -> Result<Vec<litecord_features::command::CommandMatch>> {
        let ctx = self.command_context(scope)?;
        let commands = self
            .inner
            .commands
            .read()
            .map_err(|_| Error::internal("command registry poisoned"))?;
        Ok(commands
            .search("", &ctx, usize::MAX)
            .into_iter()
            .filter(|command| command.shortcut.is_some())
            .collect())
    }

    pub async fn run_command(
        &self,
        id: &str,
        active: Option<ConversationId>,
    ) -> Result<Vec<UiEffect>> {
        self.run_command_in(
            id,
            CommandScope {
                active_conversation: active,
                selected_message: None,
            },
        )
        .await
    }

    /// Run a command against the full focus scope.
    pub async fn run_command_in(&self, id: &str, scope: CommandScope) -> Result<Vec<UiEffect>> {
        let ctx = self.command_context(scope)?;
        let intents = {
            let commands = self
                .inner
                .commands
                .read()
                .map_err(|_| Error::internal("command registry poisoned"))?;
            commands
                .execute(id, &ctx)
                .map_err(|e| Error::new(ErrorKind::Validation, e.to_string()))?
        };
        self.apply_intents(intents).await
    }

    /// Interpret feature/command intents. Features never mutate state
    /// themselves; this is where their requests are carried out.
    pub async fn apply_intents(&self, intents: Vec<AppIntent>) -> Result<Vec<UiEffect>> {
        let mut effects = Vec::new();
        for intent in intents {
            match intent {
                AppIntent::Navigate { target } => effects.push(UiEffect::Navigate { target }),
                AppIntent::CopyToClipboard { text } => {
                    effects.push(UiEffect::CopyToClipboard { text })
                }
                AppIntent::OpenExternal { target } => effects.push(UiEffect::OpenUrl {
                    url: target.web_url(),
                }),
                AppIntent::ShowNotice { message } => effects.push(UiEffect::Notice { message }),
                AppIntent::ToggleSetting { key } => {
                    let current = self
                        .inner
                        .db
                        .read(|r| repos::settings::get_json(r, &key))?
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    self.set_setting(&key, serde_json::Value::Bool(!current))?;
                }
                AppIntent::SetSetting { key, value } => self.set_setting(&key, value)?,
                AppIntent::Voice { intent } => {
                    let state = self.voice_view()?.state;
                    let control = match intent {
                        VoiceIntent::ToggleMute => VoiceControl::SetMuted(!state.muted),
                        VoiceIntent::ToggleDeafen => VoiceControl::SetDeafened(!state.deafened),
                        VoiceIntent::Leave => VoiceControl::Leave,
                    };
                    self.voice(control).await?;
                }
                AppIntent::BookmarkMessage { message_id } => {
                    self.user_action(AgentAction::BookmarkMessage {
                        message_id,
                        note: None,
                    })
                    .await?;
                }
                AppIntent::ProposeAction { action } => {
                    // Feature-originated actions are not the user's explicit
                    // input: System actor ⇒ Discord writes need approval.
                    let rev = self.revision()?;
                    self.inner
                        .actions
                        .propose(action, Actor::System, rev, None)
                        .await
                        .map_err(action_err)?;
                }
            }
        }
        Ok(effects)
    }

    // ------------------------------------------------------------------
    // User actions
    // ------------------------------------------------------------------

    async fn user_action(&self, action: AgentAction) -> Result<ProposeOutcome> {
        let rev = self.revision()?;
        self.inner
            .actions
            .propose(action, Actor::User, rev, None)
            .await
            .map_err(action_err)
    }

    /// Send a message typed by the user.
    pub async fn send_message(
        &self,
        conversation_id: ConversationId,
        content: &str,
    ) -> Result<ProposeOutcome> {
        self.user_action(AgentAction::SendMessage {
            target: MessageTarget::Conversation { conversation_id },
            content: content.to_owned(),
        })
        .await
    }

    pub async fn edit_message(
        &self,
        message_id: MessageId,
        content: &str,
    ) -> Result<ProposeOutcome> {
        self.user_action(AgentAction::EditMessage {
            message_id,
            content: content.to_owned(),
        })
        .await
    }

    pub async fn delete_message(&self, message_id: MessageId) -> Result<ProposeOutcome> {
        self.user_action(AgentAction::DeleteMessage { message_id })
            .await
    }

    /// Perform a user initiated relationship change through the Action Engine.
    pub async fn change_relationship(
        &self,
        user_id: UserId,
        action: RelationshipAction,
    ) -> Result<ProposeOutcome> {
        self.user_action(AgentAction::RelationshipChange { user_id, action })
            .await
    }

    /// The user pressed "Send"/"Approve" on a proposal (optionally after
    /// editing it). Approval and execution happen together, so the token
    /// never leaves this function.
    pub async fn approve_action(
        &self,
        id: ActionId,
        edited: Option<AgentAction>,
    ) -> Result<ExecutionOutcome> {
        let token = self.inner.actions.approve(id, edited).map_err(action_err)?;
        self.inner.actions.execute(&token).await.map_err(action_err)
    }

    pub fn reject_action(&self, id: ActionId) -> Result<()> {
        self.inner.actions.reject(id).map_err(action_err)
    }

    pub fn confirm_task(&self, id: TaskId) -> Result<()> {
        Ok(self.inner.tasks.confirm_candidate(id)?)
    }

    pub fn dismiss_task(&self, id: TaskId) -> Result<()> {
        Ok(self.inner.tasks.dismiss(id)?)
    }

    pub fn complete_task(&self, id: TaskId) -> Result<()> {
        self.inner.tasks.complete(id, Origin::UserProvided)?;
        Ok(())
    }

    pub fn confirm_memory(&self, id: MemoryId) -> Result<()> {
        self.inner.memory.confirm(id)?;
        Ok(())
    }

    pub fn reject_memory(&self, id: MemoryId) -> Result<()> {
        self.inner.memory.reject(id)?;
        Ok(())
    }

    /// Per-conversation agent visibility (`None` = use default).
    pub fn set_conversation_visibility(
        &self,
        id: ConversationId,
        v: Option<AgentVisibility>,
    ) -> Result<()> {
        self.inner
            .db
            .write(|tx| repos::conversations::set_visibility(tx, id, v))?;
        Ok(())
    }

    pub fn set_setting(&self, key: &str, value: serde_json::Value) -> Result<()> {
        if key == litecord_layout::SETTINGS_KEY {
            return Err(Error::validation("use the typed layout profile service"));
        }
        if let Some(id) = key
            .strip_prefix("features.")
            .and_then(|k| k.strip_suffix(".enabled"))
        {
            if let Some(enabled) = value.as_bool() {
                self.inner
                    .features
                    .write()
                    .map_err(|_| Error::internal("feature registry poisoned"))?
                    .set_enabled(id, enabled)
                    .map_err(|e| Error::validation(e.to_string()))?;
            }
        }
        self.inner
            .db
            .write(|tx| repos::settings::set_json(tx, key, &value))?;
        Ok(())
    }

    pub fn set_user_note(&self, note: UserNote) -> Result<()> {
        self.inner
            .db
            .write(|tx| repos::notes::upsert_note(tx, &note))?;
        Ok(())
    }

    /// Local voice controls (user-initiated).
    pub async fn voice(&self, control: VoiceControl) -> Result<VoiceState> {
        Ok(self.inner.backend.voice_control(control).await?)
    }

    /// Audio devices for the voice settings pickers. Returns an empty list
    /// (not an error) when the backend cannot enumerate devices, so the UI
    /// can simply hide the pickers; check `voice_view().devices_supported`.
    pub async fn audio_devices(&self) -> Result<Vec<AudioDevice>> {
        if !self
            .inner
            .backend
            .capabilities()
            .is_usable(Capability::VoiceDevices)
        {
            return Ok(Vec::new());
        }
        Ok(self.inner.backend.audio_devices().await?)
    }

    /// Start signing in. With `OpenBrowser`, the UI opens the URL (a user
    /// action), captures the redirect to `redirect_uri` and passes it to
    /// [`LitecordApp::complete_sign_in`]. Session progress arrives as
    /// `SessionChanged` events (Authorizing → Connecting → Ready).
    pub async fn sign_in(&self) -> Result<litecord_types::capability::AuthStep> {
        Ok(self.inner.backend.begin_sign_in().await?)
    }

    /// Finish signing in. Tokens stay inside the backend's secret store;
    /// nothing credential-bearing is returned or logged.
    pub async fn complete_sign_in(&self, redirect_url: &str) -> Result<()> {
        Ok(self.inner.backend.complete_sign_in(redirect_url).await?)
    }

    /// Sign out of Discord. Local memory is kept (it is the user's data);
    /// hydration pauses until the next sign-in.
    pub async fn sign_out(&self) -> Result<()> {
        Ok(self.inner.backend.sign_out().await?)
    }

    /// Current session state (as last reported by the backend).
    pub fn session_state(&self) -> Result<SessionState> {
        Ok(self
            .inner
            .db
            .read(|r| repos::app_state::get(r, "session_state"))?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default())
    }

    /// Explicit user refresh of some state.
    pub fn refresh(&self, key: HydrationKey) {
        self.inner
            .hydrator
            .request(HydrationRequest::high(key, HydrationReason::UserRefresh));
    }
}

fn default_value(kind: &litecord_features::feature::SettingKind) -> serde_json::Value {
    use litecord_features::feature::SettingKind as K;
    match kind {
        K::Bool { default } => serde_json::json!(default),
        K::Text { default } => serde_json::json!(default),
        K::StringList { default } => serde_json::json!(default),
        K::Number { default, .. } => serde_json::json!(default),
    }
}

fn pending_row(conn: &Connection, p: &ActionProposal) -> Result<PendingActionRow> {
    let (summary, target, content) = match &p.action {
        AgentAction::SendMessage { target, content } => {
            let label = match target {
                MessageTarget::User { user_id } => name_of(conn, *user_id)?,
                MessageTarget::Conversation { conversation_id } => {
                    match repos::conversations::get(conn, *conversation_id)? {
                        Some(c) => title_of(conn, &c.conversation)?,
                        None => conversation_id.to_string(),
                    }
                }
            };
            (
                format!("Send to {label}: {}", preview(content)),
                Some(label),
                Some(content.clone()),
            )
        }
        AgentAction::EditMessage { content, .. } => {
            ("Edit a message".to_owned(), None, Some(content.clone()))
        }
        AgentAction::DeleteMessage { .. } => ("Delete a message".to_owned(), None, None),
        AgentAction::ChangePresence { presence } => {
            (format!("Set status to {}", presence.status), None, None)
        }
        AgentAction::RelationshipChange { user_id, action } => {
            let label = name_of(conn, *user_id)?;
            (
                format!("{} {label}", action.as_str().replace('_', " ")),
                Some(label),
                None,
            )
        }
        other => (other.kind().replace('_', " "), None, None),
    };
    Ok(PendingActionRow {
        action_id: p.id,
        kind: p.action.kind(),
        class: p.class,
        status: p.status,
        summary,
        target_label: target,
        content,
        actor: p.actor.label(),
        rationale: p.rationale.clone(),
        created_at: p.created_at,
    })
}
