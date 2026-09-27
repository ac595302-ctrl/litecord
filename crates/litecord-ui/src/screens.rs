use crate::{bridge::Command, kit, ph, theme, workspace::Workspace};
use eframe::egui::{self, Align2, Rect, Sense, Ui};
use litecord_layout::{Destination, Orientation};
use litecord_types::{actions::RelationshipAction, social::ConversationKind};

impl Workspace {
    pub fn panel(&mut self, ui: &mut Ui, panel: &str, orientation: Orientation) {
        match panel {
            "primary_navigation" => self.navigation(ui, orientation),
            "user_controls" => self.account(ui, orientation),
            "conversation_list" => self.conversations(ui),
            "chat" => self.chat(ui),
            "friends" => self.friends_screen(ui),
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
        let area = ui.max_rect();
        let (attention, omni_attention) = self.snapshot.as_ref().map_or((0, 0), |s| {
            (
                s.inbox.needs_attention.len() + s.inbox.pending_actions.len(),
                s.omni.requests.len() + s.omni.checkins.len(),
            )
        });
        if orientation == Orientation::Horizontal {
            // Docked along the top/bottom: icons with labels in a row.
            let w = (area.width() / (Destination::ALL.len() as f32 + 1.0)).clamp(56.0, 96.0);
            let logo = Rect::from_center_size(
                egui::pos2(area.left() + w * 0.5, area.center().y),
                egui::vec2(36.0, 36.0),
            );
            paint_logo(ui.painter(), logo);
            for (i, d) in Destination::ALL.into_iter().enumerate() {
                let r = Rect::from_min_size(
                    egui::pos2(area.left() + w * (i as f32 + 1.0), area.top()),
                    egui::vec2(w, area.height()),
                );
                self.rail_item(ui, r, d, attention, omni_attention);
            }
            return;
        }
        let logo = Rect::from_center_size(
            egui::pos2(area.center().x, area.top() + 42.0),
            egui::vec2(48.0, 48.0),
        );
        paint_logo(ui.painter(), logo);
        let top = area.top() + 84.0;
        let step = ((area.bottom() - top - 4.0) / Destination::ALL.len() as f32).clamp(52.0, 78.0);
        for (i, d) in Destination::ALL.into_iter().enumerate() {
            let r = Rect::from_min_size(
                egui::pos2(area.left(), top + step * i as f32),
                egui::vec2(area.width(), step),
            );
            self.rail_item(ui, r, d, attention, omni_attention);
        }
    }

    /// One destination on the rail: filled icon over a label, a soft plate
    /// and a blue edge bar when selected (A01).
    fn rail_item(
        &mut self,
        ui: &mut Ui,
        r: Rect,
        d: Destination,
        attention: usize,
        omni_attention: usize,
    ) {
        let response = ui.interact(r, ui.id().with(("rail", d)), Sense::click());
        let selected = self.selection.destination == d;
        let painter = ui.painter();
        let plate = Rect::from_center_size(
            r.center(),
            egui::vec2(
                (r.width() - 16.0).clamp(40.0, 84.0),
                (r.height() - 10.0).min(66.0),
            ),
        );
        if selected {
            painter.rect_filled(plate, 11.0, theme::SELECTED_SOFT);
            painter.rect_filled(
                Rect::from_center_size(
                    egui::pos2(r.left() + 2.0, plate.center().y),
                    egui::vec2(4.0, (plate.height() - 10.0).max(20.0)),
                ),
                2.0,
                theme::PRIMARY,
            );
        } else if response.hovered() {
            painter.rect_filled(plate, 11.0, theme::lerp(theme::RAIL, theme::HOVER, 0.6));
        }
        let compact = r.height() < 64.0;
        let icon_c = egui::pos2(
            r.center().x,
            plate.center().y - if compact { 8.0 } else { 11.0 },
        );
        kit::icon(
            painter,
            icon_c,
            rail_glyph(d),
            if compact { 22.0 } else { 25.0 },
            if selected {
                egui::Color32::from_rgb(98, 150, 255)
            } else {
                egui::Color32::from_rgb(152, 161, 184)
            },
        );
        painter.text(
            egui::pos2(
                r.center().x,
                plate.center().y + if compact { 13.0 } else { 16.0 },
            ),
            Align2::CENTER_CENTER,
            d.label(),
            if selected {
                theme::medium(if compact { 12.0 } else { 14.0 })
            } else {
                theme::regular(if compact { 12.0 } else { 14.0 })
            },
            if selected {
                theme::TEXT
            } else {
                egui::Color32::from_rgb(184, 194, 212)
            },
        );
        let dot = match d {
            Destination::Inbox if attention > 0 => Some(theme::PRIMARY),
            Destination::Omni if omni_attention > 0 => Some(theme::OMNI),
            _ => None,
        };
        if let Some(color) = dot {
            painter.circle_filled(icon_c + egui::vec2(17.0, -11.0), 4.0, color);
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, d.label())
        });
        if response.clicked() {
            self.navigate(d);
        }
        if d == Destination::Servers {
            self.server_flyout(ui, &response);
        }
    }

    fn account(&mut self, ui: &mut Ui, orientation: Orientation) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        let name = self.display(&s.account.display_name);
        let area = ui.max_rect();
        let size = if orientation == Orientation::Horizontal {
            (area.height() - 12.0).clamp(28.0, 44.0)
        } else {
            (area.width() - 44.0).clamp(36.0, 56.0)
        };
        let c = if orientation == Orientation::Horizontal {
            egui::pos2(area.left() + size * 0.5 + 12.0, area.center().y)
        } else {
            egui::pos2(area.center().x, area.bottom() - size * 0.5 - 18.0)
        };
        let r = Rect::from_center_size(c, egui::vec2(size, size));
        let response = ui.interact(r, ui.id().with("account_avatar"), Sense::click());
        theme::paint_avatar(
            ui.painter(),
            c,
            size,
            &name,
            if s.diagnostics.session.is_online() {
                theme::Presence::Online
            } else {
                theme::Presence::Offline
            },
            theme::RAIL,
        );
        let label = format!("{name} · account");
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
        let response = response.on_hover_text(name.clone());
        if response.clicked() {
            self.settings_section = None;
            self.navigate(Destination::Settings);
        }
        response.context_menu(|ui| {
            ui.label(&name);
            if ui.button("Settings").clicked() {
                self.navigate(Destination::Settings);
                ui.close();
            }
        });
    }
    fn conversations(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        if kit::sidebar_title(
            ui,
            "Messages",
            Some((ph::NOTE_PENCIL, "New message: pick a friend")),
        ) {
            self.navigate(Destination::Friends);
        }
        ui.add_space(6.0);
        kit::search(ui, &mut self.filter, "Search conversations...");
        ui.add_space(10.0);
        let waiting = s
            .conversations
            .conversations
            .iter()
            .filter(|r| r.awaiting_reply)
            .count();
        kit::pill_row(
            ui,
            &[
                ("All", None),
                ("Unread", Some(waiting)),
                ("Groups", None),
                ("DMs", None),
            ],
            &mut self.conversation_tab,
        );
        ui.add_space(8.0);
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
            kit::empty(
                ui,
                Some(ph::CHAT_CIRCLE),
                "Nothing here",
                "No conversations match this filter.",
            );
        }
        let now = litecord_types::Timestamp::now();
        let private = self.private();
        egui::ScrollArea::vertical()
            .id_salt("conversation_scroll")
            .auto_shrink([false, false])
            .show_rows(ui, 70.0, rows.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for index in range {
                    let r = rows[index];
                    let selected = Some(r.conversation_id) == self.selection.conversation;
                    let (rect, response) = kit::row(ui, 70.0, selected);
                    let title = self.display(&r.title);
                    let painter = ui.painter();
                    let c = egui::pos2(rect.left() + 12.0 + 24.0, rect.center().y);
                    let ring = if selected {
                        theme::SELECTED
                    } else {
                        theme::SIDEBAR
                    };
                    match r.kind {
                        ConversationKind::GroupDm => {
                            kit::paint_group(painter, c, 48.0, ph::USERS, theme::SECONDARY)
                        }
                        ConversationKind::GuildChannel => {
                            kit::paint_group(painter, c, 48.0, ph::HASH, theme::SECONDARY)
                        }
                        _ => theme::paint_avatar(
                            painter,
                            c,
                            48.0,
                            &title,
                            r.recipient_status.map_or(theme::Presence::None, |p| {
                                theme::Presence::from_status(p.as_str())
                            }),
                            ring,
                        ),
                    }
                    let x = rect.left() + 74.0;
                    let time = r
                        .last_activity_at
                        .map(|t| crate::messages_ui::list_time(t, now))
                        .unwrap_or_default();
                    let time_w = kit::text_width(painter, &time, theme::regular(13.0));
                    let top_y = rect.center().y - 11.0;
                    let bottom_y = rect.center().y + 12.0;
                    kit::text_at(
                        painter,
                        egui::pos2(x, top_y),
                        Align2::LEFT_CENTER,
                        &title,
                        theme::medium(15.5),
                        theme::TEXT,
                        rect.right() - x - time_w - 22.0,
                    );
                    painter.text(
                        egui::pos2(rect.right() - 12.0, top_y + 1.0),
                        Align2::RIGHT_CENTER,
                        time,
                        theme::regular(13.0),
                        theme::MUTED,
                    );
                    let preview = if private {
                        "Hidden in privacy mode".to_owned()
                    } else {
                        r.last_message_preview
                            .clone()
                            .unwrap_or_else(|| "No messages cached yet".to_owned())
                    };
                    kit::text_at(
                        painter,
                        egui::pos2(x, bottom_y),
                        Align2::LEFT_CENTER,
                        &preview,
                        theme::regular(14.0),
                        theme::SECONDARY,
                        rect.right() - x - 40.0,
                    );
                    if r.awaiting_reply {
                        kit::dot(
                            painter,
                            egui::pos2(rect.right() - 20.0, bottom_y),
                            5.0,
                            theme::PRIMARY,
                        );
                    }
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            if r.awaiting_reply {
                                format!("{title}, waiting for your reply")
                            } else {
                                title.clone()
                            },
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

/// Filled Phosphor glyph for a destination.
pub(crate) fn rail_glyph(d: Destination) -> &'static str {
    match d {
        Destination::Home => ph::HOUSE,
        Destination::Messages => ph::CHAT_CIRCLE,
        Destination::Friends => ph::USERS,
        Destination::Servers => ph::SQUARES_FOUR,
        Destination::Voice => ph::WAVEFORM,
        Destination::Inbox => ph::ENVELOPE_SIMPLE,
        Destination::Omni => ph::SPARKLE,
        Destination::Tasks => ph::CHECK_SQUARE,
        Destination::Settings => ph::GEAR_SIX,
    }
}

/// The Litecord mark: a blue gradient tile with an open white ring.
pub(crate) fn paint_logo(painter: &egui::Painter, rect: Rect) {
    theme::gradient_rect(
        painter,
        rect,
        rect.width() * 0.26,
        egui::Color32::from_rgb(38, 148, 255),
        egui::Color32::from_rgb(60, 88, 246),
    );
    let c = rect.center();
    let r = rect.width() * 0.25;
    let stroke = egui::Stroke::new(rect.width() * 0.105, egui::Color32::WHITE);
    let points: Vec<egui::Pos2> = (0..=40)
        .map(|i| {
            let a = (-50.0_f32 + i as f32 * 290.0 / 40.0).to_radians();
            c + egui::vec2(a.cos(), a.sin()) * r
        })
        .collect();
    painter.add(egui::Shape::line(points, stroke));
    painter.circle_filled(
        c + egui::vec2(
            (-62.0_f32).to_radians().cos(),
            (-62.0_f32).to_radians().sin(),
        ) * r,
        stroke.width * 0.5,
        egui::Color32::WHITE,
    );
}
