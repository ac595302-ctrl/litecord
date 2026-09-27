//! Local review and configuration screens backed by app snapshots.
//!
//! These panels never read the database or feature registry directly. They
//! render the latest bridge snapshot and send explicit commands for changes.

use crate::{bridge::Command, theme, workspace::Workspace};
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

    /// Render recent memory with provenance and explicit candidate review.
    /// Render settings described by the compiled feature schema and backend
    /// diagnostics reported by the current snapshot.
    pub fn settings_screen(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.spinner();
            ui.label("Loading settings…");
            return;
        };

        let settings = snapshot.settings.clone();
        let private = self.private();
        egui::ScrollArea::vertical()
            .id_salt("settings_screen")
            .show(ui, |ui| {
                ui.heading("Settings");
                ui.label(
                    egui::RichText::new("Customize your experience and manage your account.")
                        .color(theme::MUTED),
                );
                ui.add_space(12.0);

                if snapshot.diagnostics.backend_mode == BackendMode::UserSession {
                    discord_session_connection(
                        self,
                        ui,
                        &snapshot.account,
                        &snapshot.diagnostics.session,
                        &snapshot.diagnostics.capabilities,
                        &settings,
                    );
                    ui.add_space(12.0);
                }

                if self.settings_section.as_deref().is_none_or(|s| s == "Omni") {
                    self.omni_settings(ui, &snapshot.omni);
                    ui.add_space(12.0);
                }
                if self.settings_section.as_deref() == Some("Omni") {
                    return;
                }
                if settings.sections.is_empty() {
                    quiet_empty(ui, "No settings are registered in this build.");
                }

                for (section, rows) in &settings.sections {
                    if self
                        .settings_section
                        .as_ref()
                        .is_some_and(|selected| selected != section)
                    {
                        continue;
                    }
                    theme::section_label(ui, section);
                    egui::Frame::new()
                        .fill(theme::SIDEBAR)
                        .inner_margin(egui::Margin::same(10))
                        .corner_radius(egui::CornerRadius::same(8))
                        .show(ui, |ui| {
                            if section.eq_ignore_ascii_case("Plugins") {
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
                                && (!section.eq_ignore_ascii_case("Plugins")
                                    || settings.features.is_empty())
                            {
                                quiet_empty(ui, "No settings in this section.");
                            }
                        });
                    ui.add_space(12.0);
                }

                if !settings.outbound.is_empty() {
                    theme::section_label(ui,"Recent Discord operations");
                    for operation in &settings.outbound {
                        let uncertain=operation.state==litecord_types::actions::OutboundState::Uncertain;
                        ui.horizontal_wrapped(|ui| {
                            theme::chip(ui,&format!("{} · {}",operation.kind.replace('_'," "),operation.state.as_str()),if uncertain {theme::WARNING} else {theme::MUTED});
                            if uncertain { ui.label("Delivery unknown. Refresh the conversation; this operation will not be resent automatically."); }
                        });
                    }
                    ui.add_space(12.0);
                }
                backend_diagnostics(ui, &settings, &snapshot.diagnostics);
            });
    }
}

