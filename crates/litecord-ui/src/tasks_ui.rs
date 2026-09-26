//! Tasks destination: compact task list, contextual sidebar and inspector.
//!
//! Everything renders from the latest bridge snapshot; writes go through
//! explicit bridge commands. Section filters live in `Workspace::filter`
//! (`tasks:suggested`, `tasks:open`, `tasks:reminders`), which navigation
//! already clears.

use crate::{bridge::Command, theme, workspace::Workspace};
use eframe::egui::{self, Color32, RichText, Ui};
use litecord_app::view::TaskDetailViewModel;
use litecord_types::{
    entity::EntityId,
    ids::TaskId,
    provenance::Origin,
    tasks::{Reminder, Task, TaskDraft, TaskPriority, TaskStatus},
    Timestamp,
};

const FILTER_SUGGESTED: &str = "tasks:suggested";
const FILTER_OPEN: &str = "tasks:open";
const FILTER_REMINDERS: &str = "tasks:reminders";
const ROW_HEIGHT: f32 = 36.0;

impl Workspace {
    /// Center panel: new-task row, then Suggested / Open / Reminders.
    pub(crate) fn render_tasks_screen(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Loading tasks…").color(theme::MUTED));
            });
            return;
        };
        let tasks = &snapshot.tasks;

        ui.horizontal(|ui| {
            ui.heading("Tasks");
            if let Some(label) = filter_label(&self.filter) {
                ui.label(RichText::new(format!("· {label}")).color(theme::MUTED));
                if ui
                    .link(RichText::new("Show all").size(12.0))
                    .on_hover_text("Clear the section filter")
                    .clicked()
                {
                    self.filter.clear();
                }
            }
        });
        ui.add_space(4.0);
        self.new_task_row(ui, None);
        ui.add_space(12.0);

        let filter = self.filter.clone();
        let show = |section: &str| !filter.starts_with("tasks:") || filter == section;

        egui::ScrollArea::vertical()
            .id_salt("tasks_screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if show(FILTER_SUGGESTED) {
                    theme::section_label(ui, &format!("Suggested · {}", tasks.candidates.len()));
                    if tasks.candidates.is_empty() {
                        quiet(ui, "No suggestions to review.");
                    } else {
                        for task in &tasks.candidates {
                            task_row(self, ui, task, true, 0);
                        }
                    }
                    ui.add_space(16.0);
                }

                if show(FILTER_OPEN) {
                    let open = sorted_open(&tasks.open);
                    theme::section_label(ui, &format!("Open · {}", open.len()));
                    if open.is_empty() {
                        quiet(ui, "No open tasks. Add one above or confirm a suggestion.");
                    } else {
                        for task in &open {
                            let subtasks = tasks
                                .open
                                .iter()
                                .filter(|t| t.parent_id == Some(task.id))
                                .count();
                            task_row(self, ui, task, false, subtasks);
                        }
                    }
                    ui.add_space(16.0);
                }

                if show(FILTER_REMINDERS) {
                    theme::section_label(ui, &format!("Reminders · {}", tasks.reminders.len()));
                    if tasks.reminders.is_empty() {
                        quiet(ui, "No reminders scheduled.");
                    } else {
                        let mut reminders: Vec<&Reminder> = tasks.reminders.iter().collect();
                        reminders.sort_by_key(|r| r.trigger.due_at());
                        for reminder in reminders {
                            reminder_row(self, ui, reminder);
                        }
                    }
                }
            });
    }

    /// Left contextual panel: section counts as filters and a "Due soon" list.
    pub fn tasks_sidebar(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        let tasks = &snapshot.tasks;
        theme::section_label(ui, "Tasks");
        ui.add_space(4.0);
        let open_count = sorted_open(&tasks.open).len();
        for (filter, label, count) in [
            (
                "",
                "All",
                tasks.candidates.len() + open_count + tasks.reminders.len(),
            ),
            (FILTER_SUGGESTED, "Suggested", tasks.candidates.len()),
            (FILTER_OPEN, "Open", open_count),
            (FILTER_REMINDERS, "Reminders", tasks.reminders.len()),
        ] {
            let selected = if filter.is_empty() {
                !self.filter.starts_with("tasks:")
            } else {
                self.filter == filter
            };
            if sidebar_item(ui, label, count, selected).clicked() {
                self.filter = filter.to_owned();
            }
        }

        ui.add_space(16.0);
        theme::section_label(ui, "Due soon");
        ui.add_space(4.0);
        let mut due: Vec<(Timestamp, String, Option<TaskId>)> = tasks
            .open
            .iter()
            .filter_map(|t| t.due_at.map(|d| (d, t.title.clone(), Some(t.id))))
            .chain(
                tasks
                    .reminders
                    .iter()
                    .map(|r| (r.trigger.due_at(), r.title.clone(), r.task_id)),
            )
            .filter(|(d, _, _)| d.as_millis() > 0)
            .collect();
        due.sort_by_key(|(d, _, _)| *d);
        if due.is_empty() {
            quiet(ui, "Nothing has a due time.");
        }
        for (at, title, task) in due.into_iter().take(5) {
            let title = self.display(&title);
            let (text, overdue) = relative_due(at);
            let response = ui
                .scope_builder(
                    egui::UiBuilder::new().sense(if task.is_some() {
                        egui::Sense::click()
                    } else {
                        egui::Sense::hover()
                    }),
                    |ui| {
                        let selected = task.is_some() && task == self.selected_task;
                        row_frame(selected).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.add(
                                    egui::Label::new(RichText::new(&title).color(theme::TEXT))
                                        .truncate(),
                                );
                                ui.label(RichText::new(text).size(12.0).color(if overdue {
                                    theme::PRIORITY
                                } else {
                                    theme::MUTED
                                }));
                            });
                        });
                    },
                )
                .response;
            if let Some(id) = task {
                if response.on_hover_text(&title).clicked() {
                    self.select_task(id);
                }
            }
        }
    }

    /// Right panel: details of the selected task.
    pub fn task_inspector(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        let Some(selected) = self.selected_task else {
            theme::section_label(ui, "Task");
            quiet(ui, "Select a task to see details.");
            return;
        };
        if self.selection.task != Some(selected) {
            self.selection.task = Some(selected);
            self.request();
        }
        let Some(detail) = snapshot
            .task_detail
            .as_ref()
            .filter(|d| d.task.id == selected)
        else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Loading task…").color(theme::MUTED));
            });
            return;
        };

        egui::ScrollArea::vertical()
            .id_salt("task_inspector")
            .auto_shrink([false, false])
            .show(ui, |ui| self.inspector_body(ui, &snapshot, detail));
    }

    fn inspector_body(
        &mut self,
        ui: &mut Ui,
        snapshot: &crate::bridge::Snapshot,
        detail: &TaskDetailViewModel,
    ) {
        let task = &detail.task;
        let enabled = !self.busy;
        ui.add(
            egui::Label::new(
                RichText::new(self.display(&task.title))
                    .size(16.0)
                    .strong()
                    .color(theme::TEXT),
            )
            .wrap(),
        );
        if let Some(description) = task.description.as_deref().filter(|s| !s.is_empty()) {
            ui.label(RichText::new(self.display(description)).color(theme::SECONDARY));
        }
        ui.add_space(4.0);

        egui::Grid::new("task_inspector_meta")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                meta_key(ui, "Status");
                ui.label(RichText::new(status_label(task.status)).color(theme::TEXT));
                ui.end_row();

                meta_key(ui, "Priority");
                let mut priority = task.priority;
                ui.add_enabled_ui(enabled && task.status == TaskStatus::Open, |ui| {
                    priority_combo(ui, "inspector_priority", &mut priority);
                });
                if priority != task.priority {
                    self.send(Command::SetTaskPriority(task.id, priority));
                }
                ui.end_row();

                meta_key(ui, "Due");
                match task.due_at.filter(|d| d.as_millis() > 0) {
                    Some(at) => {
                        let (text, overdue) = relative_due(at);
                        ui.label(RichText::new(text).color(if overdue {
                            theme::PRIORITY
                        } else {
                            theme::TEXT
                        }));
                    }
                    None => {
                        ui.label(RichText::new("No due date").color(theme::MUTED));
                    }
                }
                ui.end_row();

                meta_key(ui, "Origin");
                ui.label(RichText::new(origin_label(task)).color(theme::TEXT));
                ui.end_row();
            });

        if let Some(conversation) = task.conversation_id {
            let name = snapshot
                .conversations
                .conversations
                .iter()
                .find(|c| c.conversation_id == conversation)
                .map(|c| self.display(&c.title))
                .unwrap_or_else(|| "a conversation".into());
            ui.add_space(4.0);
            let from_message = matches!(
                task.source.as_ref().map(|s| &s.entity),
                Some(EntityId::Message(_))
            );
            let text = if from_message {
                format!("Created from a message in {name}")
            } else {
                format!("Linked to {name}")
            };
            ui.label(RichText::new(text).size(12.0).color(theme::SECONDARY));
            if ui.button("Open conversation").clicked() {
                self.open_conversation(conversation);
            }
        } else if let Some(note) = task
            .source
            .as_ref()
            .and_then(|s| s.note.as_deref())
            .filter(|n| !n.is_empty())
        {
            ui.label(
                RichText::new(format!("Source · {}", self.display(note)))
                    .size(12.0)
                    .color(theme::SECONDARY),
            );
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| match task.status {
            TaskStatus::Candidate => {
                if ui
                    .add_enabled(enabled, egui::Button::new("Confirm"))
                    .clicked()
                {
                    self.send(Command::ConfirmTask(task.id));
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("Dismiss"))
                    .clicked()
                {
                    self.send(Command::DismissTask(task.id));
                }
            }
            TaskStatus::Open
                if ui
                    .add_enabled(enabled, egui::Button::new("Complete"))
                    .clicked() =>
            {
                self.send(Command::CompleteTask(task.id));
            }
            _ => {}
        });

        // Subtasks are one level deep: only top-level tasks accept new ones.
        ui.add_space(12.0);
        ui.separator();
        let done = detail
            .subtasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .count();
        theme::section_label(ui, &format!("Subtasks · {done}/{}", detail.subtasks.len()));
        if detail.subtasks.is_empty() {
            quiet(ui, "No subtasks.");
        }
        for sub in &detail.subtasks {
            ui.horizontal(|ui| {
                let mut checked = sub.status == TaskStatus::Done;
                let can_complete = enabled && sub.status == TaskStatus::Open;
                let response = ui
                    .add_enabled(can_complete, egui::Checkbox::without_text(&mut checked))
                    .on_hover_text("Complete subtask");
                if response.changed() && checked {
                    self.send(Command::CompleteTask(sub.id));
                }
                let title = self.display(&sub.title);
                let mut text = RichText::new(&title);
                text = if sub.status == TaskStatus::Done {
                    text.strikethrough().color(theme::MUTED)
                } else {
                    text.color(theme::TEXT)
                };
                ui.add(egui::Label::new(text).truncate())
                    .on_hover_text(&title);
            });
        }
        if task.parent_id.is_none() && task.status == TaskStatus::Open {
            self.new_task_row(ui, Some(task.id));
        }

        ui.add_space(12.0);
        ui.separator();
        theme::section_label(ui, &format!("Comments · {}", detail.comments.len()));
        if detail.comments.is_empty() {
            quiet(ui, "No comments yet.");
        }
        for comment in &detail.comments {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(comment_author(comment.origin))
                            .size(12.0)
                            .strong()
                            .color(theme::SECONDARY),
                    );
                    ui.label(
                        RichText::new(relative_ago(comment.created_at))
                            .size(12.0)
                            .color(theme::MUTED),
                    );
                });
                ui.add(
                    egui::Label::new(RichText::new(self.display(&comment.body)).color(theme::TEXT))
                        .wrap(),
                );
            });
            ui.add_space(4.0);
        }
        let mut submit = false;
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.task_comment_draft)
                    .hint_text("Add a comment")
                    .margin(egui::Margin::symmetric(8, 6))
                    .desired_width((ui.available_width() - 64.0).max(80.0)),
            );
            let ready = enabled && !self.task_comment_draft.trim().is_empty();
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && ready {
                submit = true;
                edit.request_focus();
            }
            if ui.add_enabled(ready, egui::Button::new("Post")).clicked() {
                submit = true;
            }
        });
        if submit {
            let body = self.task_comment_draft.trim().to_owned();
            self.send(Command::AddTaskComment(task.id, body));
            if self.busy {
                self.task_comment_draft.clear();
            }
        }
    }

    /// Select a task and load its detail view for the inspector.
    fn select_task(&mut self, id: TaskId) {
        self.selected_task = Some(id);
        self.selection.task = Some(id);
        self.request();
    }

    /// Single-line creation row. `parent` makes it an "Add subtask" row.
    fn new_task_row(&mut self, ui: &mut Ui, parent: Option<TaskId>) {
        let enabled = !self.busy;
        let mut submit = false;
        // Subtask drafts live in egui temp memory; the workspace only owns
        // the top-level title/priority drafts.
        let sub_id = egui::Id::new(("task_subtask_draft", parent));
        let mut sub_draft = match parent {
            Some(_) => ui.data_mut(|d| d.get_temp::<String>(sub_id).unwrap_or_default()),
            None => String::new(),
        };
        ui.horizontal(|ui| {
            let reserve = if parent.is_some() { 64.0 } else { 190.0 };
            let draft = if parent.is_some() {
                &mut sub_draft
            } else {
                &mut self.task_title_draft
            };
            let edit = ui.add(
                egui::TextEdit::singleline(draft)
                    .hint_text(if parent.is_some() {
                        "Add subtask"
                    } else {
                        "New task"
                    })
                    .margin(egui::Margin::symmetric(8, 6))
                    .desired_width((ui.available_width() - reserve).max(120.0)),
            );
            let ready = enabled && !draft.trim().is_empty();
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && ready {
                submit = true;
                edit.request_focus();
            }
            if parent.is_none() {
                ui.add_enabled_ui(enabled, |ui| {
                    priority_combo(ui, "new_task_priority", &mut self.task_priority_draft);
                });
            }
            if ui
                .add_enabled(ready, egui::Button::new("Add"))
                .on_hover_text(if parent.is_some() {
                    "Add subtask"
                } else {
                    "Add task"
                })
                .clicked()
            {
                submit = true;
            }
        });
        if submit {
            let (title, priority) = match parent {
                Some(_) => (sub_draft.trim().to_owned(), TaskPriority::Normal),
                None => (
                    self.task_title_draft.trim().to_owned(),
                    self.task_priority_draft,
                ),
            };
            self.send(Command::CreateTask(TaskDraft {
                title,
                description: None,
                priority,
                due_at: None,
                related_users: Vec::new(),
                conversation_id: None,
                parent_id: parent,
                source: None,
            }));
            if self.busy {
                match parent {
                    Some(_) => sub_draft.clear(),
                    None => self.task_title_draft.clear(),
                }
            }
        }
        if parent.is_some() {
            ui.data_mut(|d| d.insert_temp(sub_id, sub_draft));
        }
    }
}

