//! Contextual sidebars and inspectors for each destination: real content
//! (filters with counts, today's items, selected-entity details) instead of
//! placeholder text.

use eframe::egui::{self, RichText, Ui};
use litecord_app::view::InboxItem;
use litecord_layout::Destination;
use litecord_types::memory::{MemoryKind, MemoryStatus};
use litecord_types::notes::UserNote;
use litecord_types::trust::AgentVisibility;
use litecord_types::Timestamp;

use crate::bridge::Command;
use crate::messages_ui::human_size;
use crate::theme;
use crate::workspace::Workspace;

/// Inbox filters: (label, index). 0 = everything.
pub(crate) const INBOX_FILTERS: [&str; 5] = ["All", "Replies", "Approvals", "Omni", "Suggestions"];

impl Workspace {
    pub(crate) fn context_sidebar_v2(&mut self, ui: &mut Ui) {
        match self.selection.destination {
            Destination::Voice => self.voice_sidebar(ui),
            Destination::Home => self.home_sidebar(ui),
            Destination::Inbox => self.inbox_sidebar(ui),
            Destination::Memory => self.memory_sidebar(ui),
            Destination::Tasks => self.tasks_sidebar(ui),
            Destination::Friends => self.friends_sidebar(ui),
            Destination::Settings => self.settings_sidebar(ui),
            other => {
                ui.label(RichText::new(other.label()).size(18.0));
            }
        }
    }

