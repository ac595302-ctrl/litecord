//! Tasks (mock A11): views and priorities in the sidebar; a week strip,
//! tabs and priority groups in the main view; the selected task's details,
//! subtasks and comments in the inspector.
//!
//! Everything renders from the latest bridge snapshot; writes go through
//! explicit bridge commands. Rows paint into fixed rects with elided text,
//! so actions never clip at narrow widths.

use crate::{bridge::Command, kit, ph, theme, workspace::Workspace};
use eframe::egui::{self, Align2, Color32, Rect, Ui};
use litecord_app::view::TaskDetailViewModel;
use litecord_types::{
    entity::EntityId,
    ids::TaskId,
    provenance::Origin,
    tasks::{Reminder, Task, TaskDraft, TaskPriority, TaskStatus},
    Timestamp,
};

/// Sidebar views.
const VIEW_ALL: usize = 0;
const VIEW_SUGGESTED: usize = 1;
const VIEW_REMINDERS: usize = 2;
const VIEW_TODAY: usize = 3;

/// Main-view tabs.
const TABS: [&str; 4] = ["All", "Today", "Upcoming", "No date"];

const DAY_MS: i64 = 86_400_000;

impl Workspace {
    // ---- Sidebar -------------------------------------------------------------

    pub fn tasks_sidebar(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        let tasks = &snapshot.tasks;
        if kit::sidebar_title(ui, "Tasks", Some((ph::PLUS, "New task"))) {
            ui.ctx()
                .memory_mut(|m| m.request_focus(egui::Id::new("new_task_title")));
        }
        ui.add_space(6.0);
        kit::search(ui, &mut self.filter, "Search tasks...");
        ui.add_space(10.0);
        let open = sorted_open(&tasks.open);
        let today = open.iter().filter(|t| due_today_or_overdue(t)).count();
        let views: [(&str, Color32, &str, usize, usize); 4] = [
            (
                ph::LIST_CHECKS,
                kit::BLUE.fg,
                "My tasks",
                open.len(),
                VIEW_ALL,
            ),
            (
                ph::SPARKLE,
                theme::OMNI,
                "Suggested",
                tasks.candidates.len(),
                VIEW_SUGGESTED,
            ),
            (
                ph::BELL,
                theme::WARNING,
                "Reminders",
                tasks.reminders.len(),
                VIEW_REMINDERS,
            ),
            (
                ph::CALENDAR_BLANK,
                theme::PRIORITY,
                "Due today",
                today,
                VIEW_TODAY,
            ),
        ];
        ui.spacing_mut().item_spacing.y = 4.0;
        for (g, c, label, n, view) in views {
            if kit::side_item(
                ui,
                Some((g, c)),
                label,
                Some(n),
                self.task_view == view && self.task_priority_filter.is_none(),
            )
            .clicked()
            {
                self.task_view = view;
                self.task_priority_filter = None;
            }
        }
        ui.add_space(12.0);
        kit::section(ui, "Priorities", None, None);
        for (p, tint) in [
            (TaskPriority::Urgent, kit::RED),
            (TaskPriority::High, kit::ORANGE),
            (TaskPriority::Normal, kit::BLUE),
            (TaskPriority::Low, kit::GREY),
        ] {
            let n = open.iter().filter(|t| t.priority == p).count();
            let selected = self.task_priority_filter == Some(p);
            let (rect, resp) = kit::row(ui, 46.0, selected);
            let painter = ui.painter();
            kit::paint_tile(
                painter,
                Rect::from_center_size(
                    egui::pos2(rect.left() + 26.0, rect.center().y),
                    egui::vec2(32.0, 32.0),
                ),
                ph::FLAG,
                tint,
                9.0,
            );
            kit::text_at(
                painter,
                egui::pos2(rect.left() + 54.0, rect.center().y),
                Align2::LEFT_CENTER,
                priority_name(p),
                theme::medium(15.0),
                theme::TEXT,
                rect.width() - 90.0,
            );
            painter.text(
                rect.right_center() - egui::vec2(14.0, 0.0),
                Align2::RIGHT_CENTER,
                n.to_string(),
                theme::medium(14.0),
                theme::MUTED,
            );
            if resp.clicked() {
                self.task_priority_filter = if selected { None } else { Some(p) };
                self.task_view = VIEW_ALL;
            }
        }
        ui.add_space(12.0);
        kit::section(ui, "Due soon", None, None);
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
            kit::label(
                ui,
                "Nothing has a due time.",
                theme::regular(13.5),
                theme::MUTED,
            );
        }
        for (at, title, task) in due.into_iter().take(5) {
            let title = self.display(&title);
            let (text, overdue) = relative_due(at);
            let selected = task.is_some() && task == self.selected_task;
            let (rect, resp) = kit::row(ui, 48.0, selected);
            let painter = ui.painter();
            kit::icon(
                painter,
                egui::pos2(rect.left() + 18.0, rect.center().y),
                ph::CLOCK,
                17.0,
                if overdue {
                    theme::PRIORITY
                } else {
                    theme::MUTED
                },
            );
            kit::text_at(
                painter,
                egui::pos2(rect.left() + 40.0, rect.center().y - 9.0),
                Align2::LEFT_CENTER,
                &title,
                theme::medium(14.0),
                theme::TEXT,
                rect.width() - 48.0,
            );
            kit::text_at(
                painter,
                egui::pos2(rect.left() + 40.0, rect.center().y + 10.0),
                Align2::LEFT_CENTER,
                &text,
                theme::regular(12.5),
                if overdue {
                    theme::PRIORITY_TEXT
                } else {
                    theme::MUTED
                },
                rect.width() - 48.0,
            );
            if let Some(id) = task {
                if resp.on_hover_text(&title).clicked() {
                    self.select_task(id);
                }
            }
        }
    }

    // ---- Main --------------------------------------------------------------------

    /// Center panel: header, new-task bar, tabs, week strip, priority groups.
    pub(crate) fn render_tasks_screen(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.horizontal(|ui| {
                ui.spinner();
                kit::label(ui, "Loading tasks…", theme::regular(14.0), theme::MUTED);
            });
            return;
        };
        let tasks = &snapshot.tasks;
        egui::ScrollArea::vertical()
            .id_salt("tasks_screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let (hdr, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 58.0),
                    egui::Sense::hover(),
                );
                ui.painter().text(
                    hdr.left_top() + egui::vec2(0.0, 2.0),
                    Align2::LEFT_TOP,
                    "Tasks",
                    theme::semibold(27.0),
                    theme::TEXT,
                );
                ui.painter().text(
                    hdr.left_top() + egui::vec2(0.0, 38.0),
                    Align2::LEFT_TOP,
                    "Turn conversations into action.",
                    theme::regular(15.5),
                    theme::lerp(theme::SECONDARY, theme::PRIMARY_TEXT, 0.25),
                );
                let bw = kit::button_width(ui.painter(), Some(ph::PLUS), "New task", 38.0);
                let br = Rect::from_min_size(
                    egui::pos2(hdr.right() - bw, hdr.top() + 4.0),
                    egui::vec2(bw, 38.0),
                );
                if kit::button_at(
                    ui,
                    br,
                    ui.id().with("new_task_btn"),
                    kit::Kind::Primary,
                    Some(ph::PLUS),
                    "New task",
                    true,
                )
                .clicked()
                {
                    ui.ctx()
                        .memory_mut(|m| m.request_focus(egui::Id::new("new_task_title")));
                }
                self.new_task_bar(ui);
                ui.add_space(2.0);
                kit::tab_row(ui, &TABS, &mut self.task_tab);
                ui.add_space(4.0);
                self.week_strip(ui, tasks);
                ui.add_space(6.0);
                let needle = self.filter.trim().to_lowercase();
                let matches =
                    |t: &Task| needle.is_empty() || t.title.to_lowercase().contains(&needle);
                match self.task_view {
                    VIEW_SUGGESTED => {
                        let rows: Vec<Task> = tasks
                            .candidates
                            .iter()
                            .filter(|t| matches(t))
                            .cloned()
                            .collect();
                        self.task_group(
                            ui,
                            "Suggested for you",
                            ph::SPARKLE,
                            theme::OMNI,
                            &rows,
                            true,
                            tasks,
                        );
                        if rows.is_empty() {
                            kit::empty(
                                ui,
                                Some(ph::SPARKLE),
                                "No suggestions",
                                "Commitments you make in chat show up here to confirm.",
                            );
                        }
                    }
                    VIEW_REMINDERS => {
                        let mut reminders: Vec<&Reminder> = tasks
                            .reminders
                            .iter()
                            .filter(|r| {
                                needle.is_empty() || r.title.to_lowercase().contains(&needle)
                            })
                            .collect();
                        reminders.sort_by_key(|r| r.trigger.due_at());
                        group_header(ui, "Reminders", ph::BELL, theme::WARNING, reminders.len());
                        if reminders.is_empty() {
                            kit::empty(
                                ui,
                                Some(ph::BELL),
                                "No reminders",
                                "Reminders you or Omni set appear here.",
                            );
                        }
                        for r in reminders {
                            reminder_row(self, ui, r);
                        }
                    }
                    _ => {
                        let open: Vec<Task> = sorted_open(&tasks.open)
                            .into_iter()
                            .filter(|t| matches(t))
                            .filter(|t| self.task_priority_filter.is_none_or(|p| t.priority == p))
                            .filter(|t| self.task_view != VIEW_TODAY || due_today_or_overdue(t))
                            .filter(|t| tab_matches(self.task_tab, t))
                            .filter(|t| {
                                self.task_day
                                    .is_none_or(|d| t.due_at.is_some_and(|at| local_day(at) == d))
                            })
                            .collect();
                        if !tasks.candidates.is_empty()
                            && self.task_view == VIEW_ALL
                            && self.task_priority_filter.is_none()
                            && self.task_day.is_none()
                        {
                            let rows: Vec<Task> = tasks
                                .candidates
                                .iter()
                                .filter(|t| matches(t))
                                .take(3)
                                .cloned()
                                .collect();
                            self.task_group(
                                ui,
                                "Suggested for you",
                                ph::SPARKLE,
                                theme::OMNI,
                                &rows,
                                true,
                                tasks,
                            );
                            ui.add_space(6.0);
                        }
                        if open.is_empty() {
                            kit::empty(
                                ui,
                                Some(ph::CHECK_CIRCLE),
                                "No open tasks",
                                "Add one above, or confirm a suggestion.",
                            );
                        }
                        let high: Vec<Task> = open
                            .iter()
                            .filter(|t| t.priority >= TaskPriority::High)
                            .cloned()
                            .collect();
                        let normal: Vec<Task> = open
                            .iter()
                            .filter(|t| t.priority == TaskPriority::Normal)
                            .cloned()
                            .collect();
                        let low: Vec<Task> = open
                            .iter()
                            .filter(|t| t.priority == TaskPriority::Low)
                            .cloned()
                            .collect();
                        for (title, color, rows) in [
                            ("High priority", theme::PRIORITY, &high),
                            ("Normal priority", theme::PRIMARY, &normal),
                            ("Low priority", theme::MUTED, &low),
                        ] {
                            if !rows.is_empty() {
                                self.task_group(ui, title, ph::TARGET, color, rows, false, tasks);
                                ui.add_space(6.0);
                            }
                        }
                    }
                }
                ui.add_space(12.0);
            });
    }

    /// Title field, priority pills and Add (always visible, like a composer).
    fn new_task_bar(&mut self, ui: &mut Ui) {
        let enabled = !self.busy;
        let mut submit = false;
        egui::Frame::new()
            .fill(theme::FIELD)
            .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (g, _) =
                        ui.allocate_exact_size(egui::vec2(22.0, 26.0), egui::Sense::hover());
                    kit::icon(
                        ui.painter(),
                        g.center(),
                        ph::PLUS_CIRCLE,
                        18.0,
                        theme::MUTED,
                    );
                    let pills_w = 4.0 * 62.0 + 70.0;
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.task_title_draft)
                            .id(egui::Id::new("new_task_title"))
                            .frame(egui::Frame::NONE)
                            .font(theme::regular(15.0))
                            .text_color(theme::TEXT)
                            .hint_text(
                                egui::RichText::new("Add a task…")
                                    .font(theme::regular(15.0))
                                    .color(theme::MUTED),
                            )
                            .desired_width((ui.available_width() - pills_w).max(120.0)),
                    );
                    let ready = enabled && !self.task_title_draft.trim().is_empty();
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && ready {
                        submit = true;
                        edit.request_focus();
                    }
                    for p in [
                        TaskPriority::Low,
                        TaskPriority::Normal,
                        TaskPriority::High,
                        TaskPriority::Urgent,
                    ] {
                        let sel = self.task_priority_draft == p;
                        let r = priority_pill(ui, p, sel);
                        if r.clicked() {
                            self.task_priority_draft = p;
                        }
                    }
                    if kit::button_ex(ui, kit::Kind::Primary, None, "Add", 30.0, ready).clicked() {
                        submit = true;
                    }
                });
            });
        if submit {
            self.send(Command::CreateTask(TaskDraft {
                title: self.task_title_draft.trim().to_owned(),
                description: None,
                priority: self.task_priority_draft,
                due_at: None,
                related_users: Vec::new(),
                conversation_id: None,
                parent_id: None,
                source: None,
            }));
            if self.busy {
                self.task_title_draft.clear();
            }
        }
    }

    /// A11 week strip: this week's days; clicking filters by due day.
    fn week_strip(&mut self, ui: &mut Ui, tasks: &litecord_app::view::TasksViewModel) {
        let now = Timestamp::now();
        let today = local_day(now);
        // Monday of this week (1970-01-01 was a Thursday).
        let monday = today - (today + 3).rem_euclid(7);
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 64.0), egui::Sense::hover());
        ui.painter().rect(
            rect,
            12.0,
            theme::CARD,
            egui::Stroke::new(1.0_f32, theme::BORDER),
            egui::StrokeKind::Inside,
        );
        let w = rect.width() / 7.0;
        const NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        let mut pick = None;
        for (i, name) in NAMES.iter().enumerate() {
            let day = monday + i as i64;
            let cell = Rect::from_min_size(
                egui::pos2(rect.left() + w * i as f32, rect.top()),
                egui::vec2(w, rect.height()),
            );
            let resp = ui.interact(cell, ui.id().with(("week", i)), egui::Sense::click());
            let selected = self.task_day == Some(day);
            let is_today = day == today;
            let painter = ui.painter();
            if selected || is_today {
                let plate = cell.shrink(5.0);
                painter.rect(
                    plate,
                    9.0,
                    if selected {
                        theme::lerp(theme::CARD, theme::PRIMARY, 0.28)
                    } else {
                        theme::lerp(theme::CARD, theme::PRIMARY, 0.14)
                    },
                    egui::Stroke::new(
                        1.0_f32,
                        if selected {
                            theme::PRIMARY
                        } else {
                            theme::lerp(theme::BORDER, theme::PRIMARY, 0.35)
                        },
                    ),
                    egui::StrokeKind::Inside,
                );
            } else if resp.hovered() {
                painter.rect_filled(
                    cell.shrink(5.0),
                    9.0,
                    theme::lerp(theme::CARD, theme::HOVER, 0.6),
                );
            }
            if i > 0 && !(selected || is_today) {
                painter.line_segment(
                    [
                        cell.left_top() + egui::vec2(0.0, 14.0),
                        cell.left_bottom() - egui::vec2(0.0, 14.0),
                    ],
                    egui::Stroke::new(1.0_f32, theme::DIVIDER),
                );
            }
            painter.text(
                egui::pos2(cell.center().x, cell.top() + 20.0),
                Align2::CENTER_CENTER,
                *name,
                theme::regular(13.0),
                if is_today {
                    theme::PRIMARY_TEXT
                } else {
                    theme::SECONDARY
                },
            );
            painter.text(
                egui::pos2(cell.center().x, cell.top() + 42.0),
                Align2::CENTER_CENTER,
                day_of_month(day).to_string(),
                theme::semibold(17.0),
                theme::TEXT,
            );
            let due = tasks
                .open
                .iter()
                .filter(|t| t.due_at.is_some_and(|d| local_day(d) == day))
                .count();
            for k in 0..due.min(3) {
                painter.circle_filled(
                    egui::pos2(cell.center().x - 6.0 + k as f32 * 6.0, cell.bottom() - 9.0),
                    2.0,
                    theme::PRIMARY_TEXT,
                );
            }
            if resp.clicked() {
                pick = Some(day);
            }
        }
        if let Some(day) = pick {
            self.task_day = if self.task_day == Some(day) {
                None
            } else {
                Some(day)
            };
        }
    }

    /// A group of task rows under a colored heading (A11 priority groups).
    #[allow(clippy::too_many_arguments)]
    fn task_group(
        &mut self,
        ui: &mut Ui,
        title: &str,
        glyph: &str,
        color: Color32,
        rows: &[Task],
        candidate: bool,
        tasks: &litecord_app::view::TasksViewModel,
    ) {
        group_header(ui, title, glyph, color, rows.len());
        ui.spacing_mut().item_spacing.y = 6.0;
        for t in rows {
            let subtasks = tasks
                .open
                .iter()
                .filter(|x| x.parent_id == Some(t.id))
                .count();
            task_row(self, ui, t, candidate, subtasks);
        }
    }

    // ---- Inspector -------------------------------------------------------------

    /// Right panel: details of the selected task.
    pub fn task_inspector(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        let Some(selected) = self.selected_task else {
            kit::empty(
                ui,
                Some(ph::CHECK_SQUARE),
                "No task selected",
                "Select a task to see its details, subtasks and comments.",
            );
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
                kit::label(ui, "Loading task…", theme::regular(14.0), theme::MUTED);
            });
            return;
        };
        let detail = detail.clone();
        egui::Panel::bottom(egui::Id::new(("task_comment_bar", selected)))
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                left: 0,
                right: 0,
                top: 8,
                bottom: 0,
            }))
            .show_inside(ui, |ui| self.comment_bar(ui, &snapshot, &detail));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("task_inspector")
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.inspector_body(ui, &snapshot, &detail));
            });
    }

    fn inspector_body(
        &mut self,
        ui: &mut Ui,
        snapshot: &crate::bridge::Snapshot,
        detail: &TaskDetailViewModel,
    ) {
        let task = &detail.task;
        let enabled = !self.busy;
        ui.spacing_mut().item_spacing.y = 8.0;
        ui.horizontal(|ui| {
            let w = ui.available_width() - 34.0;
            ui.allocate_ui_with_layout(
                egui::vec2(w, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(w);
                    kit::para(
                        ui,
                        self.display(&task.title),
                        theme::semibold(21.0),
                        theme::TEXT,
                    );
                },
            );
            if kit::icon_button_ex(ui, ph::X, "Close", 28.0, theme::SECONDARY, true).clicked() {
                self.selected_task = None;
                self.selection.task = None;
                self.request();
            }
        });
        ui.horizontal_wrapped(|ui| {
            match task.priority {
                TaskPriority::Urgent | TaskPriority::High => {
                    kit::status_pill(
                        ui,
                        Some(ph::FLAG),
                        &format!("{} priority", priority_name(task.priority)),
                        theme::PRIORITY,
                    );
                }
                p => {
                    kit::status_pill(
                        ui,
                        Some(ph::FLAG),
                        &format!("{} priority", priority_name(p)),
                        priority_color(p),
                    );
                }
            };
            if task.status == TaskStatus::Candidate {
                kit::status_pill(ui, Some(ph::SPARKLE), "Suggested", theme::OMNI);
            }
        });
        if let Some(description) = task.description.as_deref().filter(|s| !s.is_empty()) {
            kit::para(
                ui,
                self.display(description),
                theme::regular(15.0),
                theme::lerp(theme::TEXT, theme::SECONDARY, 0.35),
            );
        }
        let conv = task.conversation_id.and_then(|c| {
            snapshot
                .conversations
                .conversations
                .iter()
                .find(|x| x.conversation_id == c)
                .map(|x| (c, self.display(&x.title)))
        });
        ui.horizontal_wrapped(|ui| {
            if let Some((_, name)) = &conv {
                kit::tag(ui, ph::CHAT_CIRCLE, name, kit::BLUE);
            }
            kit::tag(ui, ph::TAG, &origin_label(task), kit::PURPLE);
        });
        ui.add_space(4.0);
        kit::divider(ui);
        // Meta rows.
        let due_text = match task.due_at.filter(|d| d.as_millis() > 0) {
            Some(at) => {
                let (rel, _) = relative_due(at);
                format!(
                    "{} · {rel}",
                    crate::messages_ui::list_time(at, Timestamp::now())
                )
            }
            None => "No due date".into(),
        };
        meta_row(
            ui,
            ph::CALENDAR_BLANK,
            "Due date",
            &due_text,
            task.due_at
                .is_some_and(|d| d.as_millis() > 0 && d <= Timestamp::now()),
        );
        let people: Vec<String> = task
            .related_users
            .iter()
            .map(|u| {
                snapshot
                    .friends
                    .online
                    .iter()
                    .chain(&snapshot.friends.offline)
                    .find(|f| f.user_id == *u)
                    .map_or_else(|| "Someone".to_owned(), |f| self.display(&f.display_name))
            })
            .collect();
        {
            let (row, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 40.0), egui::Sense::hover());
            let painter = ui.painter();
            kit::icon(
                painter,
                row.left_center() + egui::vec2(12.0, 0.0),
                ph::USERS,
                18.0,
                theme::SECONDARY,
            );
            painter.text(
                row.left_center() + egui::vec2(38.0, 0.0),
                Align2::LEFT_CENTER,
                "People",
                theme::regular(14.5),
                theme::SECONDARY,
            );
            if people.is_empty() {
                painter.text(
                    egui::pos2(row.left() + 130.0, row.center().y),
                    Align2::LEFT_CENTER,
                    "Just you",
                    theme::regular(14.5),
                    theme::TEXT,
                );
            }
            for (i, p) in people.iter().take(4).enumerate() {
                theme::paint_avatar(
                    painter,
                    egui::pos2(row.left() + 142.0 + i as f32 * 24.0, row.center().y),
                    30.0,
                    p,
                    theme::Presence::None,
                    theme::INSPECTOR,
                );
            }
        }
        {
            let (row, resp) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 40.0), egui::Sense::click());
            let painter = ui.painter();
            kit::icon(
                painter,
                row.left_center() + egui::vec2(12.0, 0.0),
                ph::CHAT_CIRCLE_TEXT,
                18.0,
                theme::SECONDARY,
            );
            painter.text(
                row.left_center() + egui::vec2(38.0, 0.0),
                Align2::LEFT_CENTER,
                "From",
                theme::regular(14.5),
                theme::SECONDARY,
            );
            let text = conv.as_ref().map_or_else(
                || "Not linked to a conversation".to_owned(),
                |(_, n)| n.clone(),
            );
            kit::text_at(
                painter,
                egui::pos2(row.left() + 130.0, row.center().y),
                Align2::LEFT_CENTER,
                &text,
                theme::regular(14.5),
                if conv.is_some() {
                    theme::PRIMARY_TEXT
                } else {
                    theme::MUTED
                },
                row.width() - 150.0,
            );
            if conv.is_some() {
                kit::chevron(
                    painter,
                    row.right_center() - egui::vec2(8.0, 0.0),
                    theme::MUTED,
                );
            }
            if resp.clicked() {
                if let Some((c, _)) = conv {
                    self.open_conversation(c);
                }
            }
        }
        {
            let (row, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 40.0), egui::Sense::hover());
            let painter = ui.painter();
            kit::icon(
                painter,
                row.left_center() + egui::vec2(12.0, 0.0),
                ph::FLAG,
                18.0,
                theme::SECONDARY,
            );
            painter.text(
                row.left_center() + egui::vec2(38.0, 0.0),
                Align2::LEFT_CENTER,
                "Priority",
                theme::regular(14.5),
                theme::SECONDARY,
            );
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_max(
                        egui::pos2(row.left() + 124.0, row.top()),
                        row.max,
                    ))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            child.spacing_mut().item_spacing.x = 4.0;
            let can = enabled && task.status == TaskStatus::Open;
            for p in [
                TaskPriority::Low,
                TaskPriority::Normal,
                TaskPriority::High,
                TaskPriority::Urgent,
            ] {
                if priority_pill(&mut child, p, task.priority == p).clicked()
                    && can
                    && p != task.priority
                {
                    self.send(Command::SetTaskPriority(task.id, p));
                }
            }
        }
        meta_row(
            ui,
            ph::CHECK_CIRCLE,
            "Status",
            status_label(task.status),
            false,
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| match task.status {
            TaskStatus::Candidate => {
                if kit::button_ex(
                    ui,
                    kit::Kind::Primary,
                    Some(ph::CHECK),
                    "Confirm",
                    32.0,
                    enabled,
                )
                .clicked()
                {
                    self.send(Command::ConfirmTask(task.id));
                }
                if kit::button_ex(
                    ui,
                    kit::Kind::Secondary,
                    Some(ph::X),
                    "Dismiss",
                    32.0,
                    enabled,
                )
                .clicked()
                {
                    self.send(Command::DismissTask(task.id));
                }
            }
            TaskStatus::Open => {
                if kit::button_ex(
                    ui,
                    kit::Kind::Primary,
                    Some(ph::CHECK),
                    "Mark complete",
                    32.0,
                    enabled,
                )
                .clicked()
                {
                    self.send(Command::CompleteTask(task.id));
                }
            }
            _ => {}
        });
        ui.add_space(4.0);
        kit::divider(ui);
        // Subtasks are one level deep: only top-level tasks accept new ones.
        let done = detail
            .subtasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .count();
        kit::section(
            ui,
            &format!("Subtasks  {done}/{}", detail.subtasks.len()),
            None,
            None,
        );
        if detail.subtasks.is_empty() {
            kit::label(ui, "No subtasks.", theme::regular(13.5), theme::MUTED);
        }
        for sub in &detail.subtasks {
            ui.horizontal(|ui| {
                let checked = sub.status == TaskStatus::Done;
                let can_complete = enabled && sub.status == TaskStatus::Open;
                if kit::checkbox(ui, checked, can_complete)
                    .on_hover_text("Complete subtask")
                    .clicked()
                    && can_complete
                {
                    self.send(Command::CompleteTask(sub.id));
                }
                let title = self.display(&sub.title);
                let mut text = egui::RichText::new(&title).font(theme::regular(14.5));
                text = if checked {
                    text.strikethrough().color(theme::MUTED)
                } else {
                    text.color(theme::TEXT)
                };
                ui.add(egui::Label::new(text).truncate())
                    .on_hover_text(&title);
            });
        }
        if task.parent_id.is_none() && task.status == TaskStatus::Open {
            self.subtask_row(ui, task.id);
        }
        ui.add_space(4.0);
        kit::divider(ui);
        kit::section(
            ui,
            &format!("Comments  {}", detail.comments.len()),
            None,
            None,
        );
        if detail.comments.is_empty() {
            kit::label(ui, "No comments yet.", theme::regular(13.5), theme::MUTED);
        }
        for comment in &detail.comments {
            ui.horizontal_top(|ui| {
                let author = comment_author(comment.origin);
                let (a, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
                if comment.origin == Origin::AgentDerived {
                    kit::paint_bubble(ui.painter(), a.center(), ph::SPARKLE, kit::TEAL, 30.0);
                } else {
                    theme::paint_avatar(
                        ui.painter(),
                        a.center(),
                        30.0,
                        &author,
                        theme::Presence::None,
                        theme::INSPECTOR,
                    );
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.horizontal(|ui| {
                        kit::label(ui, author, theme::medium(13.5), theme::TEXT);
                        kit::label(
                            ui,
                            relative_ago(comment.created_at),
                            theme::regular(12.5),
                            theme::MUTED,
                        );
                    });
                    kit::para(
                        ui,
                        self.display(&comment.body),
                        theme::regular(14.0),
                        theme::lerp(theme::TEXT, theme::SECONDARY, 0.3),
                    );
                });
            });
        }
    }

    /// A11 bottom bar: avatar + comment field + send.
    fn comment_bar(
        &mut self,
        ui: &mut Ui,
        snapshot: &crate::bridge::Snapshot,
        detail: &TaskDetailViewModel,
    ) {
        let enabled = !self.busy;
        let mut submit = false;
        ui.horizontal(|ui| {
            let (a, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
            theme::paint_avatar(
                ui.painter(),
                a.center(),
                36.0,
                &self.display(&snapshot.account.display_name),
                theme::Presence::None,
                theme::INSPECTOR,
            );
            egui::Frame::new()
                .fill(theme::FIELD)
                .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                .corner_radius(10)
                .inner_margin(egui::Margin::symmetric(10, 4))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut self.task_comment_draft)
                                .frame(egui::Frame::NONE)
                                .font(theme::regular(14.0))
                                .text_color(theme::TEXT)
                                .hint_text(
                                    egui::RichText::new("Add a comment...")
                                        .font(theme::regular(14.0))
                                        .color(theme::MUTED),
                                )
                                .desired_width((ui.available_width() - 40.0).max(60.0)),
                        );
                        let ready = enabled && !self.task_comment_draft.trim().is_empty();
                        if edit.lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            && ready
                        {
                            submit = true;
                            edit.request_focus();
                        }
                        if kit::icon_button_ex(
                            ui,
                            ph::PAPER_PLANE_RIGHT,
                            "Post comment",
                            30.0,
                            if ready {
                                theme::PRIMARY_TEXT
                            } else {
                                theme::MUTED
                            },
                            ready,
                        )
                        .clicked()
                        {
                            submit = true;
                        }
                    });
                });
        });
        if submit {
            let body = self.task_comment_draft.trim().to_owned();
            self.send(Command::AddTaskComment(detail.task.id, body));
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

    /// "Add subtask" row under a task's subtasks.
    fn subtask_row(&mut self, ui: &mut Ui, parent: TaskId) {
        let enabled = !self.busy;
        let mut submit = false;
        // Subtask drafts live in egui temp memory; the workspace only owns
        // the top-level title/priority drafts.
        let sub_id = egui::Id::new(("task_subtask_draft", parent));
        let mut sub_draft = ui.data_mut(|d| d.get_temp::<String>(sub_id).unwrap_or_default());
        ui.horizontal(|ui| {
            let (g, _) = ui.allocate_exact_size(egui::vec2(22.0, 26.0), egui::Sense::hover());
            kit::icon(ui.painter(), g.center(), ph::PLUS, 15.0, theme::MUTED);
            let edit = ui.add(
                egui::TextEdit::singleline(&mut sub_draft)
                    .frame(egui::Frame::NONE)
                    .font(theme::regular(14.0))
                    .text_color(theme::TEXT)
                    .hint_text(
                        egui::RichText::new("Add subtask")
                            .font(theme::regular(14.0))
                            .color(theme::MUTED),
                    )
                    .desired_width((ui.available_width() - 64.0).max(80.0)),
            );
            let ready = enabled && !sub_draft.trim().is_empty();
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && ready {
                submit = true;
                edit.request_focus();
            }
            if kit::button_ex(ui, kit::Kind::Secondary, None, "Add", 28.0, ready).clicked() {
                submit = true;
            }
        });
        if submit {
            self.send(Command::CreateTask(TaskDraft {
                title: sub_draft.trim().to_owned(),
                description: None,
                priority: TaskPriority::Normal,
                due_at: None,
                related_users: Vec::new(),
                conversation_id: None,
                parent_id: Some(parent),
                source: None,
            }));
            if self.busy {
                sub_draft.clear();
            }
        }
        ui.data_mut(|d| d.insert_temp(sub_id, sub_draft));
    }
}

