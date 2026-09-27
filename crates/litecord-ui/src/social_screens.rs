//! Servers (mock A04) and Voice (mock A05–A08).
//!
//! Servers: a server list with channels in the sidebar; the selected
//! channel's chat when the bot serves it (the "linked" chat), otherwise the
//! channel's details with Open in Discord; a server inspector with
//! capabilities. Voice: rooms in the sidebar; the room stage with
//! participant cards, tabs and the round control bar; room details and
//! voice settings in the inspector.

use crate::{bridge::Command, kit, ph, theme, workspace::Workspace};
use eframe::egui::{self, Align2, Color32, Rect, Ui};
use litecord_core::ports::VoiceControl;
use litecord_layout::Orientation;
use litecord_types::ConversationId;

impl Workspace {
    // ---- Servers -----------------------------------------------------------------

    /// Optional server strip (docked panel): server tiles in a row/column.
    pub fn server_list(&mut self, ui: &mut Ui, orientation: Orientation) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        let horizontal = orientation == Orientation::Horizontal;
        egui::ScrollArea::both()
            .id_salt("server_strip")
            .show(ui, |ui| {
                let layout = if horizontal {
                    egui::Layout::left_to_right(egui::Align::Center)
                } else {
                    egui::Layout::top_down(egui::Align::Center)
                };
                ui.with_layout(layout, |ui| {
                    for g in &s.guilds.guilds {
                        let name = self.display(&g.guild.name);
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(56.0, 56.0), egui::Sense::click());
                        let selected = self.selected_guild == Some(g.guild.id);
                        if selected {
                            ui.painter().rect_stroke(
                                rect.expand(2.0),
                                16.0,
                                egui::Stroke::new(2.0_f32, theme::PRIMARY),
                                egui::StrokeKind::Outside,
                            );
                        }
                        kit::paint_initials_tile(
                            ui.painter(),
                            rect,
                            &name,
                            kit::tint_for(&name),
                            14.0,
                        );
                        if resp.on_hover_text(&name).clicked() {
                            self.select_guild(g.guild.id);
                        }
                    }
                });
            });
    }

    fn select_guild(&mut self, id: litecord_types::GuildId) {
        self.selected_guild = Some(id);
        self.selected_channel = None;
    }

    fn select_channel(&mut self, s: &crate::bridge::Snapshot, id: litecord_types::ChannelId) {
        self.selected_channel = Some(id);
        // A channel the bot serves is a conversation: show it as a chat.
        let conv = ConversationId(id.get());
        if s.conversations
            .conversations
            .iter()
            .any(|c| c.conversation_id == conv)
        {
            self.selection.conversation = Some(conv);
            self.selection.contact = None;
            self.selection.before = None;
            self.request();
        }
    }

    /// Sidebar: your servers as cards; the selected one's channels below.
    pub fn channels(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        if self.selected_guild.is_none() {
            self.selected_guild = s.guilds.guilds.first().map(|g| g.guild.id);
        }
        // Open a channel straight away (a linked one when there is one) so the
        // main area is never an empty placeholder.
        if self.selected_channel.is_none() {
            let rows = s
                .guilds
                .guilds
                .iter()
                .find(|g| Some(g.guild.id) == self.selected_guild)
                .map(|g| &g.channels);
            let pick = rows.and_then(|rows| {
                rows.iter()
                    .find(|r| {
                        s.conversations
                            .conversations
                            .iter()
                            .any(|c| c.conversation_id == ConversationId(r.channel.id.get()))
                    })
                    .or_else(|| rows.first())
                    .map(|r| r.channel.id)
            });
            if let Some(id) = pick {
                self.select_channel(&s, id);
            }
        }
        // A04: the server list sits on a glassy, blue-edged card.
        let glass = ui.max_rect().expand2(egui::vec2(8.0, 6.0));
        ui.painter().rect(
            glass,
            18.0,
            theme::lerp(theme::SIDEBAR, theme::PRIMARY, 0.07),
            egui::Stroke::new(1.0_f32, theme::lerp(theme::SIDEBAR, theme::PRIMARY, 0.45)),
            egui::StrokeKind::Inside,
        );
        let mut glass_ui =
            ui.new_child(egui::UiBuilder::new().max_rect(glass.shrink2(egui::vec2(14.0, 12.0))));
        let ui = &mut glass_ui;
        if kit::sidebar_title(
            ui,
            "Your servers",
            Some((ph::PLUS, "Join or create a server in Discord")),
        ) {
            ui.ctx()
                .open_url(egui::OpenUrl::new_tab("https://discord.com/channels/@me"));
        }
        ui.add_space(6.0);
        kit::search(ui, &mut self.filter, "Search servers...");
        ui.add_space(8.0);
        if s.guilds.guilds.is_empty() {
            kit::empty(
                ui,
                Some(ph::SQUARES_FOUR),
                "No cached servers yet",
                "Servers appear as Discord reports them.",
            );
            return;
        }
        let needle = self.filter.trim().to_lowercase();
        egui::ScrollArea::vertical()
            .id_salt("channels")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                for g in &s.guilds.guilds {
                    let name = self.display(&g.guild.name);
                    if !needle.is_empty() && !name.to_lowercase().contains(&needle) {
                        continue;
                    }
                    let selected = self.selected_guild == Some(g.guild.id);
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 76.0),
                        egui::Sense::click(),
                    );
                    let painter = ui.painter();
                    if selected {
                        painter.rect(
                            rect,
                            14.0,
                            theme::lerp(theme::SIDEBAR, theme::PRIMARY, 0.18),
                            egui::Stroke::new(
                                1.0_f32,
                                theme::lerp(theme::SIDEBAR, theme::PRIMARY, 0.6),
                            ),
                            egui::StrokeKind::Inside,
                        );
                    } else if resp.hovered() {
                        painter.rect_filled(
                            rect,
                            14.0,
                            theme::lerp(theme::SIDEBAR, theme::HOVER, 0.6),
                        );
                    }
                    let tile = Rect::from_center_size(
                        egui::pos2(rect.left() + 42.0, rect.center().y),
                        egui::vec2(56.0, 56.0),
                    );
                    kit::paint_initials_tile(painter, tile, &name, kit::tint_for(&name), 14.0);
                    let x = tile.right() + 16.0;
                    kit::text_at(
                        painter,
                        egui::pos2(x, rect.center().y - 11.0),
                        Align2::LEFT_CENTER,
                        &name,
                        theme::medium(16.5),
                        theme::TEXT,
                        rect.right() - x - 30.0,
                    );
                    kit::icon(
                        painter,
                        egui::pos2(x + 8.0, rect.center().y + 13.0),
                        ph::HASH,
                        13.0,
                        theme::MUTED,
                    );
                    kit::text_at(
                        painter,
                        egui::pos2(x + 22.0, rect.center().y + 13.0),
                        Align2::LEFT_CENTER,
                        &format!(
                            "{} channel{}",
                            g.channels.len(),
                            if g.channels.len() == 1 { "" } else { "s" }
                        ),
                        theme::regular(13.5),
                        theme::MUTED,
                        rect.right() - x - 40.0,
                    );
                    if resp.clicked() {
                        self.select_guild(g.guild.id);
                    }
                    if selected {
                        for row in &g.channels {
                            let active = self.selected_channel == Some(row.channel.id);
                            let (r, cr) = kit::row(ui, 36.0, active);
                            let painter = ui.painter();
                            let linked =
                                s.conversations.conversations.iter().any(|c| {
                                    c.conversation_id == ConversationId(row.channel.id.get())
                                });
                            kit::icon(
                                painter,
                                egui::pos2(r.left() + 26.0, r.center().y),
                                ph::HASH,
                                16.0,
                                if active { theme::TEXT } else { theme::MUTED },
                            );
                            kit::text_at(
                                painter,
                                egui::pos2(r.left() + 44.0, r.center().y),
                                Align2::LEFT_CENTER,
                                &self.display(&row.channel.name),
                                theme::regular(15.0),
                                if active {
                                    theme::TEXT
                                } else {
                                    theme::SECONDARY
                                },
                                r.width() - 80.0,
                            );
                            if linked {
                                kit::icon(
                                    painter,
                                    egui::pos2(r.right() - 16.0, r.center().y),
                                    ph::LINK_SIMPLE,
                                    14.0,
                                    theme::OMNI,
                                );
                            }
                            if cr
                                .on_hover_text(if linked {
                                    "Linked: readable in Litecord"
                                } else {
                                    "Opens in Discord"
                                })
                                .clicked()
                            {
                                self.select_channel(&s, row.channel.id);
                            }
                        }
                        ui.add_space(4.0);
                    }
                }
            });
    }

    /// Main: the linked channel chat, or the channel's details.
    pub fn server_content(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        let channel = s
            .guilds
            .guilds
            .iter()
            .flat_map(|g| &g.channels)
            .find(|r| Some(r.channel.id) == self.selected_channel)
            .cloned();
        let area = ui.max_rect().shrink2(egui::vec2(22.0, 16.0));
        let Some(row) = channel else {
            let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(area));
            kit::page_title(
                &mut inner,
                "Servers",
                Some("Choose a channel from the sidebar."),
            );
            kit::empty(
                &mut inner,
                Some(ph::HASH),
                "No channel selected",
                "Channels your bot serves open here as chats; others open in Discord.",
            );
            return;
        };
        let conv = ConversationId(row.channel.id.get());
        let linked = s.chat.as_ref().is_some_and(|c| c.conversation_id == conv);
        if linked {
            self.chat_view(ui);
            return;
        }
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(area));
        let (hdr, _) = inner.allocate_exact_size(
            egui::vec2(inner.available_width(), 56.0),
            egui::Sense::hover(),
        );
        let painter = inner.painter();
        kit::icon(
            painter,
            hdr.left_center() + egui::vec2(14.0, 0.0),
            ph::HASH,
            26.0,
            theme::SECONDARY,
        );
        painter.text(
            hdr.left_center() + egui::vec2(40.0, -8.0),
            Align2::LEFT_CENTER,
            self.display(&row.channel.name),
            theme::semibold(22.0),
            theme::TEXT,
        );
        painter.text(
            hdr.left_center() + egui::vec2(40.0, 14.0),
            Align2::LEFT_CENTER,
            row.channel.access.as_str().replace('_', " "),
            theme::regular(14.0),
            theme::SECONDARY,
        );
        kit::divider(&mut inner);
        inner.add_space(18.0);
        kit::card(&mut inner, |ui| {
            ui.horizontal(|ui| {
                kit::bubble(ui, ph::LINK_SIMPLE, kit::BLUE, 44.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    kit::label(ui, "This channel opens in Discord", theme::medium(16.0), theme::TEXT);
                    kit::para(ui, "Litecord reads server channels through your bot. Add the bot to this server to read and reply here; until then, the channel opens in Discord.", theme::regular(14.0), theme::SECONDARY);
                });
            });
            ui.add_space(6.0);
            if kit::button_ex(
                ui,
                kit::Kind::Primary,
                Some(ph::DISCORD_LOGO),
                "Open in Discord",
                36.0,
                true,
            )
            .clicked()
            {
                ui.ctx()
                    .open_url(egui::OpenUrl::new_tab(row.open_in_discord_url.clone()));
            }
        });
    }

    /// A04 server inspector: identity, actions, capabilities, Open in Discord.
    pub(crate) fn server_inspector(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        let Some(g) = s
            .guilds
            .guilds
            .iter()
            .find(|g| Some(g.guild.id) == self.selected_guild)
            .cloned()
        else {
            kit::empty(
                ui,
                Some(ph::SQUARES_FOUR),
                "No server selected",
                "Pick a server to see its details.",
            );
            return;
        };
        let name = self.display(&g.guild.name);
        let tint = kit::tint_for(&name);
        egui::ScrollArea::vertical()
            .id_salt("server_inspector")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Banner with the server tile overlapping it.
                let (banner, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 132.0),
                    egui::Sense::hover(),
                );
                let b =
                    Rect::from_min_max(banner.min, egui::pos2(banner.right(), banner.top() + 96.0));
                theme::gradient_rect(
                    ui.painter(),
                    b,
                    14.0,
                    theme::lerp(tint.fg, Color32::from_rgb(40, 30, 90), 0.55),
                    theme::lerp(tint.fg, Color32::from_rgb(12, 16, 32), 0.75),
                );
                let tile = Rect::from_min_size(
                    egui::pos2(b.left() + 12.0, b.bottom() - 40.0),
                    egui::vec2(72.0, 72.0),
                );
                ui.painter()
                    .rect_filled(tile.expand(3.0), 18.0, theme::INSPECTOR);
                kit::paint_initials_tile(ui.painter(), tile, &name, tint, 16.0);
                ui.add_space(6.0);
                kit::label(ui, &name, theme::semibold(22.0), theme::TEXT);
                kit::label(
                    ui,
                    format!(
                        "{} channel{}",
                        g.channels.len(),
                        if g.channels.len() == 1 { "" } else { "s" }
                    ),
                    theme::regular(14.5),
                    theme::SECONDARY,
                );
                ui.add_space(10.0);
                let url = g
                    .channels
                    .first()
                    .map(|c| c.open_in_discord_url.clone())
                    .unwrap_or_else(|| "https://discord.com/channels/@me".into());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let w = ((ui.available_width() - 12.0) / 4.0).clamp(52.0, 84.0);
                    for (glyph, label, tip) in [
                        (ph::USER_PLUS, "Invite", "Invites are created in Discord"),
                        (
                            ph::BELL,
                            "Notifications",
                            "Notification settings live in Discord",
                        ),
                        (ph::GEAR_SIX, "Settings", "Server settings live in Discord"),
                        (ph::DOTS_THREE, "More", "Open the server in Discord"),
                    ] {
                        if kit::round_action(ui, glyph, label, w, true)
                            .on_hover_text(tip)
                            .clicked()
                        {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(url.clone()));
                        }
                    }
                });
                ui.add_space(10.0);
                kit::divider(ui);
                ui.add_space(10.0);
                kit::section(ui, "Server capabilities", None, None);
                let bot = s.diagnostics.bot.as_ref();
                let linked = g
                    .channels
                    .iter()
                    .filter(|c| {
                        s.conversations
                            .conversations
                            .iter()
                            .any(|x| x.conversation_id == ConversationId(c.channel.id.get()))
                    })
                    .count();
                let rows: [(&str, &str, String); 4] = [
                    (ph::USERS, "Presence", "See who's online in Discord".into()),
                    (
                        ph::LINK_SIMPLE,
                        "Linked chat",
                        if linked > 0 {
                            format!(
                                "{linked} channel{} readable here",
                                if linked == 1 { "" } else { "s" }
                            )
                        } else if bot.is_some() {
                            "Bot not in this server".into()
                        } else {
                            "Connect a bot to read channels".into()
                        },
                    ),
                    (
                        ph::WAVEFORM,
                        "Voice",
                        "Voice channels open in Discord".into(),
                    ),
                    (
                        ph::ARROW_SQUARE_OUT,
                        "Full history",
                        if linked > 0 {
                            "Sync it from a channel's details".into()
                        } else {
                            "Archives available in Discord".into()
                        },
                    ),
                ];
                for (i, (g_, t, sub)) in rows.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 50.0),
                        egui::Sense::click(),
                    );
                    let painter = ui.painter();
                    painter.rect(
                        rect,
                        10.0,
                        if resp.hovered() {
                            theme::lerp(theme::CARD, theme::HOVER, 0.6)
                        } else {
                            theme::CARD
                        },
                        egui::Stroke::new(1.0_f32, theme::BORDER),
                        egui::StrokeKind::Inside,
                    );
                    kit::icon_o(
                        painter,
                        rect.left_center() + egui::vec2(20.0, 0.0),
                        g_,
                        20.0,
                        if i == 1 && linked > 0 {
                            theme::OMNI
                        } else {
                            theme::SECONDARY
                        },
                    );
                    kit::text_at(
                        painter,
                        egui::pos2(rect.left() + 44.0, rect.center().y - 9.0),
                        Align2::LEFT_CENTER,
                        t,
                        theme::medium(14.0),
                        theme::TEXT,
                        rect.width() - 70.0,
                    );
                    kit::text_at(
                        painter,
                        egui::pos2(rect.left() + 44.0, rect.center().y + 10.0),
                        Align2::LEFT_CENTER,
                        sub,
                        theme::regular(12.5),
                        theme::MUTED,
                        rect.width() - 70.0,
                    );
                    kit::chevron(
                        painter,
                        rect.right_center() - egui::vec2(14.0, 0.0),
                        theme::MUTED,
                    );
                    if resp.clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url.clone()));
                    }
                }
                ui.add_space(10.0);
                let w = ui.available_width();
                let r = kit::button_wide(
                    ui,
                    kit::Kind::Secondary,
                    Some(ph::DISCORD_LOGO),
                    "Open in Discord",
                    w,
                    40.0,
                );
                ui.painter().rect_stroke(
                    r.rect,
                    8.0,
                    egui::Stroke::new(1.0_f32, theme::lerp(theme::BORDER, theme::PRIMARY, 0.6)),
                    egui::StrokeKind::Inside,
                );
                if r.clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            });
    }

    // ---- Voice -------------------------------------------------------------------

    pub fn voice_sidebar(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        kit::sidebar_title(ui, "Voice", None);
        ui.add_space(6.0);
        kit::search(ui, &mut self.filter, "Search rooms or people...");
        ui.add_space(10.0);
        kit::section(ui, "Active rooms", Some(s.rooms.rooms.len()), None);
        if s.rooms.rooms.is_empty() {
            kit::para(ui, "No rooms discovered yet. Rooms appear when Discord reports them; you can also join a known lobby by ID.", theme::regular(13.5), theme::MUTED);
            ui.add_space(6.0);
            kit::search_sized(ui, &mut self.lobby_text, "Known lobby ID", 34.0);
            ui.add_space(6.0);
            let id = self
                .lobby_text
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|id| *id > 0);
            if kit::button_ex(
                ui,
                kit::Kind::Primary,
                Some(ph::SIGN_IN),
                "Join by ID",
                32.0,
                !self.busy && s.voice.voice_supported && id.is_some(),
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
        let needle = self.filter.trim().to_lowercase();
        for room in &s.rooms.rooms {
            let title = self.display(&room.title);
            if !needle.is_empty() && !title.to_lowercase().contains(&needle) {
                continue;
            }
            let selected = self.selected_lobby == Some(room.lobby.id);
            let live = s.voice.state.connected && s.voice.state.lobby_id == Some(room.lobby.id);
            let (rect, resp) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 68.0), egui::Sense::click());
            let painter = ui.painter();
            if selected {
                painter.rect(
                    rect,
                    12.0,
                    theme::lerp(theme::SIDEBAR, theme::PRIMARY, 0.16),
                    egui::Stroke::new(1.0_f32, theme::lerp(theme::SIDEBAR, theme::PRIMARY, 0.6)),
                    egui::StrokeKind::Inside,
                );
            } else if resp.hovered() {
                painter.rect_filled(rect, 12.0, theme::lerp(theme::SIDEBAR, theme::HOVER, 0.6));
            }
            kit::paint_tile(
                painter,
                Rect::from_center_size(
                    egui::pos2(rect.left() + 32.0, rect.center().y),
                    egui::vec2(46.0, 46.0),
                ),
                ph::WAVEFORM,
                kit::BLUE,
                12.0,
            );
            let x = rect.left() + 66.0;
            kit::text_at(
                painter,
                egui::pos2(x, rect.center().y - 10.0),
                Align2::LEFT_CENTER,
                &title,
                theme::medium(15.5),
                theme::TEXT,
                rect.right() - x - 36.0,
            );
            let sub = if live {
                format!("{} in voice", s.voice.state.participants.len())
            } else {
                "Tap to open".to_owned()
            };
            kit::text_at(
                painter,
                egui::pos2(x, rect.center().y + 12.0),
                Align2::LEFT_CENTER,
                &sub,
                theme::regular(13.5),
                theme::MUTED,
                rect.right() - x - 36.0,
            );
            kit::icon(
                painter,
                rect.right_center() - egui::vec2(18.0, 0.0),
                ph::WAVEFORM,
                18.0,
                if live {
                    theme::PRIMARY_TEXT
                } else {
                    theme::MUTED
                },
            );
            if resp.clicked() {
                self.selected_lobby = Some(room.lobby.id);
            }
        }
        ui.add_space(12.0);
        kit::section(ui, "Voice session", None, None);
        ui.horizontal(|ui| {
            let (d, _) = ui.allocate_exact_size(egui::vec2(10.0, 18.0), egui::Sense::hover());
            kit::dot(
                ui.painter(),
                d.center(),
                4.5,
                if s.voice.state.connected {
                    theme::SUCCESS
                } else {
                    theme::FAINT
                },
            );
            kit::label(
                ui,
                if s.voice.state.connected {
                    "Connected"
                } else {
                    "Not connected"
                },
                theme::regular(14.0),
                theme::SECONDARY,
            );
        });
        if !s.voice.voice_supported {
            kit::para(
                ui,
                "Voice isn't available on this backend.",
                theme::regular(13.0),
                theme::MUTED,
            );
        }
    }

    /// The room stage: header, participant cards, tabs, control bar (A05).
    pub fn voice_room(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        let room = s
            .rooms
            .rooms
            .iter()
            .find(|r| Some(r.lobby.id) == self.selected_lobby.or(s.voice.state.lobby_id))
            .or_else(|| s.rooms.rooms.first())
            .cloned();
        if self.selected_lobby.is_none() {
            self.selected_lobby = room.as_ref().map(|r| r.lobby.id);
        }
        let title = room
            .as_ref()
            .map(|r| self.display(&r.title))
            .unwrap_or_else(|| "Voice room".into());
        let full = ui.max_rect().shrink2(egui::vec2(22.0, 14.0));
        let bar_h = 82.0;
        let body = Rect::from_min_max(
            full.min,
            egui::pos2(full.right(), full.bottom() - bar_h - 8.0),
        );
        let mut top = ui.new_child(egui::UiBuilder::new().id_salt("voice_body").max_rect(body));
        top.set_clip_rect(body.intersect(ui.clip_rect()));
        // Header.
        let (hdr, _) =
            top.allocate_exact_size(egui::vec2(body.width(), 58.0), egui::Sense::hover());
        let painter = top.painter();
        kit::icon(
            painter,
            hdr.left_center() + egui::vec2(18.0, 0.0),
            ph::WAVEFORM,
            34.0,
            theme::PRIMARY_TEXT,
        );
        painter.text(
            hdr.left_center() + egui::vec2(48.0, -11.0),
            Align2::LEFT_CENTER,
            &title,
            theme::semibold(24.0),
            theme::TEXT,
        );
        let connected = s.voice.state.connected;
        let sub = if connected {
            format!("{} connected", s.voice.state.participants.len())
        } else {
            "Not connected".to_owned()
        };
        let sr = kit::text_at(
            painter,
            hdr.left_center() + egui::vec2(48.0, 14.0),
            Align2::LEFT_CENTER,
            &sub,
            theme::regular(15.0),
            theme::SECONDARY,
            300.0,
        );
        if connected {
            let pill = Rect::from_min_size(
                egui::pos2(sr.right() + 16.0, hdr.center().y + 2.0),
                egui::vec2(104.0, 26.0),
            );
            kit::paint_status_pill(
                painter,
                pill,
                Some(ph::CELL_SIGNAL_FULL),
                "Connected",
                theme::SUCCESS,
            );
        }
        let conv = room.as_ref().and_then(|r| r.conversation_id);
        let bw = kit::button_width(top.painter(), Some(ph::CHAT_CIRCLE), "Open chat", 36.0);
        let br = Rect::from_min_size(
            egui::pos2(hdr.right() - bw, hdr.center().y - 18.0),
            egui::vec2(bw, 36.0),
        );
        if kit::button_at(
            &mut top,
            br,
            egui::Id::new("voice_open_chat"),
            kit::Kind::Secondary,
            Some(ph::CHAT_CIRCLE),
            "Open chat",
            conv.is_some(),
        )
        .clicked()
        {
            if let Some(id) = conv {
                self.open_conversation(id);
            }
        }
        top.add_space(8.0);
        // Participant cards.
        let participants = s.voice.state.participants.clone();
        if participants.is_empty() {
            let (r, _) =
                top.allocate_exact_size(egui::vec2(body.width(), 200.0), egui::Sense::hover());
            kit::paint_card(top.painter(), r, false);
            let c = r.center() - egui::vec2(0.0, 20.0);
            kit::paint_bubble(top.painter(), c, ph::WAVEFORM, kit::BLUE, 64.0);
            top.painter().text(
                c + egui::vec2(0.0, 52.0),
                Align2::CENTER_CENTER,
                if connected {
                    "Nobody else is here yet"
                } else {
                    "Participants appear when you join"
                },
                theme::medium(15.0),
                theme::SECONDARY,
            );
        } else {
            let n = participants.len().min(8);
            kit::card_grid(&mut top, n, 250.0, 12.0, 170.0, 4, |ui, i, rect| {
                let p = &participants[i];
                let name = s
                    .voice
                    .participant_names
                    .iter()
                    .find(|(id, _)| *id == p.user_id)
                    .map(|(_, n)| self.display(n))
                    .unwrap_or_else(|| "Unknown user".into());
                let painter = ui.painter();
                if p.speaking {
                    kit::glow(painter, rect, 14.0, theme::PRIMARY);
                    painter.rect(
                        rect,
                        14.0,
                        theme::lerp(theme::CARD, theme::PRIMARY, 0.14),
                        egui::Stroke::new(1.5_f32, theme::lerp(theme::CARD, theme::PRIMARY, 0.7)),
                        egui::StrokeKind::Inside,
                    );
                } else {
                    kit::paint_card(painter, rect, false);
                }
                let c = egui::pos2(rect.center().x, rect.top() + 82.0);
                if p.speaking {
                    for (k, a) in [(1.0_f32, 0.35_f32), (2.0, 0.18)] {
                        painter.circle_stroke(
                            c,
                            50.0 + k * 8.0,
                            egui::Stroke::new(2.0_f32, theme::PRIMARY_TEXT.gamma_multiply(a)),
                        );
                    }
                }
                theme::paint_avatar(
                    painter,
                    c,
                    92.0,
                    &name,
                    if p.self_muted {
                        theme::Presence::None
                    } else {
                        theme::Presence::Online
                    },
                    theme::CARD,
                );
                painter.text(
                    egui::pos2(c.x, rect.top() + 150.0),
                    Align2::CENTER_CENTER,
                    &name,
                    theme::semibold(17.0),
                    theme::TEXT,
                );
                let (status, sc) = if p.speaking {
                    ("Speaking now", theme::PRIMARY_TEXT)
                } else if p.self_muted {
                    ("Muted", theme::PRIORITY_TEXT)
                } else {
                    ("In voice", theme::SECONDARY)
                };
                painter.text(
                    egui::pos2(c.x, rect.top() + 174.0),
                    Align2::CENTER_CENTER,
                    status,
                    theme::regular(14.0),
                    sc,
                );
                let mc = egui::pos2(c.x, rect.bottom() - 34.0);
                painter.circle_filled(
                    mc,
                    22.0,
                    if p.self_muted {
                        theme::lerp(theme::CARD, theme::PRIORITY, 0.25)
                    } else {
                        theme::lerp(
                            theme::CARD,
                            if p.speaking {
                                theme::OMNI
                            } else {
                                theme::RAISED
                            },
                            0.35,
                        )
                    },
                );
                kit::icon(
                    painter,
                    mc,
                    if p.self_muted {
                        ph::MICROPHONE_SLASH
                    } else {
                        ph::MICROPHONE
                    },
                    20.0,
                    if p.self_muted {
                        theme::PRIORITY_TEXT
                    } else if p.speaking {
                        theme::OMNI
                    } else {
                        theme::SECONDARY
                    },
                );
            });
        }
        top.add_space(12.0);
        // Tabs.
        let (tabs_r, _) =
            top.allocate_exact_size(egui::vec2(body.width(), 34.0), egui::Sense::hover());
        top.painter().text(
            tabs_r.left_center(),
            Align2::LEFT_CENTER,
            "Room",
            theme::semibold(18.0),
            theme::TEXT,
        );
        let labels = ["All", "Messages", "Files", "Members", "Omni"];
        let cw: f32 = labels
            .iter()
            .map(|l| kit::text_width(top.painter(), l, theme::medium(13.0)) + 32.0)
            .sum();
        let chips = Rect::from_min_max(egui::pos2(tabs_r.right() - cw, tabs_r.top()), tabs_r.max);
        let mut cui = top.new_child(
            egui::UiBuilder::new()
                .max_rect(chips)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        kit::chip_row(&mut cui, &labels, &mut self.voice_tab);
        top.add_space(6.0);
        match self.voice_tab {
            0 | 1 => {
                if let Some(id) = conv {
                    kit::card(&mut top, |ui| {
                        ui.horizontal(|ui| {
                            kit::bubble(ui, ph::CHAT_CIRCLE_TEXT, kit::BLUE, 38.0);
                            kit::para(
                                ui,
                                "This room has a conversation in Litecord.",
                                theme::regular(14.5),
                                theme::SECONDARY,
                            );
                        });
                        if kit::button_ex(
                            ui,
                            kit::Kind::Primary,
                            Some(ph::CHAT_CIRCLE),
                            "Open room messages",
                            32.0,
                            true,
                        )
                        .clicked()
                        {
                            self.open_conversation(id);
                        }
                    });
                } else {
                    kit::empty(
                        &mut top,
                        Some(ph::CHAT_CIRCLE),
                        "No room messages",
                        "No cached conversation is associated with this room.",
                    );
                }
            }
            2 => kit::empty(
                &mut top,
                Some(ph::FILE),
                "No shared files",
                "Shared files need the room's conversation history from the backend.",
            ),
            3 => {
                if s.voice.participant_names.is_empty() {
                    kit::empty(
                        &mut top,
                        Some(ph::USERS),
                        "No members",
                        "Members appear when a voice session is connected.",
                    );
                }
                for (_, name) in &s.voice.participant_names {
                    let (r, _) = top
                        .allocate_exact_size(egui::vec2(body.width(), 44.0), egui::Sense::hover());
                    theme::paint_avatar(
                        top.painter(),
                        egui::pos2(r.left() + 20.0, r.center().y),
                        34.0,
                        &self.display(name),
                        theme::Presence::None,
                        theme::WORKSPACE,
                    );
                    top.painter().text(
                        egui::pos2(r.left() + 46.0, r.center().y),
                        Align2::LEFT_CENTER,
                        self.display(name),
                        theme::medium(15.0),
                        theme::TEXT,
                    );
                }
            }
            _ => {
                let (fill, stroke) = kit::accent_colors(theme::OMNI);
                egui::Frame::new().fill(fill).stroke(egui::Stroke::new(1.0_f32, stroke)).corner_radius(12).inner_margin(egui::Margin::same(14)).show(&mut top, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        kit::bubble(ui, ph::SPARKLE, kit::TEAL, 38.0);
                        kit::para(ui, "Omni can't hear voice rooms. Room summaries need the room's text conversation.", theme::regular(14.5), theme::SECONDARY);
                    });
                });
            }
        }
        // Control bar.
        let bar = Rect::from_min_max(egui::pos2(full.left(), full.bottom() - bar_h), full.max);
        let mut bottom = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("voice_bottom_bar")
                .max_rect(bar),
        );
        bottom.painter().rect(
            bar,
            16.0,
            theme::CARD,
            egui::Stroke::new(1.0_f32, theme::BORDER),
            egui::StrokeKind::Inside,
        );
        let enabled = !self.busy && s.voice.voice_supported;
        let st = &s.voice.state;
        let controls: [(&str, &str, bool, VoiceControl); 4] = [
            (
                if st.muted {
                    ph::MICROPHONE_SLASH
                } else {
                    ph::MICROPHONE
                },
                if st.muted { "Unmute" } else { "Mute" },
                st.muted,
                VoiceControl::SetMuted(!st.muted),
            ),
            (
                ph::HEADPHONES,
                if st.deafened { "Undeafen" } else { "Deafen" },
                st.deafened,
                VoiceControl::SetDeafened(!st.deafened),
            ),
            (
                ph::WAVEFORM,
                "Noise suppression",
                st.noise_suppression,
                VoiceControl::SetNoiseSuppression(!st.noise_suppression),
            ),
            (
                ph::BROADCAST,
                "Push to talk",
                st.push_to_talk,
                VoiceControl::SetPushToTalk(!st.push_to_talk),
            ),
        ];
        let slot = ((bar.width() - 180.0) / controls.len() as f32).clamp(70.0, 150.0);
        let mut x = bar.left() + 10.0;
        let mut send = None;
        for (i, (g, label, on, cmd)) in controls.into_iter().enumerate() {
            let r = Rect::from_min_size(
                egui::pos2(x, bar.top() + 6.0),
                egui::vec2(slot, bar_h - 12.0),
            );
            let resp = bottom.interact(
                r,
                bottom.id().with(("voice_ctl", i)),
                if enabled {
                    egui::Sense::click()
                } else {
                    egui::Sense::hover()
                },
            );
            let painter = bottom.painter();
            let c = egui::pos2(r.center().x, r.top() + 24.0);
            let danger = i < 2 && on;
            painter.rect_filled(
                Rect::from_center_size(c, egui::vec2(64.0, 38.0)),
                19.0,
                if danger {
                    theme::lerp(theme::CARD, theme::PRIORITY, 0.25)
                } else if resp.hovered() && enabled {
                    theme::HOVER
                } else {
                    theme::RAISED
                },
            );
            kit::icon(
                painter,
                c,
                g,
                20.0,
                if !enabled {
                    theme::FAINT
                } else if danger {
                    theme::PRIORITY_TEXT
                } else {
                    theme::TEXT
                },
            );
            if i >= 2 && on {
                painter.circle_filled(c + egui::vec2(24.0, -12.0), 4.5, theme::SUCCESS);
            }
            kit::text_at(
                painter,
                egui::pos2(r.center().x, r.bottom() - 8.0),
                Align2::CENTER_BOTTOM,
                label,
                theme::regular(13.0),
                if enabled {
                    theme::SECONDARY
                } else {
                    theme::FAINT
                },
                slot,
            );
            let l = label.to_owned();
            resp.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, on, &l)
            });
            if resp.clicked() {
                send = Some(cmd);
            }
            x += slot;
        }
        let action = Rect::from_min_max(
            egui::pos2(bar.right() - 160.0, bar.top() + 14.0),
            egui::pos2(bar.right() - 14.0, bar.bottom() - 14.0),
        );
        if connected {
            if kit::button_at(
                &mut bottom,
                action,
                egui::Id::new("voice_leave"),
                kit::Kind::Danger,
                Some(ph::PHONE_DISCONNECT),
                "Disconnect",
                !self.busy,
            )
            .clicked()
            {
                send = Some(VoiceControl::Leave);
            }
        } else if kit::button_at(
            &mut bottom,
            action,
            egui::Id::new("voice_join"),
            kit::Kind::Primary,
            Some(ph::SIGN_IN),
            "Join room",
            enabled && self.selected_lobby.is_some(),
        )
        .clicked()
        {
            if let Some(id) = self.selected_lobby {
                send = Some(VoiceControl::JoinLobby(id));
            }
        }
        if let Some(cmd) = send {
            self.send(Command::Voice(cmd));
        }
    }

    /// A05 inspector: room identity, voice settings, people in the room.
    pub(crate) fn voice_inspector(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        let room = s
            .rooms
            .rooms
            .iter()
            .find(|r| Some(r.lobby.id) == self.selected_lobby)
            .cloned();
        let title = room
            .as_ref()
            .map(|r| self.display(&r.title))
            .unwrap_or_else(|| "Voice room".into());
        egui::ScrollArea::vertical()
            .id_salt("voice_inspector")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (banner, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 124.0),
                    egui::Sense::hover(),
                );
                let b =
                    Rect::from_min_max(banner.min, egui::pos2(banner.right(), banner.top() + 90.0));
                theme::gradient_rect(
                    ui.painter(),
                    b,
                    14.0,
                    Color32::from_rgb(64, 54, 150),
                    Color32::from_rgb(20, 24, 52),
                );
                let tile = Rect::from_min_size(
                    egui::pos2(b.left() + 12.0, b.bottom() - 36.0),
                    egui::vec2(64.0, 64.0),
                );
                ui.painter()
                    .rect_filled(tile.expand(3.0), 16.0, theme::INSPECTOR);
                kit::paint_tile(ui.painter(), tile, ph::WAVEFORM, kit::BLUE, 14.0);
                ui.add_space(4.0);
                kit::label(ui, &title, theme::semibold(22.0), theme::TEXT);
                kit::label(
                    ui,
                    if s.voice.state.connected {
                        format!("{} connected", s.voice.state.participants.len())
                    } else {
                        "Not connected".to_owned()
                    },
                    theme::regular(14.5),
                    theme::SECONDARY,
                );
                ui.add_space(10.0);
                kit::divider(ui);
                ui.add_space(8.0);
                self.voice_settings(ui);
            });
    }

    fn voice_settings(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        kit::section(ui, "Voice settings", None, None);
        let enabled = !self.busy && s.voice.voice_supported;
        for (glyph, label, value) in [
            (
                ph::MICROPHONE,
                "Input device",
                s.voice
                    .state
                    .input_device
                    .clone()
                    .unwrap_or_else(|| "System default".into()),
            ),
            (
                ph::SPEAKER_HIGH,
                "Output device",
                s.voice
                    .state
                    .output_device
                    .clone()
                    .unwrap_or_else(|| "System default".into()),
            ),
        ] {
            ui.horizontal(|ui| {
                let (g, _) = ui.allocate_exact_size(egui::vec2(24.0, 52.0), egui::Sense::hover());
                kit::icon(
                    ui.painter(),
                    g.center() + egui::vec2(0.0, 8.0),
                    glyph,
                    19.0,
                    theme::SECONDARY,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    kit::label(ui, label, theme::regular(13.5), theme::SECONDARY);
                    let (r, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 34.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect(
                        r,
                        8.0,
                        theme::FIELD,
                        egui::Stroke::new(1.0_f32, theme::BORDER),
                        egui::StrokeKind::Inside,
                    );
                    kit::text_at(
                        ui.painter(),
                        r.left_center() + egui::vec2(12.0, 0.0),
                        Align2::LEFT_CENTER,
                        &value,
                        theme::regular(14.0),
                        theme::TEXT,
                        r.width() - 40.0,
                    );
                    kit::icon_o(
                        ui.painter(),
                        r.right_center() - egui::vec2(16.0, 0.0),
                        ph::CARET_DOWN,
                        14.0,
                        theme::MUTED,
                    );
                });
            });
        }
        if !s.voice.devices_supported {
            kit::para(
                ui,
                "Device discovery is not exposed by this backend.",
                theme::regular(12.5),
                theme::MUTED,
            );
        }
        // Output volume (draft while dragging, committed on release).
        let draft_id = ui.make_persistent_id("voice_output_volume_draft");
        let draft = if enabled {
            ui.ctx().data(|data| data.get_temp::<f32>(draft_id))
        } else {
            ui.ctx().data_mut(|data| data.remove::<f32>(draft_id));
            None
        };
        let mut volume = draft.unwrap_or(s.voice.state.output_volume);
        ui.horizontal(|ui| {
            kit::label(ui, "Output volume", theme::regular(14.0), theme::SECONDARY);
            ui.spacing_mut().slider_width = (ui.available_width() - 60.0).max(60.0);
            let response = ui.add_enabled(
                enabled,
                egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false),
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
        });
        for (glyph, label, on, cmd) in [
            (
                ph::WAVEFORM,
                "Noise suppression",
                s.voice.state.noise_suppression,
                VoiceControl::SetNoiseSuppression(!s.voice.state.noise_suppression),
            ),
            (
                ph::KEYBOARD,
                "Push to talk",
                s.voice.state.push_to_talk,
                VoiceControl::SetPushToTalk(!s.voice.state.push_to_talk),
            ),
        ] {
            let (r, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::hover());
            kit::icon(
                ui.painter(),
                r.left_center() + egui::vec2(10.0, 0.0),
                glyph,
                18.0,
                theme::SECONDARY,
            );
            ui.painter().text(
                r.left_center() + egui::vec2(32.0, 0.0),
                Align2::LEFT_CENTER,
                label,
                theme::regular(14.5),
                theme::TEXT,
            );
            let t = Rect::from_min_size(
                egui::pos2(r.right() - 48.0, r.center().y - 13.0),
                egui::vec2(46.0, 26.0),
            );
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(t));
            let mut value = on;
            if kit::toggle(&mut child, &mut value, enabled).changed() {
                self.send(Command::Voice(cmd));
            }
        }
        ui.add_space(8.0);
        kit::divider(ui);
        ui.add_space(8.0);
        kit::section(
            ui,
            &format!("In this room ({})", s.voice.participant_names.len()),
            None,
            None,
        );
        if s.voice.participant_names.is_empty() {
            kit::label(ui, "Nobody yet.", theme::regular(13.5), theme::MUTED);
        }
        for (id, name) in &s.voice.participant_names {
            let speaking = s
                .voice
                .state
                .participants
                .iter()
                .any(|p| p.user_id == *id && p.speaking);
            let (r, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 44.0), egui::Sense::hover());
            let painter = ui.painter();
            theme::paint_avatar(
                painter,
                egui::pos2(r.left() + 18.0, r.center().y),
                34.0,
                &self.display(name),
                theme::Presence::Online,
                theme::INSPECTOR,
            );
            kit::text_at(
                painter,
                egui::pos2(r.left() + 44.0, r.center().y - 8.0),
                Align2::LEFT_CENTER,
                &self.display(name),
                theme::medium(14.5),
                theme::TEXT,
                r.width() - 80.0,
            );
            painter.text(
                egui::pos2(r.left() + 44.0, r.center().y + 10.0),
                Align2::LEFT_CENTER,
                if speaking { "Speaking now" } else { "In voice" },
                theme::regular(12.5),
                if speaking {
                    theme::PRIMARY_TEXT
                } else {
                    theme::MUTED
                },
            );
            kit::icon(
                painter,
                r.right_center() - egui::vec2(12.0, 0.0),
                ph::WAVEFORM,
                16.0,
                if speaking {
                    theme::PRIMARY_TEXT
                } else {
                    theme::MUTED
                },
            );
        }
    }
}
