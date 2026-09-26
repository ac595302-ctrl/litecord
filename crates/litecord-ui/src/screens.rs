use crate::{bridge::Command, icons, theme, workspace::Workspace};
use eframe::egui::{self, Align2, FontId, Rect, Sense, Ui};
use litecord_app::view::{FriendRow, MessageRow};
use litecord_layout::{Destination, Orientation};
use litecord_types::{
    actions::RelationshipAction,
    capability::Capability,
    notes::UserNote,
    social::{ConversationKind, PresenceStatus, RelationshipKind},
    trust::AgentVisibility,
    Timestamp,
};

impl Workspace {
    pub fn panel(&mut self, ui: &mut Ui, panel: &str, orientation: Orientation) {
        match panel {
            "primary_navigation" => self.navigation(ui, orientation),
            "user_controls" => self.account(ui, orientation),
            "conversation_list" => self.conversations(ui),
            "chat" => self.chat(ui),
            "friends" => self.friends(ui),
            "context_inspector" => self.destination_inspector(ui),
            "home" => self.home_screen(ui),
            "agent_inbox" => self.inbox_screen(ui),
            "tasks" => self.tasks_screen(ui),
            "memory" => self.memory_screen(ui),
            "settings" => self.settings_screen(ui),
            "server_list" => self.server_list(ui, orientation),
            "channel_list" => self.channels(ui),
            "server_content" => self.server_content(ui),
            "voice_room" => self.voice_room(ui),
            "contextual_sidebar" => self.context_sidebar(ui),
            _ => {
                ui.heading(self.selection.destination.label());
                ui.label("This destination is being connected in the next UI stage.");
            }
        }
    }
    fn navigation(&mut self, ui: &mut Ui, orientation: Orientation) {
        let horizontal = orientation == Orientation::Horizontal;
        egui::ScrollArea::both()
            .id_salt("nav_scroll")
            .show(ui, |ui| {
                let layout = if horizontal {
                    egui::Layout::left_to_right(egui::Align::Center)
                } else {
                    egui::Layout::top_down(egui::Align::Center)
                };
                ui.with_layout(layout, |ui| {
                    let (logo, _) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), Sense::hover());
                    ui.painter()
                        .rect_filled(logo.shrink(4.0), 10.0, theme::PRIMARY);
                    ui.painter().text(
                        logo.center(),
                        Align2::CENTER_CENTER,
                        "L",
                        FontId::proportional(24.0),
                        theme::TEXT,
                    );
                    ui.add_space(12.0);
                    for d in Destination::ALL {
                        let size = if horizontal {
                            egui::vec2(76.0, 60.0)
                        } else {
                            egui::vec2(ui.available_width().max(40.0), 62.0)
                        };
                        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                        if self.selection.destination == d || response.hovered() {
                            ui.painter()
                                .rect_filled(rect.shrink(2.0), 6.0, theme::SELECTED);
                        }
                        if self.selection.destination == d {
                            ui.painter().rect_filled(
                                Rect::from_min_size(
                                    rect.left_top() + egui::vec2(0.0, 10.0),
                                    egui::vec2(3.0, 42.0),
                                ),
                                2.0,
                                theme::PRIMARY,
                            );
                        }
                        icons::paint(
                            ui.painter(),
                            egui::pos2(rect.center().x, rect.top() + 20.0),
                            20.0,
                            d,
                            if self.selection.destination == d {
                                theme::PRIMARY
                            } else {
                                theme::MUTED
                            },
                        );
                        ui.painter().text(
                            egui::pos2(rect.center().x, rect.top() + 45.0),
                            Align2::CENTER_CENTER,
                            d.label(),
                            FontId::proportional(11.0),
                            theme::SECONDARY,
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::SelectableLabel,
                                true,
                                self.selection.destination == d,
                                d.label(),
                            )
                        });
                        if response.clicked() {
                            self.navigate(d);
                        }
                        if d == Destination::Servers {
                            self.server_flyout(ui, &response);
                        }
                    }
                });
            });
    }
    fn account(&mut self, ui: &mut Ui, orientation: Orientation) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        let name = self.display(&s.account.display_name);
        ui.with_layout(
            if orientation == Orientation::Horizontal {
                egui::Layout::left_to_right(egui::Align::Center)
            } else {
                egui::Layout::top_down(egui::Align::Center)
            },
            |ui| {
                let avatar = theme::avatar(ui, &name, 40.0, s.diagnostics.session.is_online());
                avatar.context_menu(|ui| {
                    ui.label(&name);
                    if ui.button("Settings").clicked() {
                        self.navigate(Destination::Settings);
                        ui.close();
                    }
                });
                if ui.available_width() > 140.0 {
                    ui.label(name);
                }
            },
        );
    }
    fn conversations(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.heading("Messages");
        });
        ui.add_space(8.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Search conversations…")
                .desired_width(f32::INFINITY),
        );
        ui.horizontal_wrapped(|ui| {
            for (i, label) in ["All", "Reply", "Groups", "DMs"].into_iter().enumerate() {
                if ui
                    .selectable_label(self.conversation_tab == i, label)
                    .clicked()
                {
                    self.conversation_tab = i;
                }
            }
        });
        ui.add_space(4.0);
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        let filter = self.filter.to_lowercase();
        let rows: Vec<_> = s
            .conversations
            .conversations
            .iter()
            .filter(|r| r.title.to_lowercase().contains(&filter))
            .filter(|r| match self.conversation_tab {
                1 => r.awaiting_reply,
                2 => r.kind == ConversationKind::GroupDm,
                3 => r.kind == ConversationKind::DirectMessage,
                _ => true,
            })
            .collect();
        if rows.is_empty() {
            ui.label("No conversations here.");
        }
        egui::ScrollArea::vertical()
            .id_salt("conversation_scroll")
            .show_rows(ui, 64.0, rows.len(), |ui, range| {
                for index in range {
                    let r = rows[index];
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 64.0),
                        Sense::click(),
                    );
                    if Some(r.conversation_id) == self.selection.conversation || response.hovered()
                    {
                        ui.painter().rect_filled(
                            rect.shrink2(egui::vec2(0.0, 3.0)),
                            6.0,
                            theme::SELECTED,
                        );
                    }
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(rect.shrink(8.0))
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    let title = self.display(&r.title);
                    theme::avatar(
                        &mut child,
                        &title,
                        36.0,
                        r.recipient_status == Some(PresenceStatus::Online),
                    );
                    child.vertical(|ui| {
                        ui.add(egui::Label::new(egui::RichText::new(&title).strong()).truncate());
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(
                                    self.display(
                                        r.last_message_preview
                                            .as_deref()
                                            .unwrap_or("No recent messages"),
                                    ),
                                )
                                .size(12.0)
                                .color(theme::MUTED),
                            )
                            .truncate(),
                        );
                    });
                    if r.awaiting_reply {
                        ui.painter().circle_filled(
                            rect.right_center() - egui::vec2(10.0, 0.0),
                            3.0,
                            theme::PRIMARY,
                        );
                    }
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            Some(r.conversation_id) == self.selection.conversation,
                            title.clone(),
                        )
                    });
                    if response.clicked() {
                        self.open_conversation(r.conversation_id);
                    }
                }
            });
    }
    fn chat(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        let Some(chat) = &s.chat else {
            ui.heading("Messages");
            ui.label("Choose a conversation to begin.");
            return;
        };
        if s.selection.conversation != self.selection.conversation
            || s.selection.before != self.selection.before
        {
            ui.spinner();
            ui.label("Loading conversation…");
            return;
        }
        ui.horizontal(|ui| {
            theme::avatar(
                ui,
                &self.display(&chat.title),
                36.0,
                s.contact
                    .as_ref()
                    .is_some_and(|c| c.presence.status == PresenceStatus::Online),
            );
            ui.vertical(|ui| {
                ui.label(
                    egui::RichText::new(self.display(&chat.title))
                        .size(16.0)
                        .strong(),
                );
                ui.label(
                    egui::RichText::new(
                        s.contact
                            .as_ref()
                            .map(|c| c.presence.status.as_str())
                            .unwrap_or("Conversation"),
                    )
                    .size(12.0)
                    .color(theme::MUTED),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Open in Discord").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(
                        chat.capabilities.open_in_discord_url.clone(),
                    ));
                }
            });
        });
        ui.separator();
        ui.horizontal(|ui| {
            if chat.has_more && ui.small_button("Older messages").clicked() {
                self.selection.before = chat.messages.first().map(|m| m.sent_at);
                self.request();
            }
            if self.selection.before.is_some() && ui.small_button("Latest messages").clicked() {
                self.selection.before = None;
                self.request();
            }
        });
        let height = (ui.available_height() - 100.0).max(80.0);
        let private = self.private();
        egui::ScrollArea::vertical()
            .id_salt(("messages", chat.conversation_id, self.selection.before))
            .max_height(height)
            .auto_shrink([false, false])
            .stick_to_bottom(self.selection.before.is_none())
            .show_viewport(ui, |ui, viewport| {
                // Measure variable-height rows, paint only the visible range.
                let width = ui.available_width();
                let top = ui.cursor().min;
                let mut y = 0.0;
                for row in &chat.messages {
                    let text = if private {
                        "Hidden in privacy mode".to_owned()
                    } else {
                        row.render.content.clone()
                    };
                    let galley = ui.painter().layout(
                        text,
                        FontId::proportional(14.0),
                        theme::SECONDARY,
                        (width - 56.0).max(40.0),
                    );
                    let extra = if row.extras.is_empty() || private {
                        0.0
                    } else {
                        32.0
                    };
                    let row_height =
                        galley.size().y + if row.render.compact { 28.0 } else { 46.0 } + extra;
                    if y + row_height >= viewport.min.y && y <= viewport.max.y {
                        let rect = Rect::from_min_size(
                            top + egui::vec2(0.0, y),
                            egui::vec2(width, row_height),
                        );
                        self.message_row(ui, rect, row, galley, private);
                    }
                    y += row_height;
                }
                ui.allocate_space(egui::vec2(width, y));
                if chat.messages.is_empty() {
                    ui.label("No cached messages yet. History loads in the background.");
                }
            });
        ui.add_space(8.0);
        let id = chat.conversation_id;
        let draft = self.drafts.entry(id).or_default();
        let private_hint = if private {
            "Message (privacy mode)".to_owned()
        } else {
            format!("Message {}…", chat.title)
        };
        let response = ui.add_enabled(
            !self.busy && chat.capabilities.can_send,
            egui::TextEdit::multiline(draft)
                .password(private)
                .id_salt(("composer", id))
                .hint_text(private_hint)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        let submit = response.has_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
        let text = draft.clone();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(if chat.capabilities.can_send {
                    "Ctrl/Cmd+Enter to send"
                } else {
                    "Sending unavailable on this backend"
                })
                .size(12.0)
                .color(theme::MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_send = !self.busy
                    && chat.capabilities.can_send
                    && !text.trim().is_empty()
                    && text.chars().count() <= 2000;
                if ui
                    .add_enabled(can_send, egui::Button::new("Send").fill(theme::PRIMARY))
                    .clicked()
                    || submit && can_send
                {
                    self.send(Command::Send(id, text));
                }
            });
        });
    }
    fn message_row(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        row: &MessageRow,
        galley: std::sync::Arc<egui::Galley>,
        private: bool,
    ) {
        let response = ui.interact(rect, ui.id().with(row.message_id), Sense::click());
        if response.hovered() || row.render.highlighted {
            ui.painter()
                .rect_filled(rect.shrink(1.0), 6.0, theme::RAISED);
        }
        let name = if private {
            "Hidden user".to_owned()
        } else {
            row.render.author_display.clone()
        };
        let mut avatar = ui.new_child(
            egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min, egui::vec2(38.0, 38.0))),
        );
        theme::avatar(&mut avatar, &name, 32.0, false);
        let start = rect.min + egui::vec2(48.0, 4.0);
        ui.painter().text(
            start,
            Align2::LEFT_TOP,
            &name,
            FontId::proportional(14.0),
            theme::TEXT,
        );
        let time = clock(row.sent_at);
        ui.painter().text(
            egui::pos2(rect.right() - 8.0, start.y),
            Align2::RIGHT_TOP,
            if row.edited {
                format!("{time} · edited")
            } else {
                time
            },
            FontId::proportional(11.0),
            theme::MUTED,
        );
        ui.painter()
            .galley(start + egui::vec2(0.0, 24.0), galley, theme::SECONDARY);
        if !row.extras.is_empty() && !private {
            ui.painter().text(
                egui::pos2(start.x, rect.bottom() - 24.0),
                Align2::LEFT_TOP,
                format!("{} rich item(s) · open in Discord", row.extras.len()),
                FontId::proportional(12.0),
                theme::MUTED,
            );
        }
        if row.bookmarked {
            ui.painter().circle_filled(
                rect.right_bottom() - egui::vec2(10.0, 10.0),
                3.0,
                theme::OMNI,
            );
        }
        response.context_menu(|ui| {
            for action in &row.actions {
                let reveals = action.intents.iter().any(|i| {
                    matches!(
                        i,
                        litecord_features::intent::AppIntent::CopyToClipboard { .. }
                    )
                });
                if ui
                    .add_enabled(
                        !self.busy && !(private && reveals),
                        egui::Button::new(&action.label),
                    )
                    .clicked()
                {
                    self.send(Command::Intents(action.intents.clone()));
                    ui.close();
                }
            }
            if row.is_mine && !private {
                let can_edit = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.chat.as_ref())
                    .is_some_and(|c| c.capabilities.can_edit);
                if ui
                    .add_enabled(can_edit && !self.busy, egui::Button::new("Edit message"))
                    .clicked()
                {
                    self.editing_message = Some((row.message_id, row.render.content.clone()));
                    ui.close();
                }
                let can_delete = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.chat.as_ref())
                    .is_some_and(|c| c.capabilities.can_delete);
                if ui
                    .add_enabled(
                        can_delete && !self.busy,
                        egui::Button::new("Delete message"),
                    )
                    .clicked()
                {
                    self.deleting_message = Some(row.message_id);
                    ui.close();
                }
            }
        });
    }
    fn friends(&mut self, ui: &mut Ui) {
        ui.heading("Friends");
        ui.horizontal_wrapped(|ui| {
            for (i, label) in ["Online", "All", "Pending", "Blocked"]
                .into_iter()
                .enumerate()
            {
                if ui.selectable_label(self.friends_tab == i, label).clicked() {
                    self.friends_tab = i;
                }
            }
        });
        ui.separator();
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        let rows: Vec<&FriendRow> = match self.friends_tab {
            0 => s.friends.online.iter().collect(),
            1 => s.friends.online.iter().chain(&s.friends.offline).collect(),
            2 => s
                .friends
                .pending_incoming
                .iter()
                .chain(&s.friends.pending_outgoing)
                .collect(),
            _ => s.friends.blocked.iter().collect(),
        };
        theme::section_label(ui, &format!("{} contacts", rows.len()));
        if rows.is_empty() {
            ui.label("No contacts in this view.");
        }
        egui::ScrollArea::vertical()
            .id_salt("friends_scroll")
            .show_rows(ui, 72.0, rows.len(), |ui, range| {
                for i in range {
                    let r = rows[i];
                    egui::Frame::new()
                        .fill(theme::WORKSPACE)
                        .inner_margin(8)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let name =
                                    self.display(r.alias.as_deref().unwrap_or(&r.display_name));
                                theme::avatar(ui, &name, 40.0, r.status == PresenceStatus::Online);
                                ui.vertical(|ui| {
                                    if ui
                                        .selectable_label(
                                            self.selection.contact == Some(r.user_id),
                                            egui::RichText::new(name).strong(),
                                        )
                                        .clicked()
                                    {
                                        self.selection.contact = Some(r.user_id);
                                        self.note_draft = None;
                                        self.request();
                                    }
                                    ui.label(
                                        egui::RichText::new(
                                            if matches!(
                                                r.relationship,
                                                RelationshipKind::PendingIncoming
                                                    | RelationshipKind::PendingOutgoing
                                            ) {
                                                if r.relationship
                                                    == RelationshipKind::PendingIncoming
                                                {
                                                    "Incoming request"
                                                } else {
                                                    "Outgoing request"
                                                }
                                            } else {
                                                r.status.as_str()
                                            },
                                        )
                                        .size(12.0)
                                        .color(theme::MUTED),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        let enabled = !self.busy
                                            && !self.private()
                                            && s.diagnostics.session.is_online();
                                        let requests = enabled
                                            && s.diagnostics
                                                .capabilities
                                                .is_usable(Capability::FriendRequests);
                                        let blocking = enabled
                                            && s.diagnostics
                                                .capabilities
                                                .is_usable(Capability::Blocking);
                                        if r.relationship == RelationshipKind::PendingIncoming {
                                            if ui
                                                .add_enabled(requests, egui::Button::new("Decline"))
                                                .clicked()
                                            {
                                                self.relationship_confirmation = Some((
                                                    r.user_id,
                                                    RelationshipAction::RejectFriendRequest,
                                                    r.display_name.clone(),
                                                ));
                                            }
                                            if ui
                                                .add_enabled(
                                                    requests,
                                                    egui::Button::new("Accept")
                                                        .fill(theme::PRIMARY),
                                                )
                                                .clicked()
                                            {
                                                self.send(Command::Relationship(
                                                    r.user_id,
                                                    RelationshipAction::AcceptFriendRequest,
                                                ));
                                            }
                                        } else if r.relationship == RelationshipKind::Blocked {
                                            if ui
                                                .add_enabled(blocking, egui::Button::new("Unblock"))
                                                .clicked()
                                            {
                                                self.relationship_confirmation = Some((
                                                    r.user_id,
                                                    RelationshipAction::Unblock,
                                                    r.display_name.clone(),
                                                ));
                                            }
                                        } else {
                                            ui.menu_button("More", |ui| {
                                                if ui
                                                    .add_enabled(
                                                        blocking,
                                                        egui::Button::new("Block user"),
                                                    )
                                                    .clicked()
                                                {
                                                    self.relationship_confirmation = Some((
                                                        r.user_id,
                                                        RelationshipAction::Block,
                                                        r.display_name.clone(),
                                                    ));
                                                    ui.close();
                                                }
                                                if r.relationship == RelationshipKind::Friend
                                                    && ui
                                                        .add_enabled(
                                                            requests,
                                                            egui::Button::new("Remove friend"),
                                                        )
                                                        .clicked()
                                                {
                                                    self.relationship_confirmation = Some((
                                                        r.user_id,
                                                        RelationshipAction::RemoveFriend,
                                                        r.display_name.clone(),
                                                    ));
                                                    ui.close();
                                                }
                                            });
                                        }
                                        if ui
                                            .add_enabled(
                                                r.dm_conversation_id.is_some(),
                                                egui::Button::new("Message"),
                                            )
                                            .on_disabled_hover_text(
                                                "No cached DM exists for this contact",
                                            )
                                            .clicked()
                                        {
                                            if let Some(id) = r.dm_conversation_id {
                                                self.open_conversation(id);
                                            }
                                        }
                                    },
                                );
                            });
                        });
                }
            });
    }
    pub(crate) fn inspector(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        if s.selection.contact != self.selection.contact {
            ui.spinner();
            return;
        }
        if let Some(c) = &s.contact {
            egui::ScrollArea::vertical().id_salt("inspector_scroll").show(ui,|ui|{
                let name=self.display(c.alias.as_deref().unwrap_or(&c.display_name));
                theme::avatar(ui,&name,72.0,c.presence.status==PresenceStatus::Online);
                ui.add_space(8.0);ui.label(egui::RichText::new(name).size(20.0).strong());
                ui.label(egui::RichText::new(self.display(c.username.as_deref().unwrap_or("Unknown user"))).color(theme::MUTED));
                theme::chip(ui,c.presence.status.as_str(),theme::MUTED);
                if let Some(activity)=&c.presence.activity {ui.label(self.display(&activity.name));}
                ui.add_space(16.0);ui.separator();theme::section_label(ui,"Local note");
                if self.private(){ui.label("Hidden in privacy mode");}else{
                    if self.note_draft.as_ref().map(|n|n.0)!=Some(c.user_id){self.note_draft=Some((c.user_id,c.note.clone().unwrap_or_default()));}
                    let text=self.note_draft.as_mut().map(|n|&mut n.1);
                    if let Some(text)=text{ui.add(egui::TextEdit::multiline(text).desired_rows(4).desired_width(f32::INFINITY).hint_text("Only you can see this note"));}
                    if ui.add_enabled(!self.busy,egui::Button::new("Save note")).clicked(){let note=self.note_draft.as_ref().map(|n|n.1.clone()).unwrap_or_default();self.send(Command::Note(UserNote{user_id:c.user_id,alias:c.alias.clone(),note:(!note.is_empty()).then_some(note),favorite:c.favorite,updated_at:Timestamp::now()}));}
                }
                ui.add_space(16.0);ui.separator();theme::section_label(ui,"Omni access");
                if let Some(chat)=s.chat.as_ref().filter(|_|self.selection.destination==Destination::Messages) {
                    let mut visibility=chat.agent_visibility;
                    egui::ComboBox::from_id_salt("visibility").selected_text(format!("{visibility:?}")).show_ui(ui,|ui|{for v in [AgentVisibility::Hidden,AgentVisibility::MetadataOnly,AgentVisibility::Allowed]{ui.selectable_value(&mut visibility,v,format!("{v:?}"));}});
                    if visibility!=chat.agent_visibility&&!self.busy{self.send(Command::Visibility(chat.conversation_id,visibility));}
                    ui.label(egui::RichText::new("Controls what agents may read. Privacy mode controls what appears on screen.").size(12.0).color(theme::MUTED));
                }
            });
        } else {
            theme::section_label(ui, "Context");
            ui.label("Select a contact to inspect their profile.");
        }
    }
    pub fn relationship_dialog(&mut self, ctx: &egui::Context) {
        if self.private() {
            return;
        }
        let Some((user, action, name)) = self.relationship_confirmation.clone() else {
            return;
        };
        let label = match action {
            RelationshipAction::Block => "Block user",
            RelationshipAction::Unblock => "Unblock user",
            RelationshipAction::RemoveFriend => "Remove friend",
            RelationshipAction::RejectFriendRequest => "Decline request",
            _ => "Change relationship",
        };
        let mut open = true;
        egui::Window::new(label)
            .open(&mut open)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(format!("{label}: {name}?"));
                ui.label("This changes your relationship on the connected backend.");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.busy, egui::Button::new(label))
                        .clicked()
                    {
                        self.send(Command::Relationship(user, action));
                    }
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        self.relationship_confirmation = None;
                    }
                });
            });
        if !open && !self.busy {
            self.relationship_confirmation = None;
        }
    }
    fn context_sidebar(&mut self, ui: &mut Ui) {
        if self.selection.destination == Destination::Voice {
            self.voice_sidebar(ui);
            return;
        }
        ui.heading(self.selection.destination.label());
        ui.add_space(12.0);
        if self.selection.destination == Destination::Friends {
            theme::section_label(ui, "Your connections");
            for (i, label) in ["Online", "All friends", "Pending requests", "Blocked"]
                .into_iter()
                .enumerate()
            {
                if ui.selectable_label(self.friends_tab == i, label).clicked() {
                    self.friends_tab = i;
                }
            }
        } else {
            theme::section_label(ui, "Workspace");
            if self.selection.destination == Destination::Settings {
                if ui
                    .selectable_label(self.settings_section.is_none(), "All settings")
                    .clicked()
                {
                    self.settings_section = None;
                }
                if let Some(s) = self.snapshot.clone() {
                    for (section, _) in &s.settings.sections {
                        if ui
                            .selectable_label(
                                self.settings_section.as_ref() == Some(section),
                                section,
                            )
                            .clicked()
                        {
                            self.settings_section = Some(section.clone());
                        }
                    }
                }
            } else {
                ui.label("Your local information and activity.");
            }
        }
    }
    pub fn message_dialogs(&mut self, ctx: &egui::Context) {
        if self.private() {
            return;
        }
        if let Some((id, mut text)) = self.editing_message.clone() {
            let mut open = true;
            egui::Window::new("Edit message")
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut text).desired_width(440.0));
                    if ui
                        .add_enabled(
                            !self.busy && !text.trim().is_empty(),
                            egui::Button::new("Save changes"),
                        )
                        .clicked()
                    {
                        self.send(Command::Edit(id, text.clone()));
                        self.editing_message = Some((id, text.clone()));
                    } else {
                        self.editing_message = Some((id, text.clone()));
                    }
                });
            if !open {
                self.editing_message = None;
            }
        }
        if let Some(id) = self.deleting_message {
            egui::Window::new("Delete message?")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("This deletes your message from the conversation.");
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!self.busy, egui::Button::new("Delete"))
                            .clicked()
                        {
                            self.send(Command::Delete(id));
                        }
                        if ui.button("Cancel").clicked() {
                            self.deleting_message = None;
                        }
                    });
                });
        }
    }
    pub fn profiles_window(&mut self, ctx: &egui::Context) {
        if !self.profile_manager {
            return;
        }
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Workspace layouts")
            .open(&mut open)
            .default_width(460.0)
            .show(ctx, |ui| {
                if let Some(notice) = &s.layouts.recovery_notice {
                    ui.label(notice);
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Reset saved layouts"))
                        .clicked()
                    {
                        self.send(Command::ResetAll(s.layouts.storage_token.clone()));
                    }
                    return;
                }
                ui.label("Layouts change panels and geometry. Conversation drafts stay with you.");
                ui.separator();
                if self.edit_original.is_some() {
                    ui.label("Drag a panel header to an edge. Drag dividers to resize.");
                    if let Some(tree) = self
                        .profile
                        .destinations
                        .get_mut(&self.selection.destination)
                    {
                        for (id, label) in [("context", "Sidebar"), ("inspector", "Inspector")] {
                            if let Some(litecord_layout::LayoutNode::Panel { visible, .. }) =
                                tree.find(id)
                            {
                                let mut shown = *visible;
                                if ui.checkbox(&mut shown, label).changed() {
                                    let _ = tree.set_visible(id, shown);
                                }
                            }
                        }
                        if self.selection.destination == Destination::Servers
                            && !tree.panel_ids().contains(&"server_list")
                            && ui.button("Add server strip").clicked()
                        {
                            if let Err(e) = tree.insert_panel(
                                litecord_layout::LayoutNode::panel(
                                    "servers_optional",
                                    "server_list",
                                    litecord_layout::Placement::Top,
                                ),
                                "main",
                                litecord_layout::Placement::Top,
                            ) {
                                self.notice = Some(e.to_string());
                            }
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.busy,
                                egui::Button::new("Apply").fill(theme::PRIMARY),
                            )
                            .clicked()
                        {
                            self.saving_layout = true;
                            self.send(Command::SaveProfile(
                                self.profile.clone(),
                                self.layout_token.clone(),
                            ));
                        }
                        if ui
                            .add_enabled(!self.busy, egui::Button::new("Cancel"))
                            .clicked()
                        {
                            if let Some(original) = self.edit_original.take() {
                                self.profile = original;
                                self.layout_dirty = false;
                            }
                        }
                    });
                    if s.layouts.storage_token != self.layout_token {
                        ui.colored_label(
                            theme::PRIORITY,
                            "Saved layout changed elsewhere. Cancel and reload before applying.",
                        );
                    }
                } else {
                    if self.layout_dirty {
                        ui.colored_label(
                            theme::PRIORITY,
                            "Local divider changes could not be saved.",
                        );
                        if ui.button("Reload saved layout").clicked() {
                            self.layout_dirty = false;
                            self.layout_token = s.layouts.storage_token.clone();
                            if let Some(p) = s
                                .layouts
                                .profiles
                                .profiles
                                .iter()
                                .find(|p| p.id == s.layouts.profiles.active_profile_id)
                            {
                                self.profile = p.clone();
                            }
                        }
                    }
                    for p in &s.layouts.profiles.profiles {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    !self.busy,
                                    egui::Button::new(&p.name)
                                        .selected(p.id == s.layouts.profiles.active_profile_id),
                                )
                                .clicked()
                            {
                                self.send(Command::ActivateProfile(
                                    p.id.clone(),
                                    s.layouts.storage_token.clone(),
                                ));
                            }
                        });
                    }
                    ui.separator();
                    ui.add(
                        egui::TextEdit::singleline(&mut self.profile_name).hint_text("Layout name"),
                    );
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(
                                !self.busy && !self.profile_name.trim().is_empty(),
                                egui::Button::new("Create"),
                            )
                            .clicked()
                        {
                            self.send(Command::CreateProfile(
                                self.profile_name.trim().into(),
                                self.layout_token.clone(),
                            ));
                        }
                        if ui
                            .add_enabled(
                                !self.busy && !self.profile_name.trim().is_empty(),
                                egui::Button::new("Duplicate current"),
                            )
                            .clicked()
                        {
                            self.send(Command::DuplicateProfile(
                                self.profile.id.clone(),
                                self.profile_name.trim().into(),
                                self.layout_token.clone(),
                            ));
                        }
                        if ui
                            .add_enabled(
                                !self.busy && !self.profile_name.trim().is_empty(),
                                egui::Button::new("Rename current"),
                            )
                            .clicked()
                        {
                            self.send(Command::RenameProfile(
                                self.profile.id.clone(),
                                self.profile_name.trim().into(),
                                self.layout_token.clone(),
                            ));
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(!self.busy, egui::Button::new("Edit layout"))
                            .clicked()
                        {
                            self.edit_original = Some(self.profile.clone());
                            self.profile_manager = false;
                        }
                        if ui
                            .add_enabled(!self.busy, egui::Button::new("Reset current"))
                            .clicked()
                        {
                            self.send(Command::ResetProfile(
                                self.profile.id.clone(),
                                self.layout_token.clone(),
                            ));
                        }
                        if ui
                            .add_enabled(
                                !self.busy && s.layouts.profiles.profiles.len() > 1,
                                egui::Button::new("Delete current"),
                            )
                            .clicked()
                        {
                            self.send(Command::DeleteProfile(
                                self.profile.id.clone(),
                                self.layout_token.clone(),
                            ));
                        }
                    });
                }
            });
        self.profile_manager = open;
    }
}

fn clock(timestamp: Timestamp) -> String {
    let minutes = (timestamp.as_millis() / 60_000).rem_euclid(1440);
    format!("{:02}:{:02} UTC", minutes / 60, minutes % 60)
}