fn group_header(ui: &mut Ui, title: &str, glyph: &str, color: Color32, n: usize) {
    let (h, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::hover());
    let painter = ui.painter();
    let c = h.left_center() + egui::vec2(16.0, 0.0);
    painter.circle_filled(c, 16.0, theme::lerp(theme::WORKSPACE, color, 0.2));
    kit::icon(painter, c, glyph, 19.0, color);
    let t = kit::text_at(
        painter,
        h.left_center() + egui::vec2(42.0, 0.0),
        Align2::LEFT_CENTER,
        title,
        theme::semibold(16.5),
        theme::TEXT,
        h.width() - 80.0,
    );
    let badge = Rect::from_center_size(
        egui::pos2(t.right() + 20.0, h.center().y),
        egui::vec2(26.0, 22.0),
    );
    painter.rect_filled(badge, 11.0, theme::RAISED);
    painter.text(
        badge.center(),
        Align2::CENTER_CENTER,
        n.to_string(),
        theme::medium(13.0),
        theme::SECONDARY,
    );
}

/// One A11 task row: checkbox, title + priority pill, description, tags,
/// due column, people, and a menu — all fitted to the row width.
fn task_row(workspace: &mut Workspace, ui: &mut Ui, task: &Task, candidate: bool, subtasks: usize) {
    let selected = workspace.selected_task == Some(task.id);
    let enabled = !workspace.busy;
    let title = workspace.display(&task.title);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 66.0), egui::Sense::click());
    let high = task.priority >= TaskPriority::High;
    let painter = ui.painter();
    if selected && high {
        painter.rect(
            rect,
            12.0,
            theme::lerp(theme::CARD, theme::PRIORITY, 0.1),
            egui::Stroke::new(1.0_f32, theme::lerp(theme::BORDER, theme::PRIORITY, 0.55)),
            egui::StrokeKind::Inside,
        );
        painter.rect_filled(
            Rect::from_min_size(
                rect.min + egui::vec2(0.0, 6.0),
                egui::vec2(3.0, rect.height() - 12.0),
            ),
            1.5,
            theme::PRIORITY,
        );
    } else if selected {
        painter.rect(
            rect,
            12.0,
            theme::lerp(theme::CARD, theme::PRIMARY, 0.1),
            egui::Stroke::new(1.0_f32, theme::lerp(theme::BORDER, theme::PRIMARY, 0.55)),
            egui::StrokeKind::Inside,
        );
    } else {
        kit::paint_card(painter, rect, resp.hovered());
    }
    // Leading checkbox (or a sparkle for suggestions).
    let cb = Rect::from_center_size(
        egui::pos2(rect.left() + 26.0, rect.center().y),
        egui::vec2(22.0, 22.0),
    );
    if candidate {
        kit::paint_bubble(ui.painter(), cb.center(), ph::SPARKLE, kit::TEAL, 30.0);
    } else {
        let r = ui.interact(
            cb,
            ui.id().with(("task_check", task.id)),
            egui::Sense::click(),
        );
        ui.painter().rect_stroke(
            cb.shrink(1.0),
            5.0,
            egui::Stroke::new(
                1.6_f32,
                if r.hovered() {
                    theme::SECONDARY
                } else {
                    theme::MUTED
                },
            ),
            egui::StrokeKind::Inside,
        );
        if r.hovered() {
            kit::icon(ui.painter(), cb.center(), ph::CHECK, 13.0, theme::MUTED);
        }
        if r.on_hover_text("Complete task").clicked() && enabled {
            workspace.send(Command::CompleteTask(task.id));
        }
    }
    // Right side, from the edge inward.
    let mut right = rect.right() - 10.0;
    let menu = Rect::from_center_size(
        egui::pos2(right - 14.0, rect.center().y),
        egui::vec2(28.0, 28.0),
    );
    right = menu.left() - 6.0;
    let mut buttons = Vec::new();
    if candidate {
        for (label, kind, confirm) in [
            ("Dismiss", kit::Kind::Ghost, false),
            ("Confirm", kit::Kind::Primary, true),
        ] {
            let w = kit::button_width(ui.painter(), None, label, 28.0);
            buttons.push((
                Rect::from_min_size(
                    egui::pos2(right - w, rect.center().y - 14.0),
                    egui::vec2(w, 28.0),
                ),
                label,
                kind,
                confirm,
            ));
            right -= w + 6.0;
        }
    }
    // Due column.
    if let Some(at) = task.due_at.filter(|d| d.as_millis() > 0) {
        let now = Timestamp::now();
        let urgent = at <= now || local_day(at) == local_day(now);
        let color = if urgent {
            theme::PRIORITY_TEXT
        } else {
            theme::SECONDARY
        };
        let day = if local_day(at) == local_day(now) {
            "Today".to_owned()
        } else if local_day(at) == local_day(now) + 1 {
            "Tomorrow".to_owned()
        } else {
            crate::messages_ui::list_time(at, now)
        };
        let clock = crate::messages_ui::clock(at);
        let w = kit::text_width(ui.painter(), &day, theme::regular(13.5)).max(kit::text_width(
            ui.painter(),
            &clock,
            theme::regular(13.0),
        )) + 26.0;
        let x = right - w;
        let painter = ui.painter();
        kit::icon(
            painter,
            egui::pos2(x + 8.0, rect.center().y - 9.0),
            ph::CALENDAR_BLANK,
            15.0,
            color,
        );
        painter.text(
            egui::pos2(x + 22.0, rect.center().y - 9.0),
            Align2::LEFT_CENTER,
            day,
            theme::regular(13.5),
            color,
        );
        painter.text(
            egui::pos2(x + 22.0, rect.center().y + 10.0),
            Align2::LEFT_CENTER,
            clock,
            theme::regular(13.0),
            color,
        );
        right = x - 12.0;
    }
    // Tags: origin (and subtasks).
    let origin = origin_label(task);
    let tag_font = theme::regular(12.5);
    let tag_w = kit::text_width(ui.painter(), &origin, tag_font.clone()) + 34.0;
    let x0 = rect.left() + 50.0;
    let show_tag = right - tag_w - x0 > 180.0;
    if show_tag {
        let tr = Rect::from_min_size(
            egui::pos2(right - tag_w, rect.center().y - 13.0),
            egui::vec2(tag_w, 26.0),
        );
        let painter = ui.painter();
        painter.rect(
            tr,
            7.0,
            theme::lerp(theme::CARD, theme::RAISED, 0.7),
            egui::Stroke::new(1.0_f32, theme::BORDER),
            egui::StrokeKind::Inside,
        );
        kit::icon(
            painter,
            tr.left_center() + egui::vec2(13.0, 0.0),
            if task.origin == Origin::AgentDerived {
                ph::SPARKLE
            } else {
                ph::TAG
            },
            13.0,
            if task.origin == Origin::AgentDerived {
                theme::OMNI
            } else {
                kit::PURPLE.fg
            },
        );
        painter.text(
            tr.left_center() + egui::vec2(26.0, 0.0),
            Align2::LEFT_CENTER,
            &origin,
            tag_font,
            theme::lerp(theme::TEXT, theme::SECONDARY, 0.3),
        );
        right = tr.left() - 10.0;
    }
    // Title (+ priority pill) and description.
    let painter = ui.painter();
    let pill_w = if high { 64.0 } else { 0.0 };
    let tw = (right - x0 - pill_w - 8.0).max(40.0);
    let desc = task
        .description
        .as_deref()
        .filter(|d| !d.is_empty())
        .map(|d| workspace.display(d))
        .unwrap_or_else(|| {
            if subtasks > 0 {
                format!("{subtasks} subtask{}", if subtasks == 1 { "" } else { "s" })
            } else if show_tag {
                String::new()
            } else {
                origin.clone()
            }
        });
    let title_y = if desc.is_empty() {
        rect.center().y
    } else {
        rect.center().y - 10.0
    };
    let tr = kit::text_at(
        painter,
        egui::pos2(x0, title_y),
        Align2::LEFT_CENTER,
        &title,
        theme::medium(15.0),
        theme::TEXT,
        tw,
    );
    if high {
        let pr = Rect::from_min_size(
            egui::pos2(tr.right() + 10.0, title_y - 12.0),
            egui::vec2(58.0, 24.0),
        );
        kit::paint_status_pill(
            painter,
            pr,
            Some(ph::FLAG),
            priority_name(task.priority),
            theme::PRIORITY,
        );
    }
    if !desc.is_empty() {
        kit::text_at(
            painter,
            egui::pos2(x0, rect.center().y + 12.0),
            Align2::LEFT_CENTER,
            &desc,
            theme::regular(13.5),
            theme::MUTED,
            right - x0,
        );
    }
    // Menu and suggestion buttons.
    let menu_resp = kit::icon_button_at(
        ui,
        menu,
        ui.id().with(("task_menu", task.id)),
        ph::DOTS_THREE,
        "More",
        theme::SECONDARY,
    );
    egui::Popup::menu(&menu_resp).show(|ui| {
        ui.set_min_width(180.0);
        if !candidate && ui.button("Mark complete").clicked() {
            workspace.send(Command::CompleteTask(task.id));
        }
        for p in [
            TaskPriority::Urgent,
            TaskPriority::High,
            TaskPriority::Normal,
            TaskPriority::Low,
        ] {
            if p != task.priority
                && !candidate
                && ui
                    .button(format!("Set {} priority", priority_name(p).to_lowercase()))
                    .clicked()
            {
                workspace.send(Command::SetTaskPriority(task.id, p));
            }
        }
        if let Some(c) = task.conversation_id {
            if ui.button("Open conversation").clicked() {
                workspace.open_conversation(c);
            }
        }
        if candidate && ui.button("Dismiss suggestion").clicked() {
            workspace.send(Command::DismissTask(task.id));
        }
    });
    for (k, (r, label, kind, confirm)) in buttons.into_iter().enumerate() {
        if kit::button_at(
            ui,
            r,
            ui.id().with(("task_btn", task.id, k)),
            kind,
            None,
            label,
            enabled,
        )
        .clicked()
        {
            workspace.send(if confirm {
                Command::ConfirmTask(task.id)
            } else {
                Command::DismissTask(task.id)
            });
        }
    }
    let label = format!("{title}, {} priority", priority_name(task.priority));
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label)
    });
    if resp.clicked() {
        workspace.select_task(task.id);
    }
}