fn discord_session_connection(
    workspace: &mut Workspace,
    ui: &mut Ui,
    account: &litecord_app::people::AccountViewModel,
    state: &SessionState,
    capabilities: &litecord_types::capability::CapabilitySet,
    settings: &SettingsViewModel,
) {
    let can_write = capabilities.is_usable(litecord_types::capability::Capability::DmSend);
    theme::section_label(ui, "Discord connection");
    egui::Frame::new()
        .fill(theme::SIDEBAR)
        .inner_margin(egui::Margin::same(10))
        .corner_radius(egui::CornerRadius::same(8))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(
                    "Experimental account sign-in uses Discord's unsupported password endpoint. Discord forbids account automation and may terminate accounts.",
                )
                .color(theme::WARNING),
            );
            ui.label(
                egui::RichText::new(if can_write { "Read/write · Send, reply, edit and delete your messages." } else { "Message writes are disabled until sign-in with read/write access." })
                    .size(12.0)
                    .color(theme::MUTED),
            );
            ui.add_space(8.0);

            if let Some(access) = settings.account_access {
                let mut enabled = access == litecord_types::capability::SessionAccessMode::ReadWrite;
                if ui.add_enabled(!workspace.busy && settings.can_enable_account_writes, egui::Checkbox::new(&mut enabled, "Allow account writes")).changed() {
                    workspace.send(Command::AccountWrites(enabled));
                }
                if !settings.can_enable_account_writes {
                    ui.label(egui::RichText::new("Read-only access is required by configuration.").size(12.0).color(theme::MUTED));
                }
            }

            ui.horizontal_wrapped(|ui| {
                theme::chip(
                    ui,
                    &format!("Account · {}", workspace.display(&account.display_name)),
                    theme::SECONDARY,
                );
                theme::chip(
                    ui,
                    &format!("State · {}", session_label(state)),
                    if state.is_online() {
                        theme::SUCCESS
                    } else {
                        theme::MUTED
                    },
                );
                theme::chip(
                    ui,
                    &format!("Source · {}", account_source_label(account.origin)),
                    theme::MUTED,
                );
            });

            if capabilities.is_usable(litecord_types::capability::Capability::RichPresence) && state.is_online() {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Set status:");
                    for (label,status) in [("Online",litecord_types::social::PresenceStatus::Online),("Idle",litecord_types::social::PresenceStatus::Idle),("Do not disturb",litecord_types::social::PresenceStatus::DoNotDisturb),("Invisible",litecord_types::social::PresenceStatus::Invisible)] {
                        if ui.add_enabled(!workspace.busy,egui::Button::new(label)).clicked() {
                            workspace.send(Command::Presence(litecord_types::actions::PresenceDraft {status,activity:None}));
                        }
                    }
                });
            }
            ui.add_space(8.0);
            if let SessionState::Error { message } = state {
                ui.label(egui::RichText::new(message).color(theme::WARNING));
                ui.add_space(6.0);
            }
            if !state.is_online() {
                if workspace.discord_totp_required {
                    ui.label(egui::RichText::new("Authenticator code").strong());
                    ui.add_enabled(
                        !workspace.busy,
                        egui::TextEdit::singleline(&mut workspace.discord_totp_draft)
                            .desired_width(240.0)
                            .hint_text("6-digit code"),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !workspace.busy && !workspace.discord_totp_draft.trim().is_empty(),
                                egui::Button::new("Finish sign-in"),
                            )
                            .clicked()
                        {
                            let code = std::mem::take(&mut workspace.discord_totp_draft);
                            workspace.send(Command::DiscordTotp(
                                litecord_core::secrets::Secret::new(code),
                            ));
                        }
                        if ui.button("Start over").clicked() {
                            workspace.discord_totp_required = false;
                            workspace.discord_totp_draft.clear();
                        }
                    });
                } else {
                    ui.label(egui::RichText::new("Email or verified phone").strong());
                    ui.add_enabled(
                        !workspace.busy,
                        egui::TextEdit::singleline(&mut workspace.discord_email_draft)
                            .desired_width(f32::INFINITY)
                            .hint_text("Your Discord email or phone"),
                    );
                    ui.label(egui::RichText::new("Password").strong());
                    ui.add_enabled(
                        !workspace.busy,
                        egui::TextEdit::singleline(&mut workspace.discord_password_draft)
                            .password(true)
                            .desired_width(f32::INFINITY)
                            .hint_text("Discord password"),
                    );
                    let can_login = !workspace.busy
                        && !workspace.discord_email_draft.trim().is_empty()
                        && !workspace.discord_password_draft.is_empty();
                    if ui
                        .add_enabled(can_login, egui::Button::new("Sign in to Discord"))
                        .clicked()
                    {
                        let login = workspace.discord_email_draft.trim().to_owned();
                        let password = std::mem::take(&mut workspace.discord_password_draft);
                        workspace.send(Command::DiscordPasswordLogin(
                            litecord_core::secrets::Secret::new(login),
                            litecord_core::secrets::Secret::new(password),
                        ));
                    }
                }
                ui.add_space(8.0);
                ui.collapsing("Advanced: connect with a session credential", |ui| {
                    ui.label(egui::RichText::new("This is not your email or password. Only the resulting credential is saved in OS secure storage.").size(12.0).color(theme::MUTED));
                    ui.add_enabled(
                        !workspace.busy,
                        egui::TextEdit::singleline(&mut workspace.discord_session_draft)
                            .password(true)
                            .desired_width(f32::INFINITY)
                            .hint_text("Session credential"),
                    );
                    if ui
                        .add_enabled(
                            !workspace.busy && !workspace.discord_session_draft.trim().is_empty(),
                            egui::Button::new("Connect session"),
                        )
                        .clicked()
                    {
                        let credential = std::mem::take(&mut workspace.discord_session_draft);
                        workspace.send(Command::ConnectSession(
                            litecord_core::secrets::Secret::new(credential),
                        ));
                    }
                });
            }
            let connected = account.user_id.is_some() || !matches!(state, SessionState::LoggedOut);
            if ui
                .add_enabled(!workspace.busy && connected, egui::Button::new("Log out"))
                .clicked()
            {
                workspace.discord_session_draft.clear();
                workspace.discord_email_draft.clear();
                workspace.discord_password_draft.clear();
                workspace.discord_totp_draft.clear();
                workspace.discord_totp_required = false;
                workspace.send(Command::DiscordSignOut);
            }
        });
}

