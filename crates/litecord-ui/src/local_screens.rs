//! Local review and configuration screens backed by app snapshots.
//!
//! These panels never read the database or feature registry directly. They
//! render the latest bridge snapshot and send explicit commands for changes.

use crate::{bridge::Command, kit, ph, theme, workspace::Workspace};
use eframe::egui::{self, Ui};
use litecord_app::view::{SettingRow, SettingsViewModel};
use litecord_features::feature::{FeatureInfo, SettingKind};
use litecord_types::capability::BackendMode;
use litecord_types::provenance::Origin;
use litecord_types::social::SessionState;
use serde_json::Value;

impl Workspace {
    /// Render the Tasks destination (see `tasks_ui`).
    pub fn tasks_screen(&mut self, ui: &mut Ui) {
        self.render_tasks_screen(ui);
    }

    /// Settings (mock A12): account, Omni, feature sections, data and
    /// diagnostics as cards; the sidebar picks one section or all.
    pub fn settings_screen(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.spinner();
            ui.label("Loading settings…");
            return;
        };
        let settings = snapshot.settings.clone();
        let private = self.private();
        let section = self.settings_section.clone();
        let show = |name: &str| section.as_deref().is_none_or(|s| s == name);
        egui::ScrollArea::vertical()
            .id_salt("settings_screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                kit::page_title(
                    ui,
                    "Settings",
                    Some("Customize your experience and manage your account."),
                );
                if snapshot.diagnostics.backend_mode == BackendMode::UserSession && show("Account")
                {
                    discord_session_connection(
                        self,
                        ui,
                        &snapshot.account,
                        &snapshot.diagnostics.session,
                    );
                }
                if show("Account") {
                    self.account_card(ui, &snapshot);
                }
                if show("Omni") {
                    settings_card(
                        ui,
                        "Omni",
                        Some("Your assistant, powered by your own Codex or OpenCode account."),
                        |ui| {
                            self.omni_settings(ui, &snapshot.omni);
                        },
                    );
                }
                for (name, rows) in &settings.sections {
                    if !show(name) {
                        continue;
                    }
                    settings_card(ui, name, section_subtitle(name), |ui| {
                        if name.eq_ignore_ascii_case("Plugins") {
                            for feature in &settings.features {
                                feature_row(self, ui, feature);
                            }
                        }
                        let mut displayed = 0;
                        for row in rows {
                            // The feature registry generates these schema
                            // entries for the same metadata shown above.
                            if is_feature_enabled_key(&row.descriptor.key) {
                                continue;
                            }
                            setting_row(self, ui, row, private);
                            displayed += 1;
                        }
                        if displayed == 0
                            && (!name.eq_ignore_ascii_case("Plugins")
                                || settings.features.is_empty())
                        {
                            quiet_empty(ui, "No settings in this section.");
                        }
                    });
                }
                if show("Data") {
                    settings_card(
                        ui,
                        "Data & storage",
                        Some("What Litecord keeps on this computer."),
                        |ui| {
                            self.data_rows(ui, &snapshot);
                        },
                    );
                }
                if show("Diagnostics") {
                    settings_card(
                        ui,
                        "Diagnostics",
                        Some("What the connected backend reports."),
                        |ui| {
                            backend_diagnostics(ui, &settings, &snapshot.diagnostics);
                        },
                    );
                }
                ui.add_space(12.0);
            });
    }

    /// A12 "My account": avatar, names, source, and identity rows.
    fn account_card(&mut self, ui: &mut Ui, s: &crate::bridge::Snapshot) {
        let name = self.display(&s.account.display_name);
        settings_card(ui, "My account", None, |ui| {
            let (r, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), 104.0),
                egui::Sense::hover(),
            );
            theme::paint_avatar(
                ui.painter(),
                egui::pos2(r.left() + 50.0, r.center().y),
                96.0,
                &name,
                if s.diagnostics.session.is_online() {
                    theme::Presence::Online
                } else {
                    theme::Presence::Offline
                },
                theme::CARD,
            );
            let x = r.left() + 118.0;
            let painter = ui.painter();
            kit::text_at(
                painter,
                egui::pos2(x, r.center().y - 22.0),
                egui::Align2::LEFT_CENTER,
                &name,
                theme::semibold(21.0),
                theme::TEXT,
                r.width() - 280.0,
            );
            let handle = s
                .account
                .username
                .as_deref()
                .map(|u| format!("@{}", self.display(u)))
                .unwrap_or_default();
            kit::text_at(
                painter,
                egui::pos2(x, r.center().y + 2.0),
                egui::Align2::LEFT_CENTER,
                &handle,
                theme::regular(16.0),
                theme::SECONDARY,
                r.width() - 280.0,
            );
            kit::text_at(
                painter,
                egui::pos2(x, r.center().y + 26.0),
                egui::Align2::LEFT_CENTER,
                account_source_label(s.account.origin),
                theme::regular(14.0),
                theme::MUTED,
                r.width() - 280.0,
            );
            let bw =
                kit::button_width(painter, Some(ph::ARROW_SQUARE_OUT), "Edit in Discord", 34.0);
            let br = egui::Rect::from_min_size(
                egui::pos2(r.right() - bw, r.top() + 8.0),
                egui::vec2(bw, 34.0),
            );
            if kit::button_at(
                ui,
                br,
                ui.id().with("edit_profile"),
                kit::Kind::Secondary,
                Some(ph::ARROW_SQUARE_OUT),
                "Edit in Discord",
                true,
            )
            .clicked()
            {
                ui.ctx()
                    .open_url(egui::OpenUrl::new_tab("https://discord.com/channels/@me"));
            }
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(theme::lerp(theme::CARD, theme::WORKSPACE, 0.5))
                .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                .corner_radius(10)
                .inner_margin(egui::Margin::symmetric(6, 2))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let rows = [
                        (ph::USER, "Display name", name.clone()),
                        (ph::AT, "Username", handle.clone()),
                        (
                            ph::IDENTIFICATION_CARD,
                            "User ID",
                            s.account
                                .user_id
                                .map(|u| u.to_string())
                                .unwrap_or_else(|| "Not signed in".into()),
                        ),
                        (
                            ph::PLUGS_CONNECTED,
                            "Connected via",
                            backend_label(s.diagnostics.backend_mode).to_owned(),
                        ),
                    ];
                    for (i, (g, k, v)) in rows.iter().enumerate() {
                        if i > 0 {
                            kit::divider(ui);
                        }
                        let (row, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 44.0),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter();
                        kit::icon(
                            painter,
                            row.left_center() + egui::vec2(16.0, 0.0),
                            g,
                            18.0,
                            theme::SECONDARY,
                        );
                        painter.text(
                            row.left_center() + egui::vec2(42.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            *k,
                            theme::medium(14.5),
                            theme::TEXT,
                        );
                        kit::text_at(
                            painter,
                            egui::pos2(row.left() + 180.0, row.center().y),
                            egui::Align2::LEFT_CENTER,
                            &self.display(v),
                            theme::regular(14.5),
                            theme::SECONDARY,
                            row.width() - 200.0,
                        );
                    }
                });
        });
    }

    /// Database size against its quota, history sync and runtime figures.
    fn data_rows(&mut self, ui: &mut Ui, s: &crate::bridge::Snapshot) {
        let h = &s.history;
        let mib = |b: u64| b as f64 / 1_048_576.0;
        if h.quota_bytes > 0 {
            kit::progress(
                ui,
                (h.db_bytes as f32 / h.quota_bytes as f32).clamp(0.0, 1.0),
                theme::PRIMARY,
                8.0,
            );
            kit::label(
                ui,
                format!(
                    "{:.1} MiB of {:.0} MiB used",
                    mib(h.db_bytes),
                    mib(h.quota_bytes)
                ),
                theme::regular(14.0),
                theme::SECONDARY,
            );
        } else {
            kit::label(
                ui,
                format!("Database {:.1} MiB · history sync is off", mib(h.db_bytes)),
                theme::regular(14.0),
                theme::SECONDARY,
            );
        }
        let m = &s.diagnostics.metrics;
        let kib = |b: u64| format!("{:.0} KiB", b as f64 / 1024.0);
        for (k, v) in [
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
        ] {
            let (row, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::hover());
            ui.painter().text(
                row.left_center(),
                egui::Align2::LEFT_CENTER,
                k,
                theme::regular(14.0),
                theme::SECONDARY,
            );
            ui.painter().text(
                row.right_center(),
                egui::Align2::RIGHT_CENTER,
                v,
                theme::medium(14.0),
                theme::TEXT,
            );
        }
        ui.add_space(6.0);
        kit::label(ui, "Full history sync", theme::medium(15.0), theme::TEXT);
        if h.rows.is_empty() {
            kit::para(
                ui,
                "No conversations selected. Use Sync full history in a chat's details.",
                theme::regular(13.5),
                theme::MUTED,
            );
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
            let (row, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::hover());
            let painter = ui.painter();
            kit::text_at(
                painter,
                row.left_center(),
                egui::Align2::LEFT_CENTER,
                &self.display(&r.title),
                theme::regular(14.0),
                theme::TEXT,
                row.width() * 0.5,
            );
            painter.text(
                row.right_center(),
                egui::Align2::RIGHT_CENTER,
                format!("{} msgs · {state}", r.messages),
                theme::regular(13.0),
                theme::MUTED,
            );
        }
    }

    // ---- Sidebar and inspector ---------------------------------------------------

    pub(crate) fn settings_sidebar(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        ui.add_space(4.0);
        kit::search(ui, &mut self.filter, "Search settings...");
        ui.add_space(12.0);
        let needle = self.filter.trim().to_lowercase();
        let visible = |label: &str| needle.is_empty() || label.to_lowercase().contains(&needle);
        egui::ScrollArea::vertical()
            .id_salt("settings_sidebar")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                let item = |ws: &mut Workspace,
                            ui: &mut Ui,
                            glyph: &str,
                            label: &str,
                            key: Option<&str>| {
                    if !visible(label) {
                        return;
                    }
                    let selected = ws.settings_section.as_deref() == key;
                    if kit::side_item(
                        ui,
                        Some((
                            glyph,
                            if selected {
                                theme::PRIMARY_TEXT
                            } else {
                                theme::SECONDARY
                            },
                        )),
                        label,
                        None,
                        selected,
                    )
                    .clicked()
                    {
                        ws.settings_section = key.map(str::to_owned);
                    }
                };
                item(self, ui, ph::GEAR_SIX, "All settings", None);
                item(self, ui, ph::USER_CIRCLE, "My account", Some("Account"));
                kit::group_label(ui, "App settings");
                for (section, _) in &s.settings.sections {
                    item(
                        self,
                        ui,
                        section_glyph(section),
                        section,
                        Some(section.as_str()),
                    );
                }
                kit::group_label(ui, "Omni");
                item(self, ui, ph::SPARKLE, "AI (Omni)", Some("Omni"));
                kit::group_label(ui, "Data");
                item(self, ui, ph::DATABASE, "Data & storage", Some("Data"));
                item(self, ui, ph::CPU, "Diagnostics", Some("Diagnostics"));
            });
    }

    /// A12 right column: plan, preferences, connected accounts, storage.
    pub(crate) fn settings_inspector(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("settings_inspector")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                kit::card(ui, |ui| {
                    let (h, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 30.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().text(
                        h.left_center(),
                        egui::Align2::LEFT_CENTER,
                        "Litecord",
                        theme::semibold(19.0),
                        theme::TEXT,
                    );
                    let pill = egui::Rect::from_min_size(
                        egui::pos2(h.right() - 64.0, h.top()),
                        egui::vec2(64.0, 30.0),
                    );
                    ui.painter().rect_filled(pill, 8.0, theme::PRIMARY);
                    ui.painter().text(
                        pill.center(),
                        egui::Align2::CENTER_CENTER,
                        "Local",
                        theme::medium(14.0),
                        egui::Color32::WHITE,
                    );
                    kit::label(
                        ui,
                        "Everything stays on this computer.",
                        theme::regular(14.0),
                        theme::SECONDARY,
                    );
                    ui.add_space(4.0);
                    for t in [
                        "Chat, friends and servers",
                        "Omni assistant (your own harness)",
                        "Memory and file metadata, stored locally",
                        "Approval before any Discord change",
                    ] {
                        ui.horizontal(|ui| {
                            let (g, _) = ui
                                .allocate_exact_size(egui::vec2(20.0, 22.0), egui::Sense::hover());
                            kit::icon(
                                ui.painter(),
                                g.center(),
                                ph::CHECK_CIRCLE,
                                17.0,
                                theme::SUCCESS,
                            );
                            kit::label(
                                ui,
                                t,
                                theme::regular(14.0),
                                theme::lerp(theme::TEXT, theme::SECONDARY, 0.2),
                            );
                        });
                    }
                });
                kit::card(ui, |ui| {
                    kit::label(ui, "App preferences", theme::semibold(17.0), theme::TEXT);
                    kit::label(
                        ui,
                        "Set up notifications, privacy, and more.",
                        theme::regular(13.5),
                        theme::MUTED,
                    );
                    ui.add_space(2.0);
                    let names: Vec<String> = s
                        .settings
                        .sections
                        .iter()
                        .map(|(n, _)| n.clone())
                        .take(5)
                        .collect();
                    for (i, n) in names.iter().enumerate() {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 34.0),
                            egui::Sense::hover(),
                        );
                        if kit::list_line(
                            ui,
                            rect,
                            ui.id().with(("pref", i)),
                            section_glyph(n),
                            theme::SECONDARY,
                            n,
                            theme::TEXT,
                        )
                        .clicked()
                        {
                            self.settings_section = Some(n.clone());
                        }
                    }
                });
                kit::card(ui, |ui| {
                    kit::label(ui, "Connected accounts", theme::semibold(17.0), theme::TEXT);
                    kit::label(
                        ui,
                        "Where Litecord's data and Omni come from.",
                        theme::regular(13.5),
                        theme::MUTED,
                    );
                    ui.add_space(2.0);
                    let discord = match s.diagnostics.backend_mode {
                        BackendMode::Demo => ("Demo data", theme::WARNING),
                        _ if s.diagnostics.session.is_online() => ("Connected", theme::SUCCESS),
                        _ => ("Not connected", theme::MUTED),
                    };
                    let mut rows: Vec<(&str, &str, String, egui::Color32)> =
                        vec![(ph::DISCORD_LOGO, "Discord", discord.0.to_owned(), discord.1)];
                    if let Some(bot) = &s.diagnostics.bot {
                        rows.push((
                            ph::ROBOT,
                            "Discord bot",
                            session_label(&bot.session).to_owned(),
                            if bot.session.is_online() {
                                theme::SUCCESS
                            } else {
                                theme::MUTED
                            },
                        ));
                    }
                    for h in &s.omni.status.harnesses {
                        let selected = s.omni.status.selected == Some(h.kind);
                        let (text, color) = if !h.installed {
                            ("Not installed".to_owned(), theme::MUTED)
                        } else if selected {
                            match &s.omni.status.login {
                                litecord_app::harness::LoginState::Ready { .. }
                                | litecord_app::harness::LoginState::Stopped => {
                                    ("In use".to_owned(), theme::SUCCESS)
                                }
                                _ => ("Sign in needed".to_owned(), theme::WARNING),
                            }
                        } else {
                            ("Installed".to_owned(), theme::SECONDARY)
                        };
                        rows.push((
                            if h.kind.label().contains("Codex") {
                                ph::OPEN_AI_LOGO
                            } else {
                                ph::TERMINAL_WINDOW
                            },
                            h.kind.label(),
                            text,
                            color,
                        ));
                    }
                    for (i, (g, name, state, color)) in rows.iter().enumerate() {
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 36.0),
                            egui::Sense::click(),
                        );
                        if resp.hovered() {
                            ui.painter().rect_filled(
                                rect.expand2(egui::vec2(6.0, 0.0)),
                                6.0,
                                theme::lerp(theme::CARD, theme::HOVER, 0.7),
                            );
                        }
                        let painter = ui.painter();
                        kit::icon(
                            painter,
                            rect.left_center() + egui::vec2(12.0, 0.0),
                            g,
                            19.0,
                            theme::TEXT,
                        );
                        kit::text_at(
                            painter,
                            rect.left_center() + egui::vec2(34.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            name,
                            theme::medium(14.5),
                            theme::TEXT,
                            rect.width() * 0.45,
                        );
                        let sw = kit::text_width(painter, state, theme::regular(13.0));
                        painter.circle_filled(
                            egui::pos2(rect.right() - sw - 34.0, rect.center().y),
                            4.5,
                            *color,
                        );
                        painter.text(
                            egui::pos2(rect.right() - 24.0, rect.center().y),
                            egui::Align2::RIGHT_CENTER,
                            state,
                            theme::regular(13.0),
                            theme::SECONDARY,
                        );
                        kit::chevron(
                            painter,
                            rect.right_center() - egui::vec2(6.0, 0.0),
                            theme::MUTED,
                        );
                        if resp.clicked() {
                            self.settings_section = Some(if i == 0 {
                                "Account".into()
                            } else {
                                "Omni".into()
                            });
                        }
                    }
                });
                kit::card(ui, |ui| {
                    kit::label(ui, "Data & storage", theme::semibold(17.0), theme::TEXT);
                    kit::label(
                        ui,
                        "Manage your data, memory, and history.",
                        theme::regular(13.5),
                        theme::MUTED,
                    );
                    ui.add_space(4.0);
                    let h = &s.history;
                    let mib = |b: u64| b as f64 / 1_048_576.0;
                    if h.quota_bytes > 0 {
                        kit::progress(
                            ui,
                            (h.db_bytes as f32 / h.quota_bytes as f32).clamp(0.0, 1.0),
                            theme::PRIMARY,
                            8.0,
                        );
                        kit::label(
                            ui,
                            format!(
                                "{:.1} MiB of {:.0} MiB used",
                                mib(h.db_bytes),
                                mib(h.quota_bytes)
                            ),
                            theme::regular(13.5),
                            theme::SECONDARY,
                        );
                    } else {
                        kit::label(
                            ui,
                            format!("{:.1} MiB used", mib(h.db_bytes)),
                            theme::regular(13.5),
                            theme::SECONDARY,
                        );
                    }
                    if kit::link(ui, "Storage details").clicked() {
                        self.settings_section = Some("Data".into());
                    }
                });
            });
    }
}