/// One compact task row: checkbox, priority marker, title, chips, actions.
fn task_row(workspace: &mut Workspace, ui: &mut Ui, task: &Task, candidate: bool, subtasks: usize) {
    let selected = workspace.selected_task == Some(task.id);
    let enabled = !workspace.busy;
    let title = workspace.display(&task.title);
    let response = ui
        .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
            row_frame(selected).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().button_padding = egui::vec2(8.0, 3.0);
                ui.spacing_mut().interact_size.y = 24.0;
                row_line(ui, |ui| {
                    if candidate {
                        ui.add_space(22.0);
                    } else {
                        let mut done = false;
                        if ui
                            .add_enabled(enabled, egui::Checkbox::without_text(&mut done))
                            .on_hover_text("Complete task")
                            .changed()
                            && done
                        {
                            workspace.send(Command::CompleteTask(task.id));
                        }
                    }
                    priority_marker(ui, task.priority);

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                        }
                        small_chip(ui, &origin_label(task), theme::MUTED, theme::MUTED);
                        if let Some(at) = task.due_at.filter(|d| d.as_millis() > 0) {
                            let (text, overdue) = relative_due(at);
                            if overdue {
                                small_chip(ui, &text, theme::PRIORITY, theme::PRIORITY);
                            } else {
                                small_chip(ui, &text, theme::PRIMARY, theme::SECONDARY);
                            }
                        }
                        if subtasks > 0 {
                            let text = format!(
                                "{subtasks} subtask{}",
                                if subtasks == 1 { "" } else { "s" }
                            );
                            ui.label(RichText::new(text).size(12.0).color(theme::MUTED));
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            if let Some(label) = priority_text(task.priority) {
                                ui.label(
                                    RichText::new(label)
                                        .size(12.0)
                                        .strong()
                                        .color(priority_color(task.priority)),
                                );
                            }
                            ui.add(
                                egui::Label::new(RichText::new(&title).color(theme::TEXT))
                                    .truncate()
                                    .selectable(false),
                            )
                            .on_hover_text(&title);
                        });
                    });
                });
            });
        })
        .response;
    if response.clicked() {
        workspace.select_task(task.id);
    }
    ui.add_space(2.0);
}