fn reminder_row(workspace: &mut Workspace, ui: &mut Ui, reminder: &Reminder) {
    let selected = reminder.task_id.is_some() && reminder.task_id == workspace.selected_task;
    let title = workspace.display(&reminder.title);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 58.0), egui::Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect(
            rect,
            12.0,
            theme::lerp(theme::CARD, theme::PRIMARY, 0.1),
            egui::Stroke::new(1.0_f32, theme::PRIMARY),
            egui::StrokeKind::Inside,
        );
    } else {
        kit::paint_card(painter, rect, resp.hovered());
    }
    kit::paint_bubble(
        painter,
        egui::pos2(rect.left() + 28.0, rect.center().y),
        ph::BELL,
        kit::YELLOW,
        34.0,
    );
    let (text, overdue) = relative_due(reminder.trigger.due_at());
    let text = if overdue {
        text.replace("Overdue", "Was due")
    } else {
        text
    };
    let conditional = matches!(
        reminder.trigger,
        litecord_types::tasks::ReminderTrigger::Conditional { .. }
    );
    let meta = if conditional {
        format!("{text} · if no reply")
    } else {
        text
    };
    let mw = kit::text_width(painter, &meta, theme::regular(13.0));
    painter.text(
        rect.right_center() - egui::vec2(14.0, 0.0),
        Align2::RIGHT_CENTER,
        &meta,
        theme::regular(13.0),
        if overdue {
            theme::PRIORITY_TEXT
        } else {
            theme::SECONDARY
        },
    );
    let x = rect.left() + 56.0;
    kit::text_at(
        painter,
        egui::pos2(x, rect.center().y - 9.0),
        Align2::LEFT_CENTER,
        &title,
        theme::medium(15.0),
        theme::TEXT,
        rect.right() - x - mw - 28.0,
    );
    kit::text_at(
        painter,
        egui::pos2(x, rect.center().y + 11.0),
        Align2::LEFT_CENTER,
        &pretty_origin(reminder.origin),
        theme::regular(13.0),
        theme::MUTED,
        rect.right() - x - mw - 28.0,
    );
    if let Some(id) = reminder.task_id {
        if resp.clicked() {
            workspace.select_task(id);
        }
    }
}