/// A settings section card with a title and optional subtitle.
fn settings_card(ui: &mut Ui, title: &str, subtitle: Option<&str>, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::CARD)
        .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
        .corner_radius(14)
        .inner_margin(egui::Margin::same(20))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            kit::label(ui, title, theme::semibold(20.0), theme::TEXT);
            if let Some(sub) = subtitle {
                kit::label(ui, sub, theme::regular(14.0), theme::SECONDARY);
            }
            ui.add_space(6.0);
            add(ui);
        });
}

fn section_subtitle(name: &str) -> Option<&'static str> {
    match name.to_lowercase().as_str() {
        "appearance" => Some("Choose how Litecord looks and feels."),
        "notifications" => Some("Decide what gets your attention."),
        "privacy" => Some("Control what is shown on screen and shared with Omni."),
        "plugins" => Some("Built-in features you can turn on or off."),
        _ => None,
    }
}

fn section_glyph(name: &str) -> &'static str {
    match name.to_lowercase().as_str() {
        "appearance" => ph::PALETTE,
        "notifications" => ph::BELL,
        "privacy" => ph::SHIELD_CHECK,
        "plugins" => ph::PUZZLE_PIECE,
        "voice" => ph::MICROPHONE,
        "chat" | "messages" => ph::CHAT_CIRCLE,
        "accessibility" => ph::PERSON,
        "keybinds" | "shortcuts" => ph::KEYBOARD,
        _ => ph::SLIDERS_HORIZONTAL,
    }
}