fn reminder_row(workspace: &mut Workspace, ui: &mut Ui, reminder: &Reminder) {
    let selected = reminder.task_id.is_some() && reminder.task_id == workspace.selected_task;
    let title = workspace.display(&reminder.title);
    let sense = if reminder.task_id.is_some() {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let response = ui
        .scope_builder(egui::UiBuilder::new().sense(sense), |ui| {
            row_frame(selected).show(ui, |ui| {
                ui.set_width(ui.available_width());
                row_line(ui, |ui| {
                    ui.add_space(22.0);
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_stroke(
                        rect.center(),
                        3.5,
                        egui::Stroke::new(1.5_f32, theme::OMNI),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        small_chip(
                            ui,
                            &pretty_origin(reminder.origin),
                            theme::MUTED,
                            theme::MUTED,
                        );
                        let (text, overdue) = relative_due(reminder.trigger.due_at());
                        let text = if overdue {
                            text.replace("Overdue", "Was due")
                        } else {
                            text
                        };
                        small_chip(ui, &text, theme::OMNI, theme::SECONDARY);
                        if matches!(
                            reminder.trigger,
                            litecord_types::tasks::ReminderTrigger::Conditional { .. }
                        ) {
                            ui.label(RichText::new("If no reply").size(12.0).color(theme::MUTED));
                        }
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(&title).color(theme::TEXT))
                                    .truncate()
                                    .selectable(false),
                            )
                            .on_hover_text(&title);
                        });
                    });
                });
            });
        })
        .response;
    if let Some(id) = reminder.task_id {
        if response.clicked() {
            workspace.select_task(id);
        }
    }
    ui.add_space(2.0);
}

