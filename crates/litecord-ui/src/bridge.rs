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
}

#[derive(Debug)]
pub enum Command {
    Send(ConversationId, String),
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
}

#[derive(Debug, Default)]
pub struct Completion {
    pub effects: Vec<UiEffect>,
    pub sent: Option<(ConversationId, String)>,
    pub message_changed: Option<MessageId>,
    pub relationship_changed: Option<UserId>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct Bridge {
    pub commands: mpsc::Sender<Command>,
    pub completions: mpsc::Receiver<Completion>,
    pub snapshots: watch::Receiver<Result<Snapshot, String>>,
    pub selection: watch::Sender<Selection>,
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
        };
        let (selection_tx, mut selection_rx) = watch::channel(selection);
        let (command_tx, mut commands) = mpsc::channel(32);
        let (completion_tx, completions) = mpsc::channel(32);
        let (snapshot_tx, snapshots) = watch::channel(Err("Loading workspace…".into()));
        let mut events = app.subscribe();
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
                    _ = poll.tick() => {}
                }
            }
        });
        Self {
            commands: command_tx,
            completions,
            snapshots,
            selection: selection_tx,
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
        Command::Send(id, text) => match app.send_message(id, &text).await? {
            litecord_actions::ProposeOutcome::Executed { .. } => c.sent = Some((id, text)),
            litecord_actions::ProposeOutcome::PendingApproval { .. } => {
                c.effects.push(UiEffect::Notice {
                    message: "Message requires approval in Inbox; your draft is preserved.".into(),
                })
            }
        },
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
    }
    Ok(c)
}