fn discord_session_connection(
    workspace: &mut Workspace,
    ui: &mut Ui,
    account: &litecord_app::people::AccountViewModel,
    state: &SessionState,
) {
    let (fill, stroke) = kit::accent_colors(theme::WARNING);
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0_f32, stroke))
        .corner_radius(14)
        .inner_margin(egui::Margin::same(20))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            kit::label(ui, "Discord connection", theme::semibold(20.0), theme::TEXT);
            kit::para(
                ui,
                "Experimental account connection; Discord forbids account automation and may terminate accounts.",
                theme::regular(14.0),
                theme::WARNING,
            );
            kit::label(ui, "This account connection is read-only.", theme::regular(13.0), theme::MUTED);
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                theme::chip(ui, &format!("Account · {}", workspace.display(&account.display_name)), theme::SECONDARY);
                theme::chip(
                    ui,
                    &format!("State · {}", session_label(state)),
                    if state.is_online() { theme::SUCCESS } else { theme::MUTED },
                );
                theme::chip(ui, &format!("Source · {}", account_source_label(account.origin)), theme::MUTED);
            });
            ui.add_space(6.0);
            kit::label(ui, "Session credential", theme::medium(14.0), theme::TEXT);
            ui.add_enabled(
                !workspace.busy,
                egui::TextEdit::singleline(&mut workspace.discord_session_draft)
                    .password(true)
                    .desired_width(f32::INFINITY)
                    .hint_text("Paste credential"),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let can_connect = !workspace.busy && !workspace.discord_session_draft.trim().is_empty();
                if kit::button_ex(ui, kit::Kind::Primary, None, "Connect", 32.0, can_connect).clicked() {
                    let credential = std::mem::take(&mut workspace.discord_session_draft);
                    workspace.send(Command::ConnectSession(litecord_core::secrets::Secret::new(credential)));
                }
                let connected = account.user_id.is_some() || !matches!(state, SessionState::LoggedOut);
                if kit::button_ex(ui, kit::Kind::Secondary, None, "Log out", 32.0, !workspace.busy && connected).clicked() {
                    workspace.discord_session_draft.clear();
                    workspace.send(Command::DiscordSignOut);
                }
            });
        });
}