fn meta_row(ui: &mut Ui, glyph: &str, key: &str, value: &str, alert: bool) {
    let (row, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), egui::Sense::hover());
    let painter = ui.painter();
    kit::icon(
        painter,
        row.left_center() + egui::vec2(12.0, 0.0),
        glyph,
        18.0,
        theme::SECONDARY,
    );
    painter.text(
        row.left_center() + egui::vec2(38.0, 0.0),
        Align2::LEFT_CENTER,
        key,
        theme::regular(14.5),
        theme::SECONDARY,
    );
    kit::text_at(
        painter,
        egui::pos2(row.left() + 130.0, row.center().y),
        Align2::LEFT_CENTER,
        value,
        theme::regular(14.5),
        if alert {
            theme::PRIORITY_TEXT
        } else {
            theme::TEXT
        },
        row.width() - 134.0,
    );
}

/// Small selectable priority pill.
fn priority_pill(ui: &mut Ui, p: TaskPriority, selected: bool) -> egui::Response {
    let label = priority_name(p);
    let w = kit::text_width(ui.painter(), label, theme::medium(12.5)) + 20.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 26.0), egui::Sense::click());
    let color = priority_color(p);
    let painter = ui.painter();
    if selected {
        painter.rect(
            rect,
            7.0,
            theme::lerp(theme::CARD, color, 0.22),
            egui::Stroke::new(1.0_f32, theme::lerp(theme::CARD, color, 0.6)),
            egui::StrokeKind::Inside,
        );
    } else if resp.hovered() {
        painter.rect_filled(rect, 7.0, theme::HOVER);
    }
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        theme::medium(12.5),
        if selected {
            theme::TEXT
        } else {
            theme::SECONDARY
        },
    );
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    resp
}

