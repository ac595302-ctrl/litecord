//! Bounded commands, lossless completions, coalesced snapshots. No I/O on render.
use eframe::egui::Context;
use litecord_app::{view::*, workspace::LayoutProfilesViewModel, LitecordApp};
use litecord_core::ports::VoiceControl;
use litecord_features::intent::AppIntent;
use litecord_layout::{Destination, LayoutProfile};
use litecord_types::{ids::*, notes::UserNote, trust::AgentVisibility, Timestamp};
use tokio::sync::{broadcast, mpsc, watch};

#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub generation: u64,
    pub destination: Destination,
    pub conversation: Option<ConversationId>,
    pub contact: Option<UserId>,
    pub before: Option<Timestamp>,
    pub palette_query: String,
    /// Task shown in the task inspector (loads its detail view).
    pub task: Option<TaskId>,
    /// Omni session shown in the Omni panel (None = most recent chat).
    pub omni_session: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub selection: Selection,
    pub conversations: ConversationListViewModel,
    pub friends: FriendsViewModel,
    pub chat: Option<ConversationViewModel>,
    pub layouts: LayoutProfilesViewModel,
    pub diagnostics: DiagnosticsViewModel,
    pub inbox: AgentInboxViewModel,
    pub tasks: TasksViewModel,
    pub settings: SettingsViewModel,
    pub guilds: GuildsViewModel,
    pub voice: VoiceViewModel,
    pub rooms: litecord_app::rooms::RoomsViewModel,
    pub memory: MemoryViewModel,
    pub palette: CommandPaletteViewModel,
    pub shortcuts: Vec<litecord_features::command::CommandMatch>,
    pub account: litecord_app::people::AccountViewModel,
    pub contact: Option<litecord_app::people::ContactViewModel>,
    /// Shared files for the open conversation (application Files API).
    pub files: Option<FilesViewModel>,
    pub task_detail: Option<TaskDetailViewModel>,
    pub omni: litecord_app::OmniViewModel,
}

#[derive(Debug)]
pub enum Command {
    Edit(MessageId, String),
    Delete(MessageId),
    Intents(Vec<AppIntent>),
    Note(UserNote),
    Visibility(ConversationId, AgentVisibility),
    Setting(String, serde_json::Value),
    Voice(VoiceControl),
    Relationship(UserId, litecord_types::actions::RelationshipAction),
    CompleteTask(TaskId),
    ConfirmTask(TaskId),
    DismissTask(TaskId),
    ConfirmMemory(MemoryId),
    RejectMemory(MemoryId),
    Approve(ActionId),
    Reject(ActionId),
    SaveProfile(LayoutProfile, String),
    ActivateProfile(String, String),
    CreateProfile(String, String),
    DuplicateProfile(String, String, String),
    RenameProfile(String, String, String),
    DeleteProfile(String, String),
    ResetProfile(String, String),
    ResetAll(String),
    Run(String, Option<ConversationId>),
    /// Send with an explicitly displayed identity (user or application bot).
    SendAs(
        ConversationId,
        String,
        litecord_types::provenance::DiscordIdentity,
    ),
    CreateTask(litecord_types::tasks::TaskDraft),
    SetTaskPriority(TaskId, litecord_types::tasks::TaskPriority),
    AddTaskComment(TaskId, String),
    Omni(OmniCommand),
}

/// Omni panel and settings commands.
#[derive(Debug)]
pub enum OmniCommand {
    Send(Option<i64>, String),
    New(litecord_app::harness::OmniMode),
    Interrupt(i64),
    Compact(i64),
    Archive(i64),
    Answer(String, litecord_app::harness::Decision),
    Remember(i64, u32),
    Select(litecord_app::harness::HarnessKind),
    SignOut,
    Refresh,
    Heartbeats(bool),
    CheckNow,
    DismissCheckins,
    LoadLoginOptions,
    SignInWith(String),
    SubmitCode(String),
    /// Option id + key. `Secret` keeps the key out of Debug output.
    ApiKey(String, litecord_core::secrets::Secret<String>),
    LoadModels,
    SetModel(Option<String>),
    CreateAutomation(litecord_app::automations::AutomationDraft),
    SetAutomationEnabled(i64, bool),
    DeleteAutomation(i64),
    RunAutomation(i64),
}