fn account_source_label(origin: Option<Origin>) -> &'static str {
    match origin {
        Some(Origin::DiscordSocialSdk | Origin::DiscordUserSession) => "User session (read only)",
        Some(Origin::DiscordBotGateway) => "Discord bot",
        Some(Origin::UserProvided) => "User provided",
        Some(Origin::LocalApplication) => "Local application",
        Some(Origin::AgentDerived) => "Omni",
        Some(Origin::Imported) => "Imported",
        Some(Origin::Synthetic) => "Synthetic demo",
        None => "No account connected",
    }
}

fn feature_row(workspace: &mut Workspace, ui: &mut Ui, feature: &FeatureInfo) {
    let mut enabled = feature.enabled;
    let controls_enabled = !workspace.busy;
    let changed = toggle_row(
        ui,
        feature.name,
        feature.description,
        &mut enabled,
        controls_enabled,
    );
    if changed {
        workspace.send(Command::Setting(
            format!("features.{}.enabled", feature.id),
            Value::Bool(enabled),
        ));
    }
    ui.add_space(4.0);
}

fn setting_row(workspace: &mut Workspace, ui: &mut Ui, row: &SettingRow, private: bool) {
    let key = &row.descriptor.key;
    match &row.descriptor.kind {
        SettingKind::Bool { default } => {
            let mut value = row.value.as_bool().unwrap_or(*default);
            let controls_enabled = !workspace.busy;
            let changed = toggle_row(
                ui,
                &row.descriptor.label,
                &row.descriptor.description,
                &mut value,
                controls_enabled,
            );
            if changed {
                workspace.send(Command::Setting(key.clone(), Value::Bool(value)));
            }
        }
        SettingKind::Text { .. } | SettingKind::StringList { .. } | SettingKind::Number { .. } => {
            kit::label(ui, &row.descriptor.label, theme::medium(15.0), theme::TEXT);
            kit::para(
                ui,
                &row.descriptor.description,
                theme::regular(13.5),
                theme::MUTED,
            );

            let id = egui::Id::new(("litecord-setting-draft", key.as_str()));
            let mut draft = ui
                .ctx()
                .data(|data| data.get_temp::<SettingDraft>(id))
                .filter(|stored| stored.source == row.value)
                .unwrap_or_else(|| SettingDraft {
                    source: row.value.clone(),
                    text: setting_text(&row.value, &row.descriptor.kind),
                });

            let mut save = false;
            if private
                && matches!(
                    &row.descriptor.kind,
                    SettingKind::Text { .. } | SettingKind::StringList { .. }
                )
            {
                ui.label(
                    egui::RichText::new(workspace.display(&draft.text))
                        .size(12.0)
                        .color(theme::MUTED),
                );
            } else {
                let controls_enabled = !workspace.busy;
                let multiline = matches!(&row.descriptor.kind, SettingKind::StringList { .. });
                let text_edit = if multiline {
                    egui::TextEdit::multiline(&mut draft.text)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                } else {
                    egui::TextEdit::singleline(&mut draft.text).desired_width(f32::INFINITY)
                };
                ui.add_enabled(controls_enabled, text_edit);

                let value = setting_value(&draft.text, &row.descriptor.kind);
                let changed = value.as_ref().is_some_and(|v| v != &row.value);
                let controls_enabled = !workspace.busy && changed;
                ui.horizontal(|ui| {
                    if kit::button_ex(ui, kit::Kind::Primary, None, "Save", 30.0, controls_enabled)
                        .clicked()
                    {
                        save = true;
                    }
                    if value.is_none() {
                        if let SettingKind::Number { min, max, .. } = &row.descriptor.kind {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Enter a finite number from {min} to {max}."
                                ))
                                .size(12.0)
                                .color(theme::PRIORITY),
                            );
                        }
                    }
                });
            }

            ui.ctx()
                .data_mut(|data| data.insert_temp(id, draft.clone()));
            if save {
                if let Some(value) = setting_value(&draft.text, &row.descriptor.kind) {
                    workspace.send(Command::Setting(key.clone(), value));
                }
            }
        }
    }
    ui.add_space(8.0);
}

