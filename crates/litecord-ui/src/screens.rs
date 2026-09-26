use crate::{bridge::Command, icons, theme, workspace::Workspace};
use eframe::egui::{self, Align2, FontId, Rect, Sense, Ui};
use litecord_app::view::FriendRow;
use litecord_layout::{Destination, Orientation};
use litecord_types::{
    actions::RelationshipAction,
    capability::Capability,
    social::{ConversationKind, RelationshipKind},
};

impl Workspace {
    pub fn panel(&mut self, ui: &mut Ui, panel: &str, orientation: Orientation) {
        match panel {
            "primary_navigation" => self.navigation(ui, orientation),
            "user_controls" => self.account(ui, orientation),
            "conversation_list" => self.conversations(ui),
            "chat" => self.chat(ui),
            "friends" => self.friends(ui),
            "context_inspector" => self.destination_inspector_v2(ui),
            "home" => self.home_screen(ui),
            "agent_inbox" => self.inbox_screen(ui),
            "tasks" => self.tasks_screen(ui),
            "memory" => self.memory_screen_v2(ui),
            "settings" => self.settings_screen(ui),
            "server_list" => self.server_list(ui, orientation),
            "channel_list" => self.channels(ui),
            "server_content" => self.server_content(ui),
            "voice_room" => self.voice_room(ui),
            "contextual_sidebar" => self.context_sidebar_v2(ui),
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
                    theme::avatar_presence(
                        &mut child,
                        &title,
                        36.0,
                        r.recipient_status.map_or(theme::Presence::None, |p| {
                            theme::Presence::from_status(p.as_str())
                        }),
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
        self.chat_view(ui);
    }
    fn friends(&mut self, ui: &mut Ui) {
        // Filters live in the contextual sidebar; the heading names the view.
        let view =
            ["Online", "All friends", "Pending requests", "Blocked"][self.friends_tab.min(3)];
        theme::page_header(ui, "Friends", Some(view));
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
            theme::empty_state(
                ui,
                "Nobody here",
                "Contacts in this view appear as Discord reports them.",
            );
        }
        egui::ScrollArea::vertical()
            .id_salt("friends_scroll")
            .show_rows(ui, 72.0, rows.len(), |ui, range| {
                for i in range {
                    let r = rows[i];
                    let selected = self.selection.contact == Some(r.user_id);
                    egui::Frame::new()
                        .fill(if selected {
                            theme::SELECTED
                        } else {
                            theme::WORKSPACE
                        })
                        .stroke(egui::Stroke::new(
                            1.0,
                            if selected {
                                theme::PRIMARY.gamma_multiply(0.6)
                            } else {
                                theme::BORDER
                            },
                        ))
                        .corner_radius(6)
                        .inner_margin(8)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let name =
                                    self.display(r.alias.as_deref().unwrap_or(&r.display_name));
                                let presence = if r.relationship == RelationshipKind::Friend {
                                    theme::Presence::from_status(r.status.as_str())
                                } else {
                                    theme::Presence::None
                                };
                                theme::avatar_presence(ui, &name, 40.0, presence);
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
                                                presence_text(r.status.as_str())
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
                    let layout_busy = self.layout_busy();
                    let mut changed = false;
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
                                if ui
                                    .add_enabled(
                                        !layout_busy,
                                        egui::Checkbox::new(&mut shown, label),
                                    )
                                    .changed()
                                    && tree.set_visible(id, shown).is_ok()
                                {
                                    changed = true;
                                }
                            }
                        }
                        if self.selection.destination == Destination::Servers
                            && !tree.panel_ids().contains(&"server_list")
                            && ui
                                .add_enabled(!layout_busy, egui::Button::new("Add server strip"))
                                .clicked()
                        {
                            changed = true;
                            if let Err(e) = tree.insert_panel(
                                litecord_layout::LayoutNode::panel(
                                    "servers_optional",
                                    "server_list",
                                    litecord_layout::Placement::Top,
                                ),
                                "main",
                                litecord_layout::Placement::Top,
                            ) {
                                changed = false;
                                self.notice = Some(e.to_string());
                            }
                        }
                    }
                    if changed {
                        self.layout_dirty = true;
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self.busy,
                                egui::Button::new("Apply").fill(theme::PRIMARY),
                            )
                            .clicked()
                        {
                            self.apply_edit();
                        }
                        if ui
                            .add_enabled(!self.busy, egui::Button::new("Cancel"))
                            .clicked()
                        {
                            self.cancel_edit();
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

/// Readable presence text for lists.
fn presence_text(status: &str) -> &'static str {
    match status {
        "online" => "Online",
        "idle" => "Idle",
        "dnd" => "Do not disturb",
        "offline" | "invisible" => "Offline",
        _ => "",
    }
}
