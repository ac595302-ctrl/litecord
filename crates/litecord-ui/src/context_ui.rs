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
            Destination::Settings => {
                self.runtime_summary(ui);
                ui.add_space(8.0);
                self.history_summary(ui);
                ui.add_space(8.0);
                self.omni_summary(ui);
            }
            _ => self.omni_summary(ui),
        }
    }

    /// Settings inspector: live resource figures (Stage A budgets).
    fn runtime_summary(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        let m = &s.diagnostics.metrics;
        let kib = |b: u64| format!("{:.0} KiB", b as f64 / 1024.0);
        section_heading(ui, "Runtime", None);
        let rows = [
            (
                "Memory in use",
                m.rss_bytes.map_or_else(
                    || "unknown".to_owned(),
                    |b| format!("{:.1} MiB", b as f64 / 1_048_576.0),
                ),
            ),
            (
                "Event queue",
                format!(
                    "{} events · {}",
                    m.event_queue_depth,
                    kib(m.event_queue_bytes)
                ),
            ),
            ("Open conversation", kib(m.hot_cache_bytes)),
            (
                "Sync queue",
                format!(
                    "{} pending · {} active",
                    m.hydration_queue_depth, m.hydration_active
                ),
            ),
            ("Revision", s.diagnostics.revision.to_string()),
        ];
        egui::Grid::new("runtime_grid")
            .num_columns(2)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                for (k, v) in rows {
                    ui.label(theme::meta(k));
                    ui.label(RichText::new(v).size(13.0).color(theme::SECONDARY));
                    ui.end_row();
                }
            });
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
        let Some(c) = s.contact.clone() else {
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
            ui.add_space(4.0);
            theme::avatar_presence(ui, &name, 72.0, presence);
            ui.add_space(6.0);
            ui.label(RichText::new(&name).size(20.0).color(theme::TEXT));
            if let Some(u) = &c.username {
                ui.label(theme::meta(format!("@{}", self.display(u))));
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(presence.label()).size(12.0).color(presence.color()));
                if let Some(activity) = c.presence.activity.as_ref().filter(|_| !self.private()) {
                    ui.label(theme::meta(format!("· {}", activity.name)));
                }
            });
            ui.add_space(10.0);
            let dm = s
                .conversations
                .conversations
                .iter()
                .find(|r| r.recipient_id == Some(c.user_id))
                .map(|r| r.conversation_id);
            ui.horizontal(|ui| {
                // Four actions share the panel width; never wider than it.
                ui.spacing_mut().item_spacing.x = 4.0;
                let w = ((ui.available_width() - 12.0) / 4.0).clamp(40.0, 60.0);
                if round_action(ui, w, crate::icons::Glyph::Message, "Message", dm.is_some()) {
                    if let Some(conv) = dm {
                        self.navigate(Destination::Messages);
                        self.open_conversation(conv);
                    }
                }
                if round_action(ui, w, crate::icons::Glyph::Sparkle, "Ask Omni", true) {
                    self.omni_open = true;
                    self.omni_draft = format!("What should I know about {name} right now?");
                }
                if round_action(ui, w, crate::icons::Glyph::Note, "Note", !self.private()) {
                    ui.ctx().memory_mut(|m| m.request_focus(egui::Id::new("contact_note")));
                }
                if round_action(ui, w, crate::icons::Glyph::External, "Discord", true) {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(format!(
                        "https://discord.com/users/{}",
                        c.user_id
                    )));
                }
            });
            ui.add_space(8.0);
            ui.separator();
            section_heading(ui, "About", None);
            if self.private() {
                ui.label(theme::meta("Hidden in privacy mode"));
            } else {
                if self.note_draft.as_ref().map(|n| n.0) != Some(c.user_id) {
                    self.note_draft = Some((c.user_id, c.note.clone().unwrap_or_default()));
                }
                if let Some((_, text)) = self.note_draft.as_mut() {
                    ui.add(
                        egui::TextEdit::multiline(text)
                            .id(egui::Id::new("contact_note"))
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text("Your private note about them (only you can see it)"),
                    );
                }
                let changed = self.note_draft.as_ref().map(|n| n.1.as_str())
                    != Some(c.note.as_deref().unwrap_or(""));
                if changed && ui.add_enabled(!self.busy, egui::Button::new("Save note")).clicked() {
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
            if self.selection.destination != Destination::Messages {
                return;
            }
            let files = s.files.clone();
            let links = s.chat.as_ref().map(|chat| shared_links(chat)).unwrap_or_default();
            ui.add_space(6.0);
            ui.separator();
            section_heading(ui, "Shared", None);
            let file_count = files.as_ref().map_or(0, |f| f.files.len());
            egui::CollapsingHeader::new(format!("{file_count} Files{}", if files.as_ref().is_some_and(|f| f.has_more) { "+" } else { "" }))
                .id_salt("shared_files")
                .show(ui, |ui| {
                    if file_count == 0 {
                        ui.label(theme::meta("No attachments in cached history."));
                    }
                    for f in files.iter().flat_map(|f| f.files.iter()).take(20) {
                        let r = ui
                            .vertical(|ui| {
                                ui.add(egui::Label::new(self.display(&f.filename)).truncate());
                                ui.label(theme::meta(format!(
                                    "{} · {}",
                                    self.display(&f.author_name),
                                    human_size(f.size_bytes)
                                )));
                            })
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_text("Not downloaded by Litecord; opens the message in Discord");
                        if r.clicked() && !self.private() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(&f.open_in_discord_url));
                        }
                    }
                });
            egui::CollapsingHeader::new(format!("{} Links", links.len()))
                .id_salt("shared_links")
                .show(ui, |ui| {
                    if links.is_empty() {
                        ui.label(theme::meta("No links in the loaded messages."));
                    }
                    for link in links.iter().take(20) {
                        if self.private() {
                            ui.label(theme::meta("Hidden in privacy mode"));
                            break;
                        }
                        if ui.link(link.as_str()).clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(link));
                        }
                    }
                });
            if let Some(chat) = &s.chat {
                ui.add_space(6.0);
                ui.separator();
                section_heading(ui, "Omni access", None);
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
                    "What Omni and other agents may read here. Privacy mode only affects the screen.",
                ));
                ui.add_space(6.0);
                ui.separator();
                self.history_section(ui, chat.conversation_id);
            }
        });
    }

    /// Stage C: user-selected full history sync for one conversation.
    fn history_section(&mut self, ui: &mut Ui, id: litecord_types::ConversationId) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        section_heading(ui, "History", None);
        if s.history.quota_bytes == 0 {
            ui.label(theme::meta(
                "History sync is off. Set a database size limit to enable it.",
            ));
            return;
        }
        let row = s.history.row(id);
        match row {
            Some(r) if r.complete => {
                ui.label(theme::meta(format!(
                    "Full history stored · {} messages in {} pages",
                    r.messages, r.pages
                )));
            }
            Some(r) if r.enabled => {
                ui.label(theme::meta(format!(
                    "Syncing older messages · {} so far",
                    r.messages
                )));
                if let Some(reason) = &r.paused_reason {
                    ui.label(theme::meta(format!("Paused: {reason}")));
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Stop sync"))
                    .clicked()
                {
                    self.send(Command::SyncHistory(id, false));
                }
            }
            _ => {
                if let Some(err) = row.and_then(|r| r.last_error.as_ref()) {
                    ui.label(RichText::new(err).size(12.0).color(theme::PRIORITY));
                }
                ui.label(theme::meta(
                    "Only recent messages are stored. Sync walks back to the start of this conversation in the background.",
                ));
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Sync full history"))
                    .clicked()
                {
                    self.send(Command::SyncHistory(id, true));
                }
            }
        }
    }

    /// Settings inspector: every conversation selected for history sync,
    /// with database size against its quota.
    fn history_summary(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        let h = &s.history;
        section_heading(ui, "History sync", None);
        let mib = |b: u64| b as f64 / 1_048_576.0;
        if h.quota_bytes == 0 {
            ui.label(theme::meta(format!(
                "Off · database {:.1} MiB. Set retention.max_database_mb to enable.",
                mib(h.db_bytes)
            )));
            return;
        }
        let used = (h.db_bytes as f32 / h.quota_bytes as f32).clamp(0.0, 1.0);
        ui.add(egui::ProgressBar::new(used).desired_height(6.0));
        ui.label(theme::meta(format!(
            "Database {:.1} of {:.0} MiB",
            mib(h.db_bytes),
            mib(h.quota_bytes)
        )));
        if h.rows.is_empty() {
            ui.label(theme::meta(
                "No conversations selected. Use Sync full history in a chat's details.",
            ));
        }
        for r in h.rows.iter().take(12) {
            let state = if r.complete {
                "complete".to_owned()
            } else if !r.enabled {
                "stopped".to_owned()
            } else if let Some(p) = &r.paused_reason {
                format!("paused: {p}")
            } else {
                "syncing".to_owned()
            };
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(self.display(&r.title)).truncate());
                ui.label(theme::meta(format!("{} msgs · {state}", r.messages)));
            });
        }
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