/// Local calendar day number.
fn local_day(t: Timestamp) -> i64 {
    (t.as_millis() + crate::messages_ui::local_offset_ms(t)).div_euclid(DAY_MS)
}

fn day_of_month(day: i64) -> i64 {
    let label = crate::messages_ui::day_label(Timestamp::from_millis(
        day * DAY_MS - crate::messages_ui::local_offset_ms(Timestamp::from_millis(day * DAY_MS)),
    ));
    label
        .split(' ')
        .nth(1)
        .and_then(|d| d.parse().ok())
        .unwrap_or(0)
}

fn due_today_or_overdue(t: &Task) -> bool {
    t.due_at
        .filter(|d| d.as_millis() > 0)
        .is_some_and(|d| local_day(d) <= local_day(Timestamp::now()))
}

fn tab_matches(tab: usize, t: &Task) -> bool {
    let due = t.due_at.filter(|d| d.as_millis() > 0);
    match tab {
        1 => due.is_some_and(|d| local_day(d) <= local_day(Timestamp::now())),
        2 => due.is_some_and(|d| local_day(d) > local_day(Timestamp::now())),
        3 => due.is_none(),
        _ => true,
    }
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

fn priority_name(priority: TaskPriority) -> &'static str {
    match priority {
        TaskPriority::Low => "Low",
        TaskPriority::Normal => "Normal",
        TaskPriority::High => "High",
        TaskPriority::Urgent => "Urgent",
    }
}

fn priority_color(priority: TaskPriority) -> Color32 {
    match priority {
        TaskPriority::Urgent => theme::PRIORITY,
        TaskPriority::High => kit::ORANGE.fg,
        TaskPriority::Normal => theme::PRIMARY,
        TaskPriority::Low => theme::MUTED,
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

    #[test]
    fn tabs_split_tasks_by_due_day() {
        let now = Timestamp::now().0;
        assert!(tab_matches(
            1,
            &task(1, TaskPriority::Normal, Some(now - DAY_MS))
        ));
        assert!(tab_matches(
            2,
            &task(2, TaskPriority::Normal, Some(now + 3 * DAY_MS))
        ));
        assert!(tab_matches(3, &task(3, TaskPriority::Normal, None)));
        assert!(!tab_matches(3, &task(4, TaskPriority::Normal, Some(now))));
    }
}