#[derive(Debug, Default)]
pub struct Completion {
    pub effects: Vec<UiEffect>,
    pub sent: Option<(ConversationId, String)>,
    pub message_changed: Option<MessageId>,
    pub relationship_changed: Option<UserId>,
    /// Omni session to show after the command (e.g. a new chat).
    pub omni_session: Option<i64>,
    pub task_created: Option<TaskId>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct Bridge {
    pub commands: mpsc::Sender<Command>,
    pub completions: mpsc::Receiver<Completion>,
    pub snapshots: watch::Receiver<Result<Snapshot, String>>,
    pub selection: watch::Sender<Selection>,
    /// Live Omni reply text `(session, text so far)`; cleared when the turn
    /// completes. Updated per streamed delta without a full snapshot.
    pub omni_stream: watch::Receiver<Option<(i64, String)>>,
    worker: tokio::task::JoinHandle<()>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

impl Bridge {
    pub fn new(app: LitecordApp, runtime: tokio::runtime::Handle, ctx: Context) -> Self {
        let selection = Selection {
            generation: 0,
            destination: Destination::Messages,
            conversation: None,
            contact: None,
            before: None,
            palette_query: String::new(),
            task: None,
            omni_session: None,
        };
        let (selection_tx, mut selection_rx) = watch::channel(selection);
        let (command_tx, mut commands) = mpsc::channel(32);
        let (completion_tx, completions) = mpsc::channel(32);
        let (snapshot_tx, snapshots) = watch::channel(Err("Loading workspace…".into()));
        let mut events = app.subscribe();
        let mut omni_events = app.omni().subscribe();
        let (stream_tx, omni_stream) = watch::channel(None::<(i64, String)>);
        let worker = runtime.spawn(async move {
            let mut poll = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                let selected = selection_rx.borrow_and_update().clone();
                let app_copy = app.clone();
                let result = tokio::task::spawn_blocking(move || snapshot(&app_copy, selected)).await;
                let _ = snapshot_tx.send_replace(match result { Ok(r) => r.map_err(|e|e.to_string()), Err(_) => Err("Could not refresh workspace".into()) });
                ctx.request_repaint();
                tokio::select! {
                    biased;
                    command = commands.recv() => {
                        let Some(command) = command else {break};
                        let completion = match execute(&app,command).await {
                            Ok(c)=>c, Err(e)=>Completion{error:Some(e.to_string()),..Default::default()}
                        };
                        if completion_tx.send(completion).await.is_err() {break;}
                        ctx.request_repaint();
                    }
                    changed = selection_rx.changed() => if changed.is_err() {break;},
                    event = events.recv() => {
                        if matches!(event,Err(broadcast::error::RecvError::Closed)) {break;}
                        // Lag and ResyncRequired both trigger a full replacement snapshot.
                        tokio::time::sleep(std::time::Duration::from_millis(32)).await;
                        // Collapse an event burst before the next snapshot. A busy
                        // hydrator must not cause one full database read per event.
                        for _ in 0..256 {
                            if matches!(events.try_recv(),Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed)) {break;}
                        }
                    }
                    event = omni_events.recv() => match event {
                        Ok(litecord_app::OmniEvent::Delta { session_id, text }) => {
                            stream_tx.send_modify(|s| match s {
                                Some((id, buf)) if *id == session_id => buf.push_str(&text),
                                other => *other = Some((session_id, text)),
                            });
                            ctx.request_repaint();
                            // Deltas never trigger a full snapshot.
                            continue;
                        }
                        Ok(litecord_app::OmniEvent::Changed { .. }) => {
                            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
                            while omni_events.try_recv().is_ok() {}
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    _ = poll.tick() => {}
                }
            }
        });
        Self {
            commands: command_tx,
            completions,
            snapshots,
            selection: selection_tx,
            omni_stream,
            worker,
        }
    }
}

pub(crate) fn snapshot(
    app: &LitecordApp,
    mut selection: Selection,
) -> litecord_core::Result<Snapshot> {
    let conversations = app.conversations_view(200)?;
    if selection.conversation.is_none() {
        selection.conversation = conversations
            .conversations
            .first()
            .map(|r| r.conversation_id);
    }
    let chat = selection
        .conversation
        .map(|id| app.conversation_view(id, 200, selection.before))
        .transpose()?;
    let contact_id = selection.contact.or_else(|| {
        conversations
            .conversations
            .iter()
            .find(|r| Some(r.conversation_id) == selection.conversation)
            .and_then(|r| r.recipient_id)
    });
    Ok(Snapshot {
        friends: app.friends_view()?,
        layouts: app.layout_profiles_view()?,
        diagnostics: app.diagnostics_view()?,
        inbox: app.agent_inbox_view()?,
        tasks: app.tasks_view()?,
        settings: app.settings_view()?,
        guilds: app.guilds_view()?,
        voice: app.voice_view()?,
        rooms: app.rooms_view()?,
        memory: app.memory_view(None, false)?,
        account: app.account_view()?,
        palette: app.command_palette(&selection.palette_query, selection.conversation)?,
        shortcuts: app.command_shortcuts(selection.conversation)?,
        contact: contact_id
            .map(|id| app.contact_view(id))
            .transpose()?
            .flatten(),
        files: selection
            .conversation
            .map(|id| app.conversation_files_view(id, 30, None))
            .transpose()?,
        task_detail: selection.task.and_then(|id| app.task_detail_view(id).ok()),
        omni: app.omni().view(selection.omni_session)?,
        selection,
        conversations,
        chat,
    })
}

pub(crate) async fn execute(
    app: &LitecordApp,
    command: Command,
) -> litecord_core::Result<Completion> {
    let mut c = Completion::default();
    match command {
        Command::Edit(id, text) => match app.edit_message(id, &text).await? {
            litecord_actions::ProposeOutcome::Executed { .. } => c.message_changed = Some(id),
            _ => c.effects.push(UiEffect::Notice {
                message: "Edit requires approval in Inbox.".into(),
            }),
        },
        Command::Delete(id) => match app.delete_message(id).await? {
            litecord_actions::ProposeOutcome::Executed { .. } => c.message_changed = Some(id),
            _ => c.effects.push(UiEffect::Notice {
                message: "Deletion requires approval in Inbox.".into(),
            }),
        },
        Command::Intents(intents) => c.effects = app.apply_intents(intents).await?,
        Command::Note(note) => app.set_user_note(note)?,
        Command::Visibility(id, v) => app.set_conversation_visibility(id, Some(v))?,
        Command::Setting(key, value) => app.set_setting(&key, value)?,
        Command::Voice(control) => {
            app.voice(control).await?;
        }
        Command::Relationship(user, action) => match app.change_relationship(user, action).await? {
            litecord_actions::ProposeOutcome::Executed { .. } => {
                c.relationship_changed = Some(user)
            }
            _ => c.effects.push(UiEffect::Notice {
                message: "Relationship change requires review in Inbox.".into(),
            }),
        },
        Command::CompleteTask(id) => app.complete_task(id)?,
        Command::ConfirmTask(id) => app.confirm_task(id)?,
        Command::DismissTask(id) => app.dismiss_task(id)?,
        Command::ConfirmMemory(id) => app.confirm_memory(id)?,
        Command::RejectMemory(id) => app.reject_memory(id)?,
        Command::Approve(id) => {
            app.approve_action(id, None).await?;
        }
        Command::Reject(id) => app.reject_action(id)?,
        Command::SaveProfile(profile, t) => app.save_layout_profile(profile, &t)?,
        Command::ActivateProfile(id, t) => app.activate_layout_profile(&id, &t)?,
        Command::CreateProfile(name, t) => {
            let id = app.create_layout_profile(&name, &t)?;
            let next = app.layout_profiles_view()?;
            app.activate_layout_profile(&id, &next.storage_token)?;
        }
        Command::DuplicateProfile(id, name, t) => {
            app.duplicate_layout_profile(&id, &name, &t)?;
        }
        Command::RenameProfile(id, name, t) => app.rename_layout_profile(&id, &name, &t)?,
        Command::DeleteProfile(id, t) => app.delete_layout_profile(&id, &t)?,
        Command::ResetProfile(id, t) => app.reset_layout_profile(&id, &t)?,
        Command::ResetAll(t) => app.reset_all_layout_profiles(&t)?,
        Command::Run(id, active) => c.effects = app.run_command(&id, active).await?,
        Command::SendAs(id, text, identity) => {
            match app.send_message_as(id, &text, identity).await? {
                litecord_actions::ProposeOutcome::Executed { .. } => c.sent = Some((id, text)),
                litecord_actions::ProposeOutcome::PendingApproval { .. } => {
                    c.effects.push(UiEffect::Notice {
                        message: "Message requires approval in Inbox; your draft is preserved."
                            .into(),
                    })
                }
            }
        }
        Command::CreateTask(draft) => {
            if let litecord_actions::ProposeOutcome::Executed {
                result:
                    litecord_actions::ExecutionOutcome {
                        entity: Some(litecord_types::entity::EntityId::Task(id)),
                        ..
                    },
                ..
            } = app.create_task(draft).await?
            {
                c.task_created = Some(id);
            }
        }
        Command::SetTaskPriority(id, p) => {
            app.set_task_priority(id, p)?;
        }
        Command::AddTaskComment(id, body) => {
            app.add_task_comment(id, &body)?;
        }
        Command::Omni(cmd) => omni(app, cmd, &mut c).await?,
    }
    Ok(c)
}

async fn omni(
    app: &LitecordApp,
    cmd: OmniCommand,
    c: &mut Completion,
) -> litecord_core::Result<()> {
    use litecord_app::harness::LoginState;
    let omni = app.omni();
    match cmd {
        OmniCommand::Send(session, text) => c.omni_session = Some(omni.send(session, &text).await?),
        OmniCommand::New(mode) => c.omni_session = Some(omni.new_session(mode, "New chat")?),
        OmniCommand::Interrupt(id) => omni.interrupt(id).await?,
        OmniCommand::Compact(id) => omni.compact(id).await?,
        OmniCommand::Archive(id) => omni.archive(id)?,
        OmniCommand::Answer(id, decision) => omni.answer(&id, decision).await?,
        OmniCommand::Remember(session, seq) => {
            omni.remember(session, seq)?;
            c.effects.push(UiEffect::Notice {
                message: "Saved to Memory for review.".into(),
            });
        }
        OmniCommand::Select(kind) => omni.select(kind).await?,
        OmniCommand::SignOut => omni.sign_out().await?,
        OmniCommand::Refresh => {
            omni.refresh_login().await?;
        }
        OmniCommand::Heartbeats(on) => omni.set_heartbeat_enabled(on)?,
        OmniCommand::CheckNow => match omni.heartbeat(true).await? {
            litecord_app::HeartbeatOutcome::Sent { .. } => c.effects.push(UiEffect::Notice {
                message: "Omni is checking in.".into(),
            }),
            litecord_app::HeartbeatOutcome::Skipped(why) => c.effects.push(UiEffect::Notice {
                message: format!("Check-in skipped: {why}."),
            }),
        },
        OmniCommand::DismissCheckins => omni.dismiss_checkins()?,
        OmniCommand::LoadLoginOptions => {
            omni.login_options().await?;
        }
        OmniCommand::SignInWith(id) => {
            if let LoginState::SigningIn { url: Some(url), .. } = omni.sign_in_with(&id).await? {
                c.effects.push(UiEffect::OpenUrl { url });
            }
        }
        OmniCommand::SubmitCode(code) => omni.submit_login_code(&code).await?,
        OmniCommand::ApiKey(id, key) => {
            omni.sign_in_api_key(&id, &key).await?;
            c.effects.push(UiEffect::Notice {
                message: "Key handed to the harness. Litecord did not store it.".into(),
            });
        }
        OmniCommand::LoadModels => {
            omni.models().await?;
        }
        OmniCommand::SetModel(m) => omni.set_model(m.as_deref())?,
        OmniCommand::CreateAutomation(d) => {
            omni.create_automation(&d)?;
            c.effects.push(UiEffect::Notice {
                message: format!("Automation “{}” created.", d.name.trim()),
            });
        }
        OmniCommand::SetAutomationEnabled(id, on) => omni.set_automation_enabled(id, on)?,
        OmniCommand::DeleteAutomation(id) => omni.delete_automation(id)?,
        OmniCommand::RunAutomation(id) => {
            use litecord_app::automations::AutomationOutcome;
            let message = match omni.run_automation(id).await? {
                AutomationOutcome::Ran { .. } => {
                    "Automation started; results appear in Inbox.".into()
                }
                AutomationOutcome::Skipped(why) => format!("Automation skipped: {why}."),
            };
            c.effects.push(UiEffect::Notice { message });
        }
    }
    Ok(())
}