fn backend_diagnostics(
    ui: &mut Ui,
    settings: &SettingsViewModel,
    diagnostics: &litecord_app::view::DiagnosticsViewModel,
) {
    {
        {
            ui.horizontal_wrapped(|ui| {
                theme::chip(
                    ui,
                    &format!("Mode · {}", backend_label(diagnostics.backend_mode)),
                    theme::MUTED,
                );
                theme::chip(
                    ui,
                    &format!("Session · {}", session_label(&diagnostics.session)),
                    if diagnostics.session.is_online() {
                        theme::SUCCESS
                    } else {
                        theme::MUTED
                    },
                );
                theme::chip(
                    ui,
                    &format!(
                        "Hydration · {} pending · {} active",
                        diagnostics.hydration_pending, diagnostics.hydration_active
                    ),
                    theme::MUTED,
                );
            });

            ui.add_space(6.0);
            ui.label(format!(
                "Local records · {} users · {} messages",
                diagnostics.counts.users, diagnostics.counts.messages
            ));

            let usable_capabilities = diagnostics
                .capabilities
                .entries
                .values()
                .filter(|level| level.is_usable())
                .count();
            ui.label(format!(
                "Backend-reported capabilities · {usable_capabilities} usable of {} declared",
                diagnostics.capabilities.entries.len()
            ));

            let enabled_features = settings
                .features
                .iter()
                .filter(|feature| feature.enabled)
                .count();
            ui.label(format!(
                "Compiled features · {enabled_features} enabled of {} available",
                settings.features.len()
            ));
            ui.label(format!(
                "Hydration metrics · {} queued · {} active · {} failures · {} events dropped",
                diagnostics.metrics.hydration_queue_depth,
                diagnostics.metrics.hydration_active,
                diagnostics.metrics.hydration_failures,
                diagnostics.metrics.events_dropped
            ));

            if !diagnostics.counts.memories_by_status.is_empty() {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    for (status, count) in &diagnostics.counts.memories_by_status {
                        theme::chip(ui, &format!("{} · {count}", status.as_str()), theme::MUTED);
                    }
                });
            }
        }
    }
}