    fn home_sidebar(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        ui.label(RichText::new("Today").size(18.0));
        ui.add_space(6.0);
        theme::section_label(ui, "Online now");
        if s.friends.online.is_empty() {
            ui.label(theme::meta("Nobody is online."));
        }
        for f in s.friends.online.iter().take(8) {
            let name = self.display(&f.display_name);
            let r = ui
                .horizontal(|ui| {
                    theme::avatar_presence(
                        ui,
                        &name,
                        22.0,
                        theme::Presence::from_status(f.status.as_str()),
                    );
                    ui.add(egui::Label::new(&name).truncate());
                })
                .response
                .interact(egui::Sense::click());
            if r.clicked() {
                if let Some(conv) = f.dm_conversation_id {
                    self.navigate(Destination::Messages);
                    self.open_conversation(conv);
                }
            }
        }
        ui.add_space(8.0);
        theme::section_label(ui, "Coming up");
        let now = Timestamp::now().as_millis();
        let mut upcoming: Vec<(i64, String)> = s
            .tasks
            .reminders
            .iter()
            .map(|r| (r.trigger.due_at().as_millis(), r.title.clone()))
            .chain(
                s.tasks
                    .open
                    .iter()
                    .filter_map(|t| t.due_at.map(|d| (d.as_millis(), t.title.clone()))),
            )
            .collect();
        upcoming.sort_by_key(|(d, _)| *d);
        if upcoming.is_empty() {
            ui.label(theme::meta("Nothing scheduled."));
        }
        for (due, title) in upcoming.into_iter().take(6) {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(relative(due - now))
                        .size(12.0)
                        .color(if due < now {
                            theme::PRIORITY
                        } else {
                            theme::MUTED
                        }),
                );
                ui.add(egui::Label::new(self.display(&title)).truncate());
            });
        }
        ui.add_space(12.0);
        if ui
            .add(egui::Button::new(
                RichText::new("Ask Omni what you missed").color(theme::OMNI),
            ))
            .clicked()
        {
            self.omni_open = true;
            self.omni_draft = "What did I miss today, and who is waiting on a reply?".into();
        }
    }

    fn inbox_sidebar(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        ui.label(RichText::new("Inbox").size(18.0));
        ui.add_space(6.0);
        let items = visible_attention(&s.inbox.needs_attention);
        let replies = items
            .iter()
            .filter(|i| {
                matches!(
                    i,
                    InboxItem::PendingReply { .. } | InboxItem::ReminderDue { .. }
                )
            })
            .count();
        let suggestions = items.len() - replies;
        let counts = [
            items.len()
                + s.inbox.pending_actions.len()
                + s.omni.checkins.len()
                + s.omni.requests.len(),
            replies,
            s.inbox.pending_actions.len() + s.omni.requests.len(),
            s.omni.checkins.len(),
            suggestions,
        ];
        for (i, label) in INBOX_FILTERS.into_iter().enumerate() {
            if theme::nav_row(ui, label, Some(counts[i]), self.inbox_filter == i).clicked() {
                self.inbox_filter = i;
            }
        }
        ui.add_space(12.0);
        theme::section_label(ui, "Check-ins");
        let on = s.omni.heartbeat_enabled;
        ui.label(theme::meta(if on {
            "Omni checks in on its own when something changes."
        } else {
            "Scheduled check-ins are off."
        }));
        ui.horizontal(|ui| {
            if ui
                .small_button(if on { "Turn off" } else { "Turn on" })
                .clicked()
            {
                self.send(Command::Omni(crate::bridge::OmniCommand::Heartbeats(!on)));
            }
            if ui.small_button("Check now").clicked() {
                self.send(Command::Omni(crate::bridge::OmniCommand::CheckNow));
            }
        });
    }

    fn memory_sidebar(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        ui.label(RichText::new("Memory").size(18.0));
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.memory_search)
                .hint_text("Filter memories…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(6.0);
        theme::section_label(ui, "Status");
        let count = |st: MemoryStatus| {
            s.memory
                .counts_by_status
                .iter()
                .find(|(x, _)| *x == st)
                .map_or(0, |(_, n)| *n as usize)
        };
        let statuses: [(&str, Option<MemoryStatus>); 3] = [
            ("All", None),
            ("To review", Some(MemoryStatus::Candidate)),
            ("Confirmed", Some(MemoryStatus::UserConfirmed)),
        ];
        for (label, st) in statuses {
            let n = st.map_or(s.memory.memories.len(), count);
            if theme::nav_row(ui, label, Some(n), self.memory_status_filter == st).clicked() {
                self.memory_status_filter = st;
            }
        }
        ui.add_space(8.0);
        theme::section_label(ui, "Kind");
        if theme::nav_row(ui, "Everything", None, self.memory_kind_filter.is_none()).clicked() {
            self.memory_kind_filter = None;
        }
        for kind in [
            MemoryKind::Commitment,
            MemoryKind::PendingReply,
            MemoryKind::ImportantDate,
            MemoryKind::Observation,
            MemoryKind::Summary,
            MemoryKind::Fact,
            MemoryKind::Preference,
            MemoryKind::Note,
        ] {
            let n = s.memory.memories.iter().filter(|m| m.kind == kind).count();
            if n == 0 && self.memory_kind_filter != Some(kind) {
                continue;
            }
            if theme::nav_row(
                ui,
                kind_label(kind),
                Some(n),
                self.memory_kind_filter == Some(kind),
            )
            .clicked()
            {
                self.memory_kind_filter = Some(kind);
            }
        }
    }

    fn friends_sidebar(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        ui.label(RichText::new("Friends").size(18.0));
        ui.add_space(6.0);
        let f = &s.friends;
        let counts = [
            f.online.len(),
            f.online.len() + f.offline.len(),
            f.pending_incoming.len() + f.pending_outgoing.len(),
            f.blocked.len(),
        ];
        for (i, label) in ["Online", "All friends", "Pending", "Blocked"]
            .into_iter()
            .enumerate()
        {
            if theme::nav_row(ui, label, Some(counts[i]), self.friends_tab == i).clicked() {
                self.friends_tab = i;
            }
        }
    }

    fn settings_sidebar(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Settings").size(18.0));
        ui.add_space(6.0);
        if theme::nav_row(ui, "All settings", None, self.settings_section.is_none()).clicked() {
            self.settings_section = None;
        }
        if theme::nav_row(
            ui,
            "Omni",
            None,
            self.settings_section.as_deref() == Some("Omni"),
        )
        .clicked()
        {
            self.settings_section = Some("Omni".into());
        }
        if let Some(s) = self.snapshot.clone() {
            for (section, _) in &s.settings.sections {
                if theme::nav_row(
                    ui,
                    section,
                    None,
                    self.settings_section.as_ref() == Some(section),
                )
                .clicked()
                {
                    self.settings_section = Some(section.clone());
                }
            }
        }
    }

    // ---- inspectors ----

    pub(crate) fn destination_inspector_v2(&mut self, ui: &mut Ui) {
        match self.selection.destination {
            Destination::Messages | Destination::Friends => self.contact_inspector(ui),
            Destination::Memory => self.memory_inspector(ui),
            Destination::Tasks => self.task_inspector(ui),
            Destination::Voice => {
                theme::section_label(ui, "Room");
                ui.label(theme::meta(
                    "Voice controls stay inside the room when you move this panel.",
                ));
            }
            _ => self.omni_summary(ui),
        }
    }

    /// Compact Omni status + latest check-ins, for Home/Inbox/Servers.
    fn omni_summary(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        ui.label(RichText::new("Omni").size(16.0).color(theme::OMNI));
        let status = match (&s.omni.status.selected, &s.omni.status.login) {
            (None, _) => "No agent harness connected".to_owned(),
            (
                Some(k),
                litecord_app::harness::LoginState::Ready { .. }
                | litecord_app::harness::LoginState::Stopped,
            ) => {
                format!("Ready on {}", k.label())
            }
            (Some(k), _) => format!("{} · sign-in needed", k.label()),
        };
        ui.label(theme::meta(status));
        if ui.button("Open Omni").clicked() {
            self.omni_open = true;
        }
        ui.add_space(8.0);
        theme::section_label(ui, "Latest check-ins");
        if s.omni.checkins.is_empty() {
            ui.label(theme::meta(if s.omni.heartbeat_enabled {
                "Nothing needs you right now."
            } else {
                "Check-ins are off. Turn them on in Inbox or in Omni settings."
            }));
        }
        for c in s.omni.checkins.iter().take(3) {
            egui::Frame::new()
                .fill(theme::OMNI.gamma_multiply(0.06))
                .corner_radius(6)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(self.display(&c.text)).size(13.0));
                });
        }
    }

    fn contact_inspector(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        if s.selection.contact != self.selection.contact {
            ui.spinner();
            return;
        }
        let Some(c) = &s.contact else {
            theme::empty_state(
                ui,
                "No contact selected",
                "Select a person to see their profile.",
            );
            return;
        };
        egui::ScrollArea::vertical().id_salt("inspector_scroll").show(ui, |ui| {
            let name = self.display(c.alias.as_deref().unwrap_or(&c.display_name));
            let presence = theme::Presence::from_status(c.presence.status.as_str());
            ui.horizontal(|ui| {
                theme::avatar_presence(ui, &name, 56.0, presence);
                ui.vertical(|ui| {
                    ui.label(RichText::new(&name).size(18.0).strong());
                    ui.label(theme::meta(self.display(c.username.as_deref().unwrap_or("Unknown user"))));
                    ui.label(RichText::new(presence.label()).size(12.0).color(presence.color()));
                });
            });
            if let Some(activity) = &c.presence.activity {
                ui.label(theme::meta(self.display(&activity.name)));
            }
            if self.selection.destination == Destination::Friends {
                if let Some(conv) = s
                    .conversations
                    .conversations
                    .iter()
                    .find(|r| r.recipient_id == Some(c.user_id))
                    .map(|r| r.conversation_id)
                {
                    if ui.button("Open conversation").clicked() {
                        self.navigate(Destination::Messages);
                        self.open_conversation(conv);
                    }
                }
            }
            ui.add_space(8.0);
            ui.separator();
            theme::section_label(ui, "Local note");
            if self.private() {
                ui.label(theme::meta("Hidden in privacy mode"));
            } else {
                if self.note_draft.as_ref().map(|n| n.0) != Some(c.user_id) {
                    self.note_draft = Some((c.user_id, c.note.clone().unwrap_or_default()));
                }
                if let Some((_, text)) = self.note_draft.as_mut() {
                    ui.add(
                        egui::TextEdit::multiline(text)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text("Only you can see this note"),
                    );
                }
                let changed = self.note_draft.as_ref().map(|n| n.1.as_str()) != Some(c.note.as_deref().unwrap_or(""));
                if ui.add_enabled(!self.busy && changed, egui::Button::new("Save note")).clicked() {
                    let note = self.note_draft.as_ref().map(|n| n.1.clone()).unwrap_or_default();
                    self.send(Command::Note(UserNote {
                        user_id: c.user_id,
                        alias: c.alias.clone(),
                        note: (!note.is_empty()).then_some(note),
                        favorite: c.favorite,
                        updated_at: Timestamp::now(),
                    }));
                }
            }
            if self.selection.destination == Destination::Messages {
                if let Some(chat) = &s.chat {
                    ui.add_space(8.0);
                    ui.separator();
                    theme::section_label(ui, "Omni access");
                    let mut v = chat.agent_visibility;
                    egui::ComboBox::from_id_salt("visibility")
                        .selected_text(visibility_label(v))
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for opt in [AgentVisibility::Allowed, AgentVisibility::MetadataOnly, AgentVisibility::Hidden] {
                                ui.selectable_value(&mut v, opt, visibility_label(opt));
                            }
                        });
                    if v != chat.agent_visibility && !self.busy {
                        self.send(Command::Visibility(chat.conversation_id, v));
                    }
                    ui.label(theme::meta(
                        "What Omni and other agents may read here. Privacy mode only affects what is on screen.",
                    ));
                }
                if let Some(files) = &s.files {
                    ui.add_space(8.0);
                    ui.separator();
                    theme::section_label(ui, &format!("Shared files · {}", files.files.len()));
                    if files.files.is_empty() {
                        ui.label(theme::meta("No attachments in cached history."));
                    }
                    for f in files.files.iter().take(12) {
                        let r = ui
                            .horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.add(egui::Label::new(self.display(&f.filename)).truncate());
                                    ui.label(theme::meta(format!(
                                        "{} · {}",
                                        self.display(&f.author_name),
                                        human_size(f.size_bytes)
                                    )));
                                });
                            })
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_text("Not downloaded by Litecord; opens the message in Discord");
                        if r.clicked() && !self.private() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(&f.open_in_discord_url));
                        }
                    }
                    if files.has_more {
                        ui.label(theme::meta("Older files are in Discord."));
                    }
                }
            }
        });
    }
}

