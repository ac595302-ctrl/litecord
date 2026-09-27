use crate::{bridge::Command, theme, workspace::Workspace};
use eframe::egui::{self, Rect, Ui};
use litecord_core::ports::VoiceControl;
use litecord_layout::Orientation;

impl Workspace {
    pub fn server_list(&mut self, ui: &mut Ui, orientation: Orientation) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        egui::ScrollArea::both()
            .id_salt("server_strip")
            .show(ui, |ui| {
                ui.with_layout(
                    if orientation == Orientation::Horizontal {
                        egui::Layout::left_to_right(egui::Align::Center)
                    } else {
                        egui::Layout::top_down(egui::Align::Center)
                    },
                    |ui| {
                        for g in &s.guilds.guilds {
                            if ui
                                .selectable_label(
                                    self.selected_guild == Some(g.guild.id),
                                    self.display(&g.guild.name),
                                )
                                .clicked()
                            {
                                self.selected_guild = Some(g.guild.id);
                                self.selected_channel = None;
                            }
                        }
                    },
                );
            });
    }
    pub fn channels(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        if self.selected_guild.is_none() {
            self.selected_guild = s.guilds.guilds.first().map(|g| g.guild.id);
        }
        let Some(g) = s
            .guilds
            .guilds
            .iter()
            .find(|g| Some(g.guild.id) == self.selected_guild)
        else {
            ui.heading("Servers");
            ui.label("No cached servers yet.");
            return;
        };
        egui::ComboBox::from_id_salt("server_switcher")
            .selected_text(self.display(&g.guild.name))
            .show_ui(ui, |ui| {
                for guild in &s.guilds.guilds {
                    let label = self.display(&guild.guild.name);
                    if ui
                        .selectable_value(&mut self.selected_guild, Some(guild.guild.id), label)
                        .clicked()
                    {
                        self.selected_channel = None;
                    }
                }
            });
        if self.selected_channel.is_none() {
            self.selected_channel = s
                .guilds
                .guilds
                .iter()
                .find(|guild| Some(guild.guild.id) == self.selected_guild)
                .and_then(|guild| {
                    guild.channels.iter().find(|row| {
                        row.channel.capabilities.contains(
                            litecord_types::social::ChannelCapabilities::READABLE,
                        )
                    })
                })
                .map(|channel| channel.channel.id);
        }
        ui.add_space(16.0);
        theme::section_label(ui, "Channels");
        egui::ScrollArea::vertical()
            .id_salt("channels")
            .show(ui, |ui| {
                for row in &g.channels {
                    if ui
                        .selectable_label(
                            self.selected_channel == Some(row.channel.id),
                            format!("# {}", self.display(&row.channel.name)),
                        )
                        .clicked()
                    {
                        self.selected_channel = Some(row.channel.id);
                        if row.channel.capabilities.contains(
                            litecord_types::social::ChannelCapabilities::READABLE,
                        ) && s.diagnostics
                            .capabilities
                            .is_usable(litecord_types::capability::Capability::GuildMessages)
                        {
                            self.open_conversation(litecord_types::ConversationId(
                                row.channel.id.get(),
                            ));
                        }
                    }
                }
            });
    }
    pub fn server_content(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        if s.diagnostics.backend_mode == litecord_types::capability::BackendMode::UserSession
            && !s.diagnostics.session.is_online()
        {
            theme::empty_state(
                ui,
                "No Discord account connected",
                "Connect an account in Settings to load your servers and channels.",
            );
            if ui.button("Open Discord settings").clicked() {
                self.navigate(litecord_layout::Destination::Settings);
            }
            return;
        }
        let channel = s
            .guilds
            .guilds
            .iter()
            .flat_map(|g| &g.channels)
            .find(|r| Some(r.channel.id) == self.selected_channel);
        let Some(row) = channel else {
            ui.heading("Servers");
            ui.label(
                "Choose a channel from the sidebar. Hover over Servers to switch communities.",
            );
            return;
        };
        ui.heading(format!("# {}", self.display(&row.channel.name)));
        ui.separator();
        ui.add_space(24.0);
        theme::chip(ui, row.channel.access.as_str(), theme::MUTED);
        if !row.channel.capabilities.contains(
            litecord_types::social::ChannelCapabilities::READABLE,
        ) {
            ui.label("This channel does not contain messages. Choose a text channel to read its history.");
        } else if s.diagnostics
            .capabilities
            .is_usable(litecord_types::capability::Capability::GuildMessages)
        {
            if ui.button("Open conversation in Litecord").clicked() {
                self.open_conversation(litecord_types::ConversationId(row.channel.id.get()));
            }
        } else {
            ui.label("Messages are not available through this connection.");
        }
        if ui.button("Open channel in Discord").clicked() {
            ui.ctx()
                .open_url(egui::OpenUrl::new_tab(row.open_in_discord_url.clone()));
        }
    }
    pub fn voice_sidebar(&mut self, ui: &mut Ui) {
        ui.heading("Voice");
        ui.add_space(12.0);
        theme::section_label(ui, "Known rooms");
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        if !s.voice.voice_supported {
            ui.label("Voice is unavailable through this Discord connection.");
            return;
        }
        if s.rooms.rooms.is_empty() {
            ui.label("No rooms discovered yet.");
            ui.add(egui::TextEdit::singleline(&mut self.lobby_text).hint_text("Known lobby ID"));
            let id = self
                .lobby_text
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|id| *id > 0);
            if ui
                .add_enabled(
                    !self.busy && s.voice.voice_supported && id.is_some(),
                    egui::Button::new("Join by ID"),
                )
                .clicked()
            {
                if let Some(id) = id {
                    self.selected_lobby = Some(litecord_types::LobbyId(id));
                    self.send(Command::Voice(VoiceControl::JoinLobby(
                        litecord_types::LobbyId(id),
                    )));
                }
            }
        }
        for room in &s.rooms.rooms {
            if ui
                .selectable_label(
                    self.selected_lobby == Some(room.lobby.id),
                    self.display(&room.title),
                )
                .clicked()
            {
                self.selected_lobby = Some(room.lobby.id);
            }
        }
        ui.add_space(16.0);
        theme::section_label(ui, "Voice session");
        ui.label(if s.voice.state.connected {
            "Connected"
        } else {
            "Not connected"
        });
    }
    pub fn voice_room(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        if !s.voice.voice_supported {
            theme::empty_state(
                ui,
                "Voice is unavailable",
                "This Discord connection does not provide voice rooms or device controls.",
            );
            return;
        }
        let room = s
            .rooms
            .rooms
            .iter()
            .find(|r| Some(r.lobby.id) == self.selected_lobby.or(s.voice.state.lobby_id))
            .or_else(|| s.rooms.rooms.first());
        if self.selected_lobby.is_none() {
            self.selected_lobby = room.map(|r| r.lobby.id);
        }
        let title = room
            .map(|r| self.display(&r.title))
            .unwrap_or_else(|| "Voice room".into());
        // The room is one dockable unit; card, tab, settings, and bar stay together.
        let full = ui.available_rect_before_wrap();
        ui.horizontal(|ui| {
            ui.heading(title);
            theme::chip(
                ui,
                if s.voice.state.connected {
                    "Connected"
                } else {
                    "Not connected"
                },
                theme::MUTED,
            );
        });
        ui.add_space(12.0);
        let card_width = ((full.width() - 24.0) / 4.0).max(80.0);
        egui::ScrollArea::horizontal()
            .id_salt("voice_people")
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if s.voice.state.participants.is_empty() {
                        ui.label("Participants appear when a voice session is connected.");
                    }
                    for p in &s.voice.state.participants {
                        let name = s
                            .voice
                            .participant_names
                            .iter()
                            .find(|(id, _)| *id == p.user_id)
                            .map(|(_, name)| self.display(name))
                            .unwrap_or_else(|| "Unknown user".into());
                        egui::Frame::new()
                            .fill(if p.speaking {
                                theme::SELECTED
                            } else {
                                theme::RAISED
                            })
                            .corner_radius(8)
                            .inner_margin(12)
                            .show(ui, |ui| {
                                ui.set_width(card_width - 24.0);
                                ui.set_min_height(140.0);
                                ui.vertical_centered(|ui| {
                                    theme::avatar(ui, &name, 56.0, !p.self_muted);
                                    ui.label(egui::RichText::new(name).strong());
                                    ui.label(
                                        egui::RichText::new(if p.speaking {
                                            "Speaking now"
                                        } else if p.self_muted {
                                            "Muted"
                                        } else {
                                            "In voice"
                                        })
                                        .size(12.0)
                                        .color(
                                            if p.speaking {
                                                theme::OMNI
                                            } else {
                                                theme::MUTED
                                            },
                                        ),
                                    );
                                });
                            });
                    }
                });
            });
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            for (i, label) in ["All", "Messages", "Files", "Members", "Omni"]
                .into_iter()
                .enumerate()
            {
                if ui.selectable_label(self.voice_tab == i, label).clicked() {
                    self.voice_tab = i;
                }
            }
        });
        ui.separator();
        let body = Rect::from_min_max(
            ui.cursor().min,
            egui::pos2(
                full.right(),
                (full.bottom() - 82.0).max(ui.cursor().min.y + 100.0),
            ),
        );
        let settings_width = if body.width() > 650.0 { 240.0 } else { 0.0 };
        let content = Rect::from_min_max(
            body.min,
            egui::pos2(body.right() - settings_width - 12.0, body.bottom()),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("voice_tab_body")
                .max_rect(content),
        );
        child.set_clip_rect(content.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical()
            .id_salt("voice_tab_scroll")
            .show(&mut child, |ui| match self.voice_tab {
                0 | 1 => {
                    theme::section_label(ui, "Room messages");
                    if let Some(id) = room.and_then(|r| r.conversation_id) {
                        if ui.button("Open room conversation").clicked() {
                            self.open_conversation(id);
                        }
                    } else {
                        ui.label("No cached conversation is associated with this room.");
                    }
                }
                2 => {
                    theme::section_label(ui, "Files");
                    ui.label("Shared files require room conversation history from the backend.");
                }
                3 => {
                    theme::section_label(ui, "Members");
                    for (_, name) in &s.voice.participant_names {
                        ui.label(self.display(name));
                    }
                }
                _ => {
                    theme::section_label(ui, "Omni");
                    ui.label("Room summaries and transcription are not available on this backend.");
                }
            });
        if settings_width > 0.0 {
            let settings = Rect::from_min_max(
                egui::pos2(body.right() - settings_width, body.top()),
                body.max,
            );
            let mut settings_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt("voice_settings")
                    .max_rect(settings),
            );
            settings_ui.set_clip_rect(settings.intersect(ui.clip_rect()));
            self.voice_settings(&mut settings_ui);
        }
        let bar = Rect::from_min_max(egui::pos2(full.left(), full.bottom() - 72.0), full.max);
        let mut bottom = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("voice_bottom_bar")
                .max_rect(bar)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        bottom.set_clip_rect(bar.intersect(ui.clip_rect()));
        bottom.add_enabled_ui(!self.busy && s.voice.voice_supported, |ui| {
            if ui
                .button(if s.voice.state.muted {
                    "Unmute"
                } else {
                    "Mute"
                })
                .clicked()
            {
                self.send(Command::Voice(VoiceControl::SetMuted(!s.voice.state.muted)));
            }
            if ui
                .button(if s.voice.state.deafened {
                    "Undeafen"
                } else {
                    "Deafen"
                })
                .clicked()
            {
                self.send(Command::Voice(VoiceControl::SetDeafened(
                    !s.voice.state.deafened,
                )));
            }
            if ui
                .button(if s.voice.state.noise_suppression {
                    "Noise suppression on"
                } else {
                    "Noise suppression off"
                })
                .clicked()
            {
                self.send(Command::Voice(VoiceControl::SetNoiseSuppression(
                    !s.voice.state.noise_suppression,
                )));
            }
            if ui
                .button(if s.voice.state.push_to_talk {
                    "Push to talk on"
                } else {
                    "Push to talk off"
                })
                .clicked()
            {
                self.send(Command::Voice(VoiceControl::SetPushToTalk(
                    !s.voice.state.push_to_talk,
                )));
            }
            if s.voice.state.connected {
                if ui
                    .button(egui::RichText::new("Disconnect").color(theme::PRIORITY))
                    .clicked()
                {
                    self.send(Command::Voice(VoiceControl::Leave));
                }
            } else if ui
                .add_enabled(
                    self.selected_lobby.is_some(),
                    egui::Button::new("Join room").fill(theme::PRIMARY),
                )
                .clicked()
            {
                if let Some(id) = self.selected_lobby {
                    self.send(Command::Voice(VoiceControl::JoinLobby(id)));
                }
            }
        });
        if !s.voice.voice_supported {
            bottom.label("Voice unavailable on this backend");
        }
    }
    fn voice_settings(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("voice_settings_scroll")
            .show(ui, |ui| {
                theme::section_label(ui, "Voice settings");
                ui.label("Input device");
                ui.label(
                    s.voice
                        .state
                        .input_device
                        .as_deref()
                        .unwrap_or("System default"),
                );
                ui.label("Output device");
                ui.label(
                    s.voice
                        .state
                        .output_device
                        .as_deref()
                        .unwrap_or("System default"),
                );
                ui.label(
                    egui::RichText::new("Device discovery is not exposed by this backend.")
                        .size(12.0)
                        .color(theme::MUTED),
                );
                let enabled = !self.busy && s.voice.voice_supported;
                let draft_id = ui.make_persistent_id("voice_output_volume_draft");
                let draft = if enabled {
                    ui.ctx().data(|data| data.get_temp::<f32>(draft_id))
                } else {
                    ui.ctx().data_mut(|data| data.remove::<f32>(draft_id));
                    None
                };
                let mut volume = draft.unwrap_or(s.voice.state.output_volume);
                let response = ui.add_enabled(
                    enabled,
                    egui::Slider::new(&mut volume, 0.0..=1.0).text("Output"),
                );
                let pointer_down = ui.ctx().input(|input| input.pointer.primary_down());
                let pointer_adjusting = pointer_down && response.hovered();
                if enabled && (response.drag_stopped() || (draft.is_some() && !pointer_down)) {
                    ui.ctx().data_mut(|data| data.remove::<f32>(draft_id));
                    if volume != s.voice.state.output_volume {
                        self.send(Command::Voice(VoiceControl::SetOutputVolume(volume)));
                    }
                } else if enabled && (response.dragged() || pointer_adjusting) {
                    if response.changed() || draft.is_some() {
                        ui.ctx().data_mut(|data| data.insert_temp(draft_id, volume));
                    }
                } else if enabled && response.changed() {
                    ui.ctx().data_mut(|data| data.remove::<f32>(draft_id));
                    self.send(Command::Voice(VoiceControl::SetOutputVolume(volume)));
                }
                ui.separator();
                theme::section_label(ui, "In this room");
                for (_, name) in &s.voice.participant_names {
                    ui.label(self.display(name));
                }
            });
    }
}