/// Label + description on the left, a toggle on the right. Returns whether
/// the value changed.
fn toggle_row(
    ui: &mut Ui,
    label: &str,
    description: &str,
    value: &mut bool,
    enabled: bool,
) -> bool {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 52.0), egui::Sense::hover());
    let painter = ui.painter();
    kit::text_at(
        painter,
        egui::pos2(rect.left(), rect.center().y - 10.0),
        egui::Align2::LEFT_CENTER,
        label,
        theme::medium(15.0),
        theme::TEXT,
        rect.width() - 70.0,
    );
    kit::text_at(
        painter,
        egui::pos2(rect.left(), rect.center().y + 11.0),
        egui::Align2::LEFT_CENTER,
        description,
        theme::regular(13.5),
        theme::MUTED,
        rect.width() - 70.0,
    );
    let t = egui::Rect::from_min_size(
        egui::pos2(rect.right() - 48.0, rect.center().y - 13.0),
        egui::vec2(46.0, 26.0),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(t));
    kit::toggle(&mut child, value, enabled)
        .on_hover_text(label)
        .changed()
}

#[derive(Clone)]
struct SettingDraft {
    source: Value,
    text: String,
}

fn setting_text(value: &Value, kind: &SettingKind) -> String {
    match kind {
        SettingKind::Bool { default } => value.as_bool().unwrap_or(*default).to_string(),
        SettingKind::Text { default } => value.as_str().unwrap_or(default).to_owned(),
        SettingKind::StringList { default } => value
            .as_array()
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| default.join("\n")),
        SettingKind::Number { default, .. } => value
            .as_f64()
            .filter(|number| number.is_finite())
            .unwrap_or(*default)
            .to_string(),
    }
}