/// A left-to-right line of bounded height, so right-aligned children stay
/// on the row instead of stretching to the remaining panel height.
fn row_line(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    row_line_height(ui, ROW_HEIGHT - 8.0, add);
}

fn row_line_height(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_height(height);
            add(ui);
        },
    );
}

fn row_frame(selected: bool) -> egui::Frame {
    egui::Frame::new()
        .fill(if selected {
            theme::SELECTED
        } else {
            theme::SIDEBAR
        })
        .stroke(egui::Stroke::new(
            1.0_f32,
            if selected {
                theme::PRIMARY.gamma_multiply(0.6)
            } else {
                Color32::TRANSPARENT
            },
        ))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(8, 4))
}

fn sidebar_item(ui: &mut Ui, label: &str, count: usize, selected: bool) -> egui::Response {
    ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
        let hovered = ui.response().hovered();
        egui::Frame::new()
            .fill(if selected {
                theme::SELECTED
            } else if hovered {
                theme::RAISED
            } else {
                Color32::TRANSPARENT
            })
            .corner_radius(egui::CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                row_line_height(ui, 20.0, |ui| {
                    ui.label(RichText::new(label).color(if selected {
                        theme::TEXT
                    } else {
                        theme::SECONDARY
                    }));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(count.to_string())
                                .size(12.0)
                                .color(theme::MUTED),
                        );
                    });
                });
            });
    })
    .response
    .on_hover_text(format!("Show {label}"))
}

