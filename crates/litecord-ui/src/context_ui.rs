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
use crate::workspace::Workspace;
use crate::{kit, ph, theme};

/// Inbox filters: (label, index). 0 = everything.
pub(crate) const INBOX_FILTERS: [&str; 5] = ["All", "Replies", "Approvals", "Omni", "Suggestions"];

impl Workspace {
    pub(crate) fn context_sidebar_v2(&mut self, ui: &mut Ui) {
        match self.selection.destination {
            Destination::Voice => self.voice_sidebar(ui),
            Destination::Home => self.home_sidebar(ui),
            Destination::Inbox => self.inbox_sidebar(ui),
            Destination::Omni => self.memory_sidebar(ui),
            Destination::Tasks => self.tasks_sidebar(ui),
            Destination::Friends => self.friends_sidebar(ui),
            Destination::Settings => self.settings_sidebar(ui),
            other => {
                ui.label(RichText::new(other.label()).size(18.0));
            }
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
            Destination::Home => self.home_inspector(ui),
            Destination::Messages | Destination::Friends => self.contact_inspector(ui),
            Destination::Omni => self.memory_inspector(ui),
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
            kit::empty(
                ui,
                Some(ph::USER_CIRCLE),
                "No contact selected",
                "Select a person to see their profile.",
            );
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("inspector_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let width = ui.available_width();
                let name = self.display(c.alias.as_deref().unwrap_or(&c.display_name));
                let presence = theme::Presence::from_status(c.presence.status.as_str());
                let (r, _) = ui.allocate_exact_size(egui::vec2(width, 90.0), egui::Sense::hover());
                theme::paint_avatar_url(
                    ui.painter(),
                    egui::pos2(r.left() + 43.0, r.top() + 44.0),
                    86.0,
                    &name,
                    c.avatar_url.as_deref().filter(|_| !self.private()),
                    presence,
                    theme::INSPECTOR,
                );
                ui.add_space(8.0);
                kit::label(ui, &name, theme::semibold(23.0), theme::TEXT);
                if let Some(u) = &c.username {
                    ui.add_space(-4.0);
                    kit::label(ui, format!("@{}", self.display(u)), theme::regular(16.0), theme::SECONDARY);
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let (d, _) = ui.allocate_exact_size(egui::vec2(10.0, 18.0), egui::Sense::hover());
                    kit::dot(ui.painter(), d.center(), 4.5, presence.color());
                    kit::label(ui, presence.title(), theme::regular(14.0), theme::SECONDARY);
                });
                if let Some(activity) = c.presence.activity.as_ref().filter(|_| !self.private()) {
                    ui.horizontal(|ui| {
                        let (g, _) = ui.allocate_exact_size(egui::vec2(18.0, 20.0), egui::Sense::hover());
                        kit::icon(ui.painter(), g.center(), ph::GAME_CONTROLLER, 15.0, theme::MUTED);
                        let detail = activity
                            .details
                            .as_deref()
                            .map_or_else(|| activity.name.clone(), |d| format!("{} · {d}", activity.name));
                        kit::label(ui, detail, theme::regular(14.0), theme::SECONDARY);
                    });
                }
                ui.add_space(14.0);
                let dm = s
                    .conversations
                    .conversations
                    .iter()
                    .find(|r| r.recipient_id == Some(c.user_id))
                    .map(|r| r.conversation_id);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let w = ((ui.available_width() - 12.0) / 4.0).clamp(52.0, 84.0);
                    if kit::round_action(ui, ph::CHAT_CIRCLE, "Message", w, dm.is_some())
                        .on_disabled_hover_text("No cached DM with this person")
                        .clicked()
                    {
                        if let Some(conv) = dm {
                            self.open_conversation(conv);
                        }
                    }
                    if kit::round_action(ui, ph::PHONE, "Call", w, true)
                        .on_hover_text("Calls happen in Discord")
                        .clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(format!(
                            "https://discord.com/users/{}",
                            c.user_id
                        )));
                    }
                    if kit::round_action(ui, ph::SPARKLE, "Ask Omni", w, true).clicked() {
                        self.omni_open = true;
                        self.omni_draft = format!("What should I know about {name} right now?");
                    }
                    let more = kit::round_action(ui, ph::DOTS_THREE, "More", w, true);
                    egui::Popup::menu(&more).show(|ui| {
                        ui.set_min_width(200.0);
                        use litecord_types::actions::RelationshipAction;
                        use litecord_types::capability::Capability;
                        use litecord_types::social::RelationshipKind;
                        let enabled = !self.busy && !self.private() && s.diagnostics.session.is_online() && s.account.user_id != Some(c.user_id);
                        let requests = enabled && s.diagnostics.capabilities.is_usable(Capability::FriendRequests);
                        let blocking = enabled && s.diagnostics.capabilities.is_usable(Capability::Blocking);
                        let mut relationship_actions = Vec::new();
                        match c.relationship {
                            RelationshipKind::Friend => relationship_actions.push(("Remove friend",RelationshipAction::RemoveFriend,requests,true)),
                            RelationshipKind::PendingIncoming => {
                                relationship_actions.push(("Accept request",RelationshipAction::AcceptFriendRequest,requests,false));
                                relationship_actions.push(("Decline request",RelationshipAction::RejectFriendRequest,requests,true));
                            }
                            RelationshipKind::None | RelationshipKind::Implicit => relationship_actions.push(("Send friend request",RelationshipAction::SendFriendRequest,requests,false)),
                            _ => {}
                        }
                        if c.relationship == RelationshipKind::Blocked {
                            relationship_actions.push(("Unblock",RelationshipAction::Unblock,blocking,true));
                        } else {
                            relationship_actions.push(("Block",RelationshipAction::Block,blocking,true));
                        }
                        for (label,action,allowed,confirm) in relationship_actions {
                            if ui.add_enabled(allowed,egui::Button::new(label)).clicked() {
                                if confirm {
                                    self.relationship_confirmation=Some((c.user_id,action,c.display_name.clone()));
                                } else {
                                    self.send(Command::Relationship(c.user_id,action));
                                }
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.add_enabled(!self.private(), egui::Button::new("Edit private note")).clicked() {
                            self.pending_note_focus = Some(c.user_id);
                            ui.close();
                        }
                        if ui.button("Open profile in Discord").clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(format!(
                                "https://discord.com/users/{}",
                                c.user_id
                            )));
                        }
                        if ui
                            .button(if c.favorite { "Remove from favorites" } else { "Add to favorites" })
                            .clicked()
                            && !self.private()
                        {
                            self.send(Command::Note(UserNote {
                                user_id: c.user_id,
                                alias: c.alias.clone(),
                                note: c.note.clone(),
                                favorite: !c.favorite,
                                updated_at: Timestamp::now(),
                            }));
                        }
                    });
                });
                ui.add_space(10.0);
                kit::divider(ui);
                ui.add_space(12.0);
                kit::section(ui, "About", None, None);
                if self.private() {
                    kit::label(ui, "Hidden in privacy mode", theme::regular(15.0), theme::MUTED);
                } else {
                    if self.note_draft.as_ref().map(|n| n.0) != Some(c.user_id) {
                        self.note_draft = Some((c.user_id, c.note.clone().unwrap_or_default()));
                    }
                    if let Some((_, text)) = self.note_draft.as_mut() {
                        let response = ui.add(
                            egui::TextEdit::multiline(text)
                                .id(egui::Id::new(("contact_note", c.user_id)))
                                .font(theme::regular(15.0))
                                .text_color(theme::lerp(theme::TEXT, theme::SECONDARY, 0.4))
                                .frame(egui::Frame::NONE)
                                .desired_rows(2)
                                .desired_width(f32::INFINITY)
                                .hint_text(
                                    egui::RichText::new("Add a private note about them. Only you can see it.")
                                        .font(theme::regular(15.0))
                                        .color(theme::MUTED),
                                ),
                        );
                        if self.pending_note_focus == Some(c.user_id) {
                            self.pending_note_focus = None;
                            response.request_focus();
                        }
                    }
                    let changed = self.note_draft.as_ref().map(|n| n.1.as_str())
                        != Some(c.note.as_deref().unwrap_or(""));
                    if changed
                        && kit::button_ex(ui, kit::Kind::Primary, Some(ph::CHECK), "Save note", 30.0, !self.busy)
                            .clicked()
                    {
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
                    ui.add_space(12.0);
                    kit::divider(ui);
                    ui.add_space(12.0);
                    kit::section(ui, "Direct messages", None, None);
                    if let Some(conv) = dm {
                        let last = s
                            .conversations
                            .conversations
                            .iter()
                            .find(|r| r.conversation_id == conv)
                            .and_then(|r| r.last_message_preview.clone())
                            .unwrap_or_default();
                        if disclosure(ui, ph::CHAT_CIRCLE_TEXT, &self.display(&last), None).clicked() {
                            self.open_conversation(conv);
                        }
                    } else {
                        kit::label(ui, "No cached conversation yet.", theme::regular(14.0), theme::MUTED);
                    }
                    return;
                }
                let files = s.files.clone();
                let links = s.chat.as_ref().map(|chat| shared_links(chat)).unwrap_or_default();
                let saved = s
                    .chat
                    .as_ref()
                    .map_or(0, |chat| chat.messages.iter().filter(|m| m.bookmarked).count());
                ui.add_space(12.0);
                kit::divider(ui);
                ui.add_space(12.0);
                kit::section(ui, "Shared", None, None);
                let file_count = files.as_ref().map_or(0, |f| f.files.len());
                let more = if files.as_ref().is_some_and(|f| f.has_more) { "+" } else { "" };
                let files_id = ui.id().with("shared_files_open");
                let links_id = ui.id().with("shared_links_open");
                let files_open = ui.data(|d| d.get_temp::<bool>(files_id).unwrap_or(false));
                let links_open = ui.data(|d| d.get_temp::<bool>(links_id).unwrap_or(false));
                if disclosure(ui, ph::PAPERCLIP, &format!("{file_count}{more} Files"), Some(files_open)).clicked() {
                    ui.data_mut(|d| d.insert_temp(files_id, !files_open));
                }
                if files_open {
                    if file_count == 0 {
                        kit::label(ui, "No attachments in cached history.", theme::regular(13.0), theme::MUTED);
                    }
                    for f in files.iter().flat_map(|f| f.files.iter()).take(20) {
                        let (rect, r) = kit::row(ui, 46.0, false);
                        let painter = ui.painter();
                        kit::paint_tile(
                            painter,
                            egui::Rect::from_center_size(rect.left_center() + egui::vec2(20.0, 0.0), egui::vec2(32.0, 32.0)),
                            ph::FILE_TEXT,
                            kit::BLUE,
                            8.0,
                        );
                        kit::paint_row_text(
                            painter,
                            rect,
                            &kit::RowSpec {
                                title: &self.display(&f.filename),
                                subtitle: Some(&format!("{} · {}", self.display(&f.author_name), human_size(f.size_bytes))),
                                trailing: None,
                                title_color: theme::TEXT,
                                subtitle_color: theme::MUTED,
                                leading: 46.0,
                            },
                        );
                        if r.on_hover_text("Not downloaded by Litecord; opens the message in Discord").clicked()
                            && !self.private()
                        {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(&f.open_in_discord_url));
                        }
                    }
                }
                if disclosure(ui, ph::LINK, &format!("{} Links", links.len()), Some(links_open)).clicked() {
                    ui.data_mut(|d| d.insert_temp(links_id, !links_open));
                }
                if links_open {
                    if links.is_empty() {
                        kit::label(ui, "No links in the loaded messages.", theme::regular(13.0), theme::MUTED);
                    }
                    for link in links.iter().take(20) {
                        if self.private() {
                            kit::label(ui, "Hidden in privacy mode", theme::regular(13.0), theme::MUTED);
                            break;
                        }
                        if kit::link(ui, link).clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(link));
                        }
                    }
                }
                disclosure(ui, ph::BOOKMARK_SIMPLE, &format!("{saved} Saved messages"), None)
                    .on_hover_text("Messages you bookmarked in this conversation");
                if let Some(chat) = &s.chat {
                    ui.add_space(12.0);
                    kit::divider(ui);
                    ui.add_space(12.0);
                    kit::section(ui, "Omni access", None, None);
                    let mut v = chat.agent_visibility;
                    for opt in [AgentVisibility::Allowed, AgentVisibility::MetadataOnly, AgentVisibility::Hidden] {
                        let (rect, r) = kit::row(ui, 34.0, false);
                        let painter = ui.painter();
                        let c = rect.left_center() + egui::vec2(14.0, 0.0);
                        painter.circle_stroke(c, 8.0, egui::Stroke::new(1.5_f32, if v == opt { theme::PRIMARY } else { theme::MUTED }));
                        if v == opt {
                            painter.circle_filled(c, 4.5, theme::PRIMARY);
                        }
                        kit::text_at(painter, rect.left_center() + egui::vec2(32.0, 0.0), egui::Align2::LEFT_CENTER, visibility_label(opt), theme::regular(14.0), if v == opt { theme::TEXT } else { theme::SECONDARY }, rect.width() - 40.0);
                        if r.clicked() {
                            v = opt;
                        }
                    }
                    if v != chat.agent_visibility && !self.busy {
                        self.send(Command::Visibility(chat.conversation_id, v));
                    }
                    kit::para(
                        ui,
                        "What Omni and other agents may read here. Privacy mode only affects the screen.",
                        theme::regular(12.5),
                        theme::MUTED,
                    );
                    ui.add_space(12.0);
                    kit::divider(ui);
                    ui.add_space(12.0);
                    self.history_section(ui, chat.conversation_id);
                }
            });
    }

    /// Stage C: user-selected full history sync for one conversation.
    fn history_section(&mut self, ui: &mut Ui, id: litecord_types::ConversationId) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        kit::section(ui, "History", None, None);
        if s.history.quota_bytes == 0 {
            kit::para(
                ui,
                "History sync is off. Set a database size limit to enable it.",
                theme::regular(13.0),
                theme::MUTED,
            );
            return;
        }
        let row = s.history.row(id);
        match row {
            Some(r) if r.complete => {
                ui.horizontal(|ui| {
                    let (g, _) =
                        ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                    kit::icon(
                        ui.painter(),
                        g.center(),
                        ph::CHECK_CIRCLE,
                        16.0,
                        theme::SUCCESS,
                    );
                    kit::label(
                        ui,
                        format!("Full history stored · {} messages", r.messages),
                        theme::regular(14.0),
                        theme::SECONDARY,
                    );
                });
            }
            Some(r) if r.enabled => {
                kit::label(
                    ui,
                    format!("Syncing older messages · {} so far", r.messages),
                    theme::regular(14.0),
                    theme::SECONDARY,
                );
                if let Some(reason) = &r.paused_reason {
                    kit::label(
                        ui,
                        format!("Paused: {reason}"),
                        theme::regular(13.0),
                        theme::MUTED,
                    );
                }
                ui.add_space(4.0);
                if kit::button_ex(
                    ui,
                    kit::Kind::Secondary,
                    Some(ph::STOP),
                    "Stop sync",
                    32.0,
                    !self.busy,
                )
                .clicked()
                {
                    self.send(Command::SyncHistory(id, false));
                }
            }
            _ => {
                if let Some(err) = row.and_then(|r| r.last_error.as_ref()) {
                    kit::para(ui, err, theme::regular(13.0), theme::PRIORITY_TEXT);
                }
                kit::para(
                    ui,
                    "Only recent messages are stored. Sync walks back to the start of this conversation in the background.",
                    theme::regular(13.0),
                    theme::MUTED,
                );
                ui.add_space(4.0);
                if kit::button_ex(
                    ui,
                    kit::Kind::Secondary,
                    Some(ph::ARROWS_CLOCKWISE),
                    "Sync full history",
                    32.0,
                    !self.busy,
                )
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

/// Inspector row: icon, label, chevron (or disclosure arrow when `open`
/// is set), A01 "Shared" rows.
pub(crate) fn disclosure(
    ui: &mut Ui,
    glyph: &str,
    text: &str,
    open: Option<bool>,
) -> egui::Response {
    let (rect, response) = kit::row(ui, 38.0, false);
    let painter = ui.painter();
    kit::icon_o(
        painter,
        rect.left_center() + egui::vec2(12.0, 0.0),
        glyph,
        19.0,
        theme::SECONDARY,
    );
    kit::text_at(
        painter,
        rect.left_center() + egui::vec2(36.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        theme::regular(15.0),
        theme::lerp(theme::TEXT, theme::SECONDARY, 0.3),
        rect.width() - 64.0,
    );
    kit::icon_o(
        painter,
        rect.right_center() - egui::vec2(12.0, 0.0),
        if open == Some(true) {
            ph::CARET_DOWN
        } else {
            ph::CARET_RIGHT
        },
        15.0,
        theme::SECONDARY,
    );
    let t = text.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &t));
    response
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
