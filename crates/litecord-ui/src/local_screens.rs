//! Local review and configuration screens backed by app snapshots.
//!
//! These panels never read the database or feature registry directly. They
//! render the latest bridge snapshot and send explicit commands for changes.

use crate::{bridge::Command, theme, workspace::Workspace};
use eframe::egui::{self, Ui};
use litecord_app::view::{SettingRow, SettingsViewModel};
use litecord_features::feature::{FeatureInfo, SettingKind};
use litecord_types::{provenance::Origin, social::SessionState, tasks::Task};
use serde_json::Value;

impl Workspace {
    /// Render pending task candidates and confirmed open tasks.
    pub fn tasks_screen(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.spinner();
            ui.label("Loading tasks…");
            return;
        };

        egui::ScrollArea::vertical()
            .id_salt("tasks_screen")
            .show(ui, |ui| {
                ui.heading("Tasks");
                ui.label(
                    egui::RichText::new("Review suggestions, then track confirmed tasks here.")
                        .color(theme::MUTED),
                );
                ui.add_space(12.0);

                theme::section_label(
                    ui,
                    &format!("Pending review · {}", snapshot.tasks.candidates.len()),
                );
                if snapshot.tasks.candidates.is_empty() {
                    quiet_empty(ui, "No task suggestions need review.");
                } else {
                    for task in &snapshot.tasks.candidates {
                        task_card(self, ui, task, true);
                    }
                }

                ui.add_space(16.0);
                theme::section_label(ui, &format!("Open · {}", snapshot.tasks.open.len()));
                if snapshot.tasks.open.is_empty() {
                    quiet_empty(ui, "No open tasks.");
                } else {
                    for task in &snapshot.tasks.open {
                        task_card(self, ui, task, false);
                    }
                }
            });
    }

    /// Render recent memory with provenance and explicit candidate review.
    pub fn memory_screen(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.spinner();
            ui.label("Loading memory…");
            return;
        };

        egui::ScrollArea::vertical()
            .id_salt("memory_screen")
            .show(ui, |ui| {
                ui.heading("Memory");
                ui.label(
                    egui::RichText::new(
                        "Review what Litecord has retained, including its source and confidence.",
                    )
                    .color(theme::MUTED),
                );
                ui.add_space(12.0);

                let candidates = snapshot
                    .memory
                    .memories
                    .iter()
                    .filter(|item| item.status == litecord_types::memory::MemoryStatus::Candidate)
                    .count();
                theme::section_label(
                    ui,
                    &format!(
                        "Recent records · {} · {candidates} to review",
                        snapshot.memory.memories.len()
                    ),
                );
                if snapshot.memory.memories.is_empty() {
                    quiet_empty(ui, "No memory records are available in this snapshot.");
                } else {
                    for item in &snapshot.memory.memories {
                        memory_card(self, ui, item);
                    }
                }
            });
    }

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
                    egui::RichText::new(
                        "Available controls come from the features compiled into this build.",
                    )
                    .color(theme::MUTED),
                );
                ui.add_space(12.0);

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

                backend_diagnostics(ui, &settings, &snapshot.diagnostics);
            });
    }
}