fn setting_value(text: &str, kind: &SettingKind) -> Option<Value> {
    match kind {
        SettingKind::Bool { .. } => None,
        SettingKind::Text { .. } => Some(Value::String(text.to_owned())),
        SettingKind::StringList { .. } => Some(Value::Array(
            text.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| Value::String(line.to_owned()))
                .collect(),
        )),
        SettingKind::Number { min, max, .. } => {
            let number = text.trim().parse::<f64>().ok()?;
            if !number.is_finite() || number < *min || number > *max {
                return None;
            }
            serde_json::Number::from_f64(number).map(Value::Number)
        }
    }
}

fn is_feature_enabled_key(key: &str) -> bool {
    key.strip_prefix("features.")
        .is_some_and(|rest| rest.ends_with(".enabled"))
}

fn backend_label(mode: litecord_types::capability::BackendMode) -> &'static str {
    use litecord_types::capability::BackendMode;
    match mode {
        BackendMode::FullSocialSdk => "Full Social SDK",
        BackendMode::PresenceOnly => "Presence only",
        BackendMode::Demo => "Demo · synthetic",
        BackendMode::BotBridge => "Bot bridge",
        BackendMode::UserSession => "User session (read only)",
    }
}

fn session_label(session: &SessionState) -> &'static str {
    match session {
        SessionState::LoggedOut => "Logged out",
        SessionState::Authorizing => "Authorizing",
        SessionState::Connecting => "Connecting",
        SessionState::Hydrating => "Hydrating",
        SessionState::Ready => "Ready",
        SessionState::Reconnecting => "Reconnecting",
        SessionState::Offline => "Offline",
        SessionState::Error { .. } => "Error",
    }
}

