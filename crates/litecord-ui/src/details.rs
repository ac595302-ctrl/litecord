use crate::{theme, workspace::Workspace};
use eframe::egui::{self, Ui};
use litecord_layout::Destination;

impl Workspace {
    pub fn details(&mut self, ui: &mut Ui) -> bool {
        let Some(s) = self.snapshot.clone() else {
            return false;
        };
        match self.selection.destination {
            Destination::Memory => {
                theme::section_label(ui, "Memory inspector");
                let memory = s
                    .memory
                    .memories
                    .iter()
                    .find(|m| Some(m.id) == self.selected_memory);
                if let Some(m) = memory {
                    ui.label(self.display(&m.content));
                    ui.separator();
                    ui.label(format!("{} · {}", m.kind.as_str(), m.status.as_str()));
                    ui.label(format!("Confidence {:.0}%", m.confidence.get() * 100.0));
                    ui.label(format!("Origin: {:?}", m.origin));
                    theme::section_label(ui, "Source references");
                    for source in &m.source_refs {
                        ui.label(self.display(&source.entity.to_string()));
                        if let Some(note) = &source.note {
                            ui.label(self.display(note));
                        }
                    }
                    theme::section_label(ui, "Related entities");
                    for entity in &m.entities {
                        ui.label(self.display(&entity.to_string()));
                    }
                    if let Some(id) = m.superseded_by {
                        ui.label(format!("Superseded by record {id}"));
                    }
                } else {
                    ui.label("Choose Memory details on a record to review its source.");
                }
                true
            }
            Destination::Tasks => {
                theme::section_label(ui, "Task inspector");
                let task = s
                    .tasks
                    .open
                    .iter()
                    .chain(&s.tasks.candidates)
                    .find(|t| Some(t.id) == self.selected_task);
                if let Some(task) = task {
                    ui.label(egui::RichText::new(self.display(&task.title)).strong());
                    if let Some(description) = &task.description {
                        ui.label(self.display(description));
                    }
                    ui.label(format!("{} · {:?}", task.status.as_str(), task.origin));
                    if let Some(source) = &task.source {
                        ui.label(self.display(&source.entity.to_string()));
                        if let Some(note) = &source.note {
                            ui.label(self.display(note));
                        }
                    }
                    if let Some(id) = task.conversation_id {
                        if ui.button("Open source conversation").clicked() {
                            self.open_conversation(id);
                        }
                    }
                } else {
                    ui.label("Choose Task details on a task to inspect its source.");
                }
                true
            }
            Destination::Servers => {
                theme::section_label(ui, "Channel details");
                if let Some(row) = s
                    .guilds
                    .guilds
                    .iter()
                    .flat_map(|g| &g.channels)
                    .find(|r| Some(r.channel.id) == self.selected_channel)
                {
                    ui.label(self.display(&row.channel.name));
                    ui.label(format!(
                        "{} · {}",
                        row.channel.kind.as_str(),
                        row.channel.access.as_str()
                    ));
                    ui.label("Availability is reported by the backend. Discord may provide additional channel features.");
                } else {
                    ui.label("Choose a channel to inspect its availability.");
                }
                true
            }
            Destination::Settings => {
                theme::section_label(ui, "Workspace layouts");
                ui.label(
                    "Create profiles and arrange your panels from the always-visible Layouts menu.",
                );
                if ui.button("Manage layouts").clicked() {
                    self.profile_manager = true;
                }
                ui.separator();
                theme::section_label(ui, "Runtime");
                ui.label(format!("Revision {}", s.diagnostics.revision.get()));
                if let Some(rss) = s.diagnostics.metrics.rss_bytes {
                    ui.label(format!("Resident memory {:.1} MiB", rss as f64 / 1048576.0));
                }
                true
            }
            _ => false,
        }
    }
}