/// Attention items as the Inbox shows them: a commitment that already
/// produced a suggested task appears once (as the task).
pub(crate) fn visible_attention(items: &[InboxItem]) -> Vec<&InboxItem> {
    let titles: Vec<&str> = items
        .iter()
        .filter_map(|i| match i {
            InboxItem::TaskCandidate { title, .. } => Some(title.as_str()),
            _ => None,
        })
        .collect();
    items
        .iter()
        .filter(|i| match i {
            InboxItem::Commitment { text, .. } => !titles.iter().any(|t| text.contains(t)),
            _ => true,
        })
        .collect()
}

pub(crate) fn visibility_label(v: AgentVisibility) -> &'static str {
    match v {
        AgentVisibility::Allowed => "Omni can read messages",
        AgentVisibility::MetadataOnly => "Names and times only",
        AgentVisibility::Hidden => "Hidden from Omni",
    }
}

pub(crate) fn kind_label(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Observation => "Observations",
        MemoryKind::Summary => "Summaries",
        MemoryKind::Commitment => "Commitments",
        MemoryKind::PendingReply => "Waiting on reply",
        MemoryKind::ImportantDate => "Dates",
        MemoryKind::Preference => "Preferences",
        MemoryKind::Fact => "Facts",
        MemoryKind::Note => "Notes",
        MemoryKind::Operational => "Omni activity",
    }
}

/// "in 3h", "2d ago".
pub(crate) fn relative(delta_ms: i64) -> String {
    let past = delta_ms < 0;
    let mins = delta_ms.unsigned_abs() / 60_000;
    let text = if mins < 60 {
        format!("{mins}m")
    } else if mins < 48 * 60 {
        format!("{}h", mins / 60)
    } else {
        format!("{}d", mins / (24 * 60))
    };
    if past {
        format!("{text} ago")
    } else {
        format!("in {text}")
    }
}