/// Round icon action with a caption below (A01 inspector action row).
fn round_action(
    ui: &mut Ui,
    width: f32,
    glyph: crate::icons::Glyph,
    label: &str,
    enabled: bool,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, 58.0),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let c = egui::pos2(rect.center().x, rect.top() + 20.0);
        let fill = if enabled && response.hovered() {
            theme::HOVER
        } else {
            theme::RAISED
        };
        painter.circle_filled(c, 19.0, fill);
        crate::icons::glyph(
            painter,
            c,
            17.0,
            glyph,
            if enabled { theme::TEXT } else { theme::BORDER },
        );
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 2.0),
            egui::Align2::CENTER_BOTTOM,
            label,
            egui::FontId::proportional(11.0),
            if enabled {
                theme::SECONDARY
            } else {
                theme::MUTED
            },
        );
    }
    let text = label.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, &text));
    enabled && response.clicked()
}

/// Section title with an optional right-aligned "See all" style action.
pub(crate) fn section_heading(ui: &mut Ui, title: &str, action: Option<&str>) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(14.0).color(theme::TEXT).strong());
        if let Some(a) = action {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                clicked = ui
                    .add(
                        egui::Label::new(RichText::new(a).size(12.0).color(theme::PRIMARY_TEXT))
                            .sense(egui::Sense::click()),
                    )
                    .clicked();
            });
        }
    });
    clicked
}

/// Distinct http(s) links in the loaded messages, newest first.
fn shared_links(chat: &litecord_app::view::ConversationViewModel) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in chat.messages.iter().rev() {
        for word in m.render.content.split_whitespace() {
            let w =
                word.trim_matches(|c: char| matches!(c, '<' | '>' | '(' | ')' | ',' | '.' | '"'));
            if (w.starts_with("https://") || w.starts_with("http://"))
                && !out.iter().any(|o| o == w)
            {
                out.push(w.to_owned());
            }
        }
        for extra in &m.extras {
            if let litecord_types::social::MessageExtra::Embed { url: Some(u), .. } = extra {
                if !out.iter().any(|o| o == u) {
                    out.push(u.clone());
                }
            }
        }
    }
    out
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