fn quiet_empty(ui: &mut Ui, message: &str) {
    ui.label(egui::RichText::new(message).color(theme::MUTED));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge;
    use discord_adapter::{fixtures, MockBackend};
    use litecord_app::LitecordApp;
    use litecord_core::config::LitecordConfig;
    use litecord_layout::Destination;
    use litecord_types::{provenance::Origin, Timestamp};
    use std::sync::Arc;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn user_session_settings_masks_credential_and_shows_connection_metadata() {
        const CREDENTIAL: &str = "session_credential_sentinel_b28c";

        let backend = Arc::new(MockBackend::new(fixtures::generate(83, Timestamp::now())));
        let app = LitecordApp::builder(LitecordConfig::default())
            .backend(backend)
            .in_memory()
            .start()
            .await
            .unwrap();
        let selection = bridge::Selection {
            generation: 1,
            destination: Destination::Settings,
            conversation: None,
            contact: None,
            before: None,
            palette_query: String::new(),
            task: None,
            omni_session: None,
        };
        let mut snapshot = bridge::snapshot(&app, selection.clone(), None).unwrap();
        snapshot.diagnostics.backend_mode = BackendMode::UserSession;
        snapshot.account.origin = Some(Origin::DiscordUserSession);
        snapshot.account.display_name = "Example account".into();

        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let mut workspace =
            Workspace::new(app.clone(), tokio::runtime::Handle::current(), ctx.clone());
        workspace.selection = selection;
        workspace.discord_session_draft = CREDENTIAL.to_owned();
        workspace.snapshot = Some(Arc::new(snapshot));

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 900.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| workspace.draw(ui));
        let rendered = rendered_text(&output);

        assert!(!rendered.contains(CREDENTIAL));
        assert!(
            rendered.contains("Experimental account connection; Discord forbids account automation and may terminate accounts.")
        );
        assert!(rendered.contains("User session (read only)"));
        assert!(rendered.contains("Example account"));

        drop(workspace);
        app.shutdown().await;
    }

    fn rendered_text(output: &egui::FullOutput) -> String {
        let mut text = String::new();
        for clipped in &output.shapes {
            collect_shape_text(&clipped.shape, &mut text);
        }
        text
    }

    fn collect_shape_text(shape: &egui::Shape, text: &mut String) {
        match shape {
            egui::Shape::Text(text_shape) => {
                text.push_str(&text_shape.galley.job.text);
                text.push('\n');
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect_shape_text(shape, text);
                }
            }
            _ => {}
        }
    }
}