fn priority_marker(ui: &mut Ui, priority: TaskPriority) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(4.0, 18.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(2), priority_color(priority));
    let label = format!("Priority: {}", priority_name(priority));
    response
        .on_hover_text(&label)
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &label));
}

fn priority_combo(ui: &mut Ui, id: &str, priority: &mut TaskPriority) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(priority_name(*priority))
        .width(92.0)
        .show_ui(ui, |ui| {
            for p in [
                TaskPriority::Low,
                TaskPriority::Normal,
                TaskPriority::High,
                TaskPriority::Urgent,
            ] {
                ui.selectable_value(priority, p, priority_name(p));
            }
        })
        .response
        .on_hover_text("Priority");
}

fn small_chip(ui: &mut Ui, text: &str, tint: Color32, text_color: Color32) {
    egui::Frame::new()
        .fill(tint.gamma_multiply(0.12))
        .stroke(egui::Stroke::new(1.0_f32, tint.gamma_multiply(0.32)))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(RichText::new(text).size(12.0).color(text_color))
                    .selectable(false),
            );
        });
}

fn meta_key(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(theme::MUTED));
}

fn quiet(ui: &mut Ui, message: &str) {
    ui.label(RichText::new(message).color(theme::MUTED));
}

/// Open top-level tasks (subtasks of an open parent are shown in the
/// inspector), highest priority first, then earliest due date.
fn sorted_open(open: &[Task]) -> Vec<Task> {
    let mut tasks: Vec<Task> = open
        .iter()
        .filter(|t| {
            t.parent_id
                .is_none_or(|parent| !open.iter().any(|p| p.id == parent))
        })
        .cloned()
        .collect();
    tasks.sort_by(|a, b| {
        b.priority.cmp(&a.priority).then_with(|| {
            let due = |t: &Task| t.due_at.filter(|d| d.as_millis() > 0);
            match (due(a), due(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => b.created_at.cmp(&a.created_at),
            }
        })
    });
    tasks
}

fn filter_label(filter: &str) -> Option<&'static str> {
    match filter {
        FILTER_SUGGESTED => Some("Suggested"),
        FILTER_OPEN => Some("Open"),
        FILTER_REMINDERS => Some("Reminders"),
        _ => None,
    }
}