fn account_source_label(origin: Option<Origin>) -> &'static str {
    match origin {
        Some(Origin::DiscordSocialSdk | Origin::DiscordUserSession) => "Discord account",
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
    let mut changed = false;
    let controls_enabled = !workspace.busy;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(feature.name)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.label(
                egui::RichText::new(feature.description)
                    .size(12.0)
                    .color(theme::MUTED),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed = ui
                .add_enabled(
                    controls_enabled,
                    egui::Checkbox::new(&mut enabled, "Enabled"),
                )
                .changed();
        });
    });
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
            let mut changed = false;
            let controls_enabled = !workspace.busy;
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(&row.descriptor.label).strong());
                    ui.label(
                        egui::RichText::new(&row.descriptor.description)
                            .size(12.0)
                            .color(theme::MUTED),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    changed = ui
                        .add_enabled(controls_enabled, egui::Checkbox::new(&mut value, "Enabled"))
                        .changed();
                });
            });
            if changed {
                workspace.send(Command::Setting(key.clone(), Value::Bool(value)));
            }
        }
        SettingKind::Text { .. } | SettingKind::StringList { .. } | SettingKind::Number { .. } => {
            ui.label(egui::RichText::new(&row.descriptor.label).strong());
            ui.label(
                egui::RichText::new(&row.descriptor.description)
                    .size(12.0)
                    .color(theme::MUTED),
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
                    if ui
                        .add_enabled(controls_enabled, egui::Button::new("Save"))
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
    ui.add_space(4.0);
    theme::section_label(ui, "Backend diagnostics");
    egui::Frame::new()
        .fill(theme::SIDEBAR)
        .inner_margin(egui::Margin::same(10))
        .corner_radius(egui::CornerRadius::same(8))
        .show(ui, |ui| {
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
        });
    ui.add_space(8.0);
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
        BackendMode::UserSession => "Discord account",
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
            rendered.contains("Experimental account sign-in uses Discord's unsupported password endpoint. Discord forbids account automation and may terminate accounts.")
        );
        assert!(rendered.contains("Email or verified phone"));
        assert!(rendered.contains("Discord account"));
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