fn task_card(workspace: &mut Workspace, ui: &mut Ui, task: &Task, candidate: bool) {
    egui::Frame::new()
        .fill(theme::SIDEBAR)
        .inner_margin(egui::Margin::same(10))
        .corner_radius(egui::CornerRadius::same(8))
        .show(ui, |ui| {
            if ui
                .selectable_label(workspace.selected_task == Some(task.id), "Task details")
                .clicked()
            {
                workspace.selected_task = Some(task.id);
            }
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(workspace.display(&task.title))
                            .strong()
                            .color(theme::TEXT),
                    );
                    if let Some(description) = task.description.as_deref().filter(|s| !s.is_empty())
                    {
                        ui.label(
                            egui::RichText::new(workspace.display(description))
                                .color(theme::SECONDARY),
                        );
                    }
                    ui.horizontal_wrapped(|ui| {
                        theme::chip(ui, &pretty_origin(task.origin), theme::MUTED);
                        if let Some(due) = task.due_at.and_then(relative_deadline) {
                            theme::chip(ui, &due, theme::PRIORITY);
                        }
                        if let Some(source) = &task.source {
                            if let Some(note) = source.note.as_deref() {
                                if !note.is_empty() {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "Source · {}",
                                            workspace.display(note)
                                        ))
                                        .size(12.0)
                                        .color(theme::MUTED),
                                    );
                                }
                            } else {
                                ui.label(
                                    egui::RichText::new("Source linked")
                                        .size(12.0)
                                        .color(theme::MUTED),
                                );
                            }
                        }
                    });
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let enabled = !workspace.busy;
                    if candidate {
                        if ui
                            .add_enabled(enabled, egui::Button::new("Dismiss"))
                            .clicked()
                        {
                            workspace.send(Command::DismissTask(task.id));
                        }
                        if ui
                            .add_enabled(enabled, egui::Button::new("Confirm"))
                            .clicked()
                        {
                            workspace.send(Command::ConfirmTask(task.id));
                        }
                    } else if ui
                        .add_enabled(enabled, egui::Button::new("Complete"))
                        .clicked()
                    {
                        workspace.send(Command::CompleteTask(task.id));
                    }
                });
            });
        });
    ui.add_space(6.0);
}

fn memory_card(workspace: &mut Workspace, ui: &mut Ui, item: &litecord_types::memory::MemoryItem) {
    egui::Frame::new()
        .fill(theme::SIDEBAR)
        .inner_margin(egui::Margin::same(10))
        .corner_radius(egui::CornerRadius::same(8))
        .show(ui, |ui| {
            if ui
                .selectable_label(workspace.selected_memory == Some(item.id), "Memory details")
                .clicked()
            {
                workspace.selected_memory = Some(item.id);
            }
            ui.label(
                egui::RichText::new(workspace.display(&item.content))
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                theme::chip(ui, item.kind.as_str(), theme::MUTED);
                theme::chip(ui, item.status.as_str(), theme::PRIMARY);
                theme::chip(ui, &pretty_origin(item.origin), theme::MUTED);
                theme::chip(
                    ui,
                    &format!("Confidence · {:.0}%", item.confidence.get() * 100.0),
                    theme::OMNI,
                );
                ui.label(
                    egui::RichText::new(format!("{} source reference(s)", item.source_refs.len()))
                        .size(12.0)
                        .color(theme::MUTED),
                );
            });

            if item.status == litecord_types::memory::MemoryStatus::Candidate {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let enabled = !workspace.busy;
                    if ui
                        .add_enabled(enabled, egui::Button::new("Confirm"))
                        .clicked()
                    {
                        workspace.send(Command::ConfirmMemory(item.id));
                    }
                    if ui
                        .add_enabled(enabled, egui::Button::new("Reject"))
                        .clicked()
                    {
                        workspace.send(Command::RejectMemory(item.id));
                    }
                });
            }
        });
    ui.add_space(6.0);
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

fn pretty_origin(origin: Origin) -> String {
    origin.as_str().replace('_', " ")
}

fn relative_deadline(deadline: litecord_types::Timestamp) -> Option<String> {
    if deadline.as_millis() <= 0 {
        return None;
    }
    let now = litecord_types::Timestamp::now();
    let (amount, future) = if deadline > now {
        (deadline.since(now).as_millis(), true)
    } else {
        (now.since(deadline).as_millis(), false)
    };
    let (value, unit) = if amount >= 86_400_000 {
        (amount / 86_400_000, "d")
    } else if amount >= 3_600_000 {
        (amount / 3_600_000, "h")
    } else {
        ((amount / 60_000).max(1), "m")
    };
    Some(if future {
        format!("Due in {value}{unit}")
    } else {
        format!("Overdue {value}{unit}")
    })
}

fn backend_label(mode: litecord_types::capability::BackendMode) -> &'static str {
    use litecord_types::capability::BackendMode;
    match mode {
        BackendMode::FullSocialSdk => "Full Social SDK",
        BackendMode::PresenceOnly => "Presence only",
        BackendMode::Demo => "Demo · synthetic",
        BackendMode::BotBridge => "Bot bridge",
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