fn priority_name(priority: TaskPriority) -> &'static str {
    match priority {
        TaskPriority::Low => "Low",
        TaskPriority::Normal => "Normal",
        TaskPriority::High => "High",
        TaskPriority::Urgent => "Urgent",
    }
}

/// Visible priority text; Normal stays quiet (its marker still has a label).
fn priority_text(priority: TaskPriority) -> Option<&'static str> {
    (priority != TaskPriority::Normal).then(|| priority_name(priority))
}

fn priority_color(priority: TaskPriority) -> Color32 {
    match priority {
        TaskPriority::Urgent => theme::PRIORITY,
        TaskPriority::High => theme::PRIORITY.gamma_multiply(0.7),
        TaskPriority::Normal => theme::PRIMARY.gamma_multiply(0.8),
        TaskPriority::Low => theme::MUTED.gamma_multiply(0.7),
    }
}

fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Candidate => "Suggested · needs review",
        TaskStatus::Open => "Open",
        TaskStatus::Done => "Done",
        TaskStatus::Cancelled => "Cancelled",
        TaskStatus::Dismissed => "Dismissed",
    }
}

fn origin_label(task: &Task) -> String {
    let from_message = matches!(
        task.source.as_ref().map(|s| &s.entity),
        Some(EntityId::Message(_))
    );
    match task.origin {
        Origin::AgentDerived => "From Omni".into(),
        Origin::UserProvided if from_message => "From a message".into(),
        Origin::UserProvided => "Added by you".into(),
        Origin::LocalApplication if from_message => "From your messages".into(),
        Origin::LocalApplication => "Automatic".into(),
        Origin::DiscordSocialSdk | Origin::DiscordUserSession | Origin::DiscordBotGateway => {
            "From Discord".into()
        }
        Origin::Imported => "Imported".into(),
        Origin::Synthetic => "Demo data".into(),
    }
}

fn pretty_origin(origin: Origin) -> String {
    match origin {
        Origin::AgentDerived => "From Omni".into(),
        Origin::UserProvided => "Added by you".into(),
        Origin::LocalApplication => "Automatic".into(),
        Origin::Synthetic => "Demo data".into(),
        other => {
            let s = other.as_str().replace('_', " ");
            let mut chars = s.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

fn comment_author(origin: Origin) -> String {
    match origin {
        Origin::UserProvided => "You".into(),
        Origin::AgentDerived => "Omni".into(),
        other => pretty_origin(other),
    }
}

fn span(ms: u64) -> String {
    if ms >= 86_400_000 {
        format!("{}d", ms / 86_400_000)
    } else if ms >= 3_600_000 {
        format!("{}h", ms / 3_600_000)
    } else {
        format!("{}m", (ms / 60_000).max(1))
    }
}

/// ("Due in 6h", false) or ("Overdue 2d", true).
fn relative_due(at: Timestamp) -> (String, bool) {
    let now = Timestamp::now();
    if at > now {
        (format!("Due in {}", span(at.since(now).as_millis())), false)
    } else {
        (format!("Overdue {}", span(now.since(at).as_millis())), true)
    }
}

fn relative_ago(at: Timestamp) -> String {
    let now = Timestamp::now();
    if at >= now {
        "just now".into()
    } else {
        let ms = now.since(at).as_millis();
        if ms < 60_000 {
            "just now".into()
        } else {
            format!("{} ago", span(ms))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: i64, priority: TaskPriority, due: Option<i64>) -> Task {
        Task {
            id: TaskId(id),
            title: format!("t{id}"),
            description: None,
            status: TaskStatus::Open,
            priority,
            origin: Origin::UserProvided,
            source: None,
            related_users: Vec::new(),
            conversation_id: None,
            parent_id: None,
            due_at: due.map(Timestamp),
            created_at: Timestamp(1),
            updated_at: Timestamp(1),
            completed_at: None,
            revision: Default::default(),
        }
    }

    #[test]
    fn open_tasks_sort_by_priority_then_due_and_hide_nested_subtasks() {
        let mut sub = task(5, TaskPriority::Urgent, None);
        sub.parent_id = Some(TaskId(2));
        let open = vec![
            task(1, TaskPriority::Normal, Some(500)),
            task(2, TaskPriority::High, None),
            task(3, TaskPriority::Normal, Some(100)),
            task(4, TaskPriority::Normal, None),
            sub,
        ];
        let ids: Vec<i64> = sorted_open(&open).iter().map(|t| t.id.0).collect();
        assert_eq!(ids, vec![2, 3, 1, 4]);
    }

    #[test]
    fn relative_due_marks_overdue() {
        let (text, overdue) = relative_due(Timestamp(Timestamp::now().0 - 2 * 3_600_000));
        assert!(overdue);
        assert_eq!(text, "Overdue 2h");
        let (text, overdue) = relative_due(Timestamp(Timestamp::now().0 + 6 * 3_600_000 + 60_000));
        assert!(!overdue);
        assert_eq!(text, "Due in 6h");
    }
}
