//! Memory: what Litecord remembers, where each item came from and how sure
//! it is. Candidates (automatic extractions, Omni suggestions) wait for the
//! user's confirmation; confirmed items feed Omni's context.

use eframe::egui::{self, Align2, FontId, RichText, Sense, Stroke, Ui};
use litecord_types::entity::{EntityId, LocalEntityKind};
use litecord_types::memory::{MemoryItem, MemoryKind, MemoryStatus};
use litecord_types::provenance::Origin;
use litecord_types::Timestamp;

use crate::bridge::Command;
use crate::context_ui::{kind_label, relative};
use crate::theme;
use crate::workspace::Workspace;

const ROW: f32 = 58.0;

impl Workspace {
    pub(crate) fn memory_screen_v2(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        theme::page_header(
            ui,
            "Memory",
            Some("What Litecord remembers, where it came from, and how sure it is."),
        );
        let needle = self.memory_search.trim().to_lowercase();
        let items: Vec<&MemoryItem> = s
            .memory
            .memories
            .iter()
            .filter(|m| self.memory_kind_filter.is_none_or(|k| m.kind == k))
            .filter(|m| self.memory_status_filter.is_none_or(|st| m.status == st))
            .filter(|m| needle.is_empty() || m.content.to_lowercase().contains(&needle))
            .collect();
        if s.memory.memories.is_empty() {
            theme::empty_state(
                ui,
                "Nothing remembered yet",
                "Commitments, dates and replies you owe are picked up from your messages as they \
                 sync. You can also ask Omni to remember something from a chat.",
            );
            return;
        }
        if items.is_empty() {
            theme::empty_state(ui, "No matches", "Try another filter or clear the search.");
            return;
        }
        let (review, rest): (Vec<&MemoryItem>, Vec<&MemoryItem>) = items
            .into_iter()
            .partition(|m| m.status == MemoryStatus::Candidate);
        egui::ScrollArea::vertical()
            .id_salt("memory_rows")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !review.is_empty() {
                    ui.horizontal(|ui| {
                        theme::section_label(ui, &format!("To review · {}", review.len()));
                        ui.label(theme::meta("Confirm what's right; reject the rest."));
                    });
                    for m in review {
                        self.memory_row(ui, m);
                    }
                    ui.add_space(10.0);
                }
                if !rest.is_empty() {
                    theme::section_label(ui, &format!("Remembered · {}", rest.len()));
                    for m in rest {
                        self.memory_row(ui, m);
                    }
                }
            });
    }

    fn memory_row(&mut self, ui: &mut Ui, m: &MemoryItem) {
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(egui::vec2(width, ROW), Sense::click());
        let selected = self.selected_memory == Some(m.id);
        let candidate = m.status == MemoryStatus::Candidate;
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let fill = if selected {
                theme::SELECTED
            } else if response.hovered() {
                theme::HOVER
            } else {
                theme::WORKSPACE
            };
            painter.rect(
                rect.shrink2(egui::vec2(0.0, 2.0)),
                6.0,
                fill,
                Stroke::NONE,
                egui::StrokeKind::Inside,
            );
            let color = kind_color(m.kind);
            painter.rect_filled(
                egui::Rect::from_min_size(
                    rect.min + egui::vec2(0.0, 8.0),
                    egui::vec2(3.0, ROW - 16.0),
                ),
                2.0,
                color,
            );
            // Kind label, then content (elided), then provenance line.
            let right_reserve = if candidate { 170.0 } else { 90.0 };
            let text_w = (width - 24.0 - right_reserve).max(60.0);
            painter.text(
                rect.min + egui::vec2(14.0, 8.0),
                Align2::LEFT_TOP,
                kind_label(m.kind).to_uppercase(),
                FontId::proportional(10.5),
                color,
            );
            let mut job = egui::text::LayoutJob::simple_singleline(
                self.display(&m.content),
                FontId::proportional(14.0),
                theme::TEXT,
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(text_w);
            painter.galley(
                rect.min + egui::vec2(14.0, 21.0),
                painter.layout_job(job),
                theme::TEXT,
            );
            let when = m.observed_at.unwrap_or(m.created_at);
            painter.text(
                rect.min + egui::vec2(14.0, 40.0),
                Align2::LEFT_TOP,
                format!(
                    "{} · {} · {}",
                    origin_label(m.origin),
                    relative(when.as_millis() - Timestamp::now().as_millis()),
                    source_label(m)
                ),
                FontId::proportional(11.5),
                theme::MUTED,
            );
            confidence_meter(
                ui,
                rect.right_top() + egui::vec2(-right_reserve + 8.0, 12.0),
                m.confidence.get(),
            );
        }
        if response.clicked() {
            self.selected_memory = Some(m.id);
        }
        let label = format!("{}: {}", kind_label(m.kind), self.display(&m.content));
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label)
        });
        if candidate {
            let buttons = egui::Rect::from_min_size(
                egui::pos2(rect.right() - 162.0, rect.center().y - 14.0),
                egui::vec2(156.0, 28.0),
            );
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(buttons)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            if child
                .add_enabled(
                    !self.busy,
                    egui::Button::new(RichText::new("Confirm").color(theme::SUCCESS)),
                )
                .clicked()
            {
                self.send(Command::ConfirmMemory(m.id));
            }
            if child
                .add_enabled(!self.busy, egui::Button::new("Reject"))
                .clicked()
            {
                self.send(Command::RejectMemory(m.id));
            }
        }
    }

    /// Memory inspector (right panel).
    pub(crate) fn memory_inspector(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        let Some(m) = s
            .memory
            .memories
            .iter()
            .find(|m| Some(m.id) == self.selected_memory)
        else {
            theme::empty_state(
                ui,
                "No memory selected",
                "Select a record to see where it came from.",
            );
            return;
        };
        ui.label(
            RichText::new(kind_label(m.kind))
                .size(12.0)
                .color(kind_color(m.kind)),
        );
        ui.label(RichText::new(self.display(&m.content)).size(15.0));
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            theme::chip(ui, status_label(m.status), status_color(m.status));
            theme::chip(ui, origin_label(m.origin), theme::MUTED);
            theme::chip(
                ui,
                &format!("{:.0}% sure", m.confidence.get() * 100.0),
                theme::OMNI,
            );
        });
        if m.status == MemoryStatus::Candidate {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Confirm"))
                    .clicked()
                {
                    self.send(Command::ConfirmMemory(m.id));
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Reject"))
                    .clicked()
                {
                    self.send(Command::RejectMemory(m.id));
                }
            });
        }
        ui.add_space(8.0);
        ui.separator();
        theme::section_label(ui, "Where it came from");
        if m.source_refs.is_empty() {
            ui.label(theme::meta("No source recorded."));
        }
        for src in &m.source_refs {
            let (text, action) = self.describe_entity(&s, &src.entity);
            ui.horizontal(|ui| {
                ui.label(self.display(&text));
                if let Some(action) = action {
                    if ui.small_button("Open").clicked() {
                        action(self);
                    }
                }
            });
            if let Some(note) = &src.note {
                ui.label(theme::meta(self.display(note)));
            }
        }
        if !m.entities.is_empty() {
            ui.add_space(6.0);
            theme::section_label(ui, "About");
            for e in &m.entities {
                let (text, action) = self.describe_entity(&s, e);
                ui.horizontal(|ui| {
                    ui.label(self.display(&text));
                    if let Some(action) = action {
                        if ui.small_button("Open").clicked() {
                            action(self);
                        }
                    }
                });
            }
        }
        ui.add_space(6.0);
        ui.separator();
        ui.label(theme::meta(format!(
            "Noticed {} · {}",
            relative(
                m.observed_at.unwrap_or(m.created_at).as_millis() - Timestamp::now().as_millis()
            ),
            if m.origin == Origin::AgentDerived {
                "suggested by an agent; Omni uses it only after you confirm or when asked"
            } else {
                "used as context for Omni and search"
            }
        )));
        if let Some(id) = m.superseded_by {
            ui.label(theme::meta(format!("Replaced by a newer record (#{id}).")));
        }
    }

    /// Human description of an entity plus an optional navigation action.
    #[allow(clippy::type_complexity)]
    fn describe_entity(
        &self,
        s: &crate::bridge::Snapshot,
        e: &EntityId,
    ) -> (String, Option<Box<dyn Fn(&mut Workspace)>>) {
        match *e {
            EntityId::Conversation(id) => {
                let title = s
                    .conversations
                    .conversations
                    .iter()
                    .find(|c| c.conversation_id == id)
                    .map_or_else(|| "a conversation".to_owned(), |c| c.title.clone());
                (
                    format!("Conversation with {title}"),
                    Some(Box::new(move |w: &mut Workspace| {
                        w.navigate(litecord_layout::Destination::Messages);
                        w.open_conversation(id);
                    })),
                )
            }
            EntityId::Message(_) => ("A message".into(), None),
            EntityId::User(id) => {
                let name = s
                    .friends
                    .online
                    .iter()
                    .chain(&s.friends.offline)
                    .find(|f| f.user_id == id)
                    .map_or_else(|| "someone".to_owned(), |f| f.display_name.clone());
                (format!("Person: {name}"), None)
            }
            EntityId::Task(_) => ("A task".into(), None),
            EntityId::Local(LocalEntityKind::OmniSession, sid) => (
                "An Omni chat".into(),
                Some(Box::new(move |w: &mut Workspace| {
                    w.selection.omni_session = Some(sid.0);
                    w.omni_open = true;
                    w.request();
                })),
            ),
            other => (other.kind_str().replace('_', " "), None),
        }
    }
}

fn confidence_meter(ui: &Ui, at: egui::Pos2, value: f32) {
    let painter = ui.painter();
    let bar = egui::Rect::from_min_size(at + egui::vec2(0.0, 14.0), egui::vec2(60.0, 4.0));
    painter.rect_filled(bar, 2.0, theme::BORDER);
    let mut fill = bar;
    fill.set_width(bar.width() * value.clamp(0.0, 1.0));
    painter.rect_filled(fill, 2.0, theme::OMNI);
    painter.text(
        at,
        Align2::LEFT_TOP,
        format!("{:.0}%", value * 100.0),
        FontId::proportional(11.0),
        theme::MUTED,
    );
}

fn kind_color(kind: MemoryKind) -> egui::Color32 {
    match kind {
        MemoryKind::Commitment => theme::WARNING,
        MemoryKind::PendingReply => theme::PRIMARY_TEXT,
        MemoryKind::ImportantDate => theme::PRIORITY,
        MemoryKind::Observation | MemoryKind::Summary | MemoryKind::Operational => theme::OMNI,
        MemoryKind::Fact | MemoryKind::Preference => theme::SUCCESS,
        MemoryKind::Note => theme::SECONDARY,
    }
}

pub(crate) fn origin_label(origin: Origin) -> &'static str {
    match origin {
        Origin::UserProvided => "You",
        Origin::AgentDerived => "From Omni",
        Origin::LocalApplication => "From your messages",
        Origin::DiscordSocialSdk | Origin::DiscordBotGateway => "From Discord",
        Origin::Imported => "Imported",
        Origin::Synthetic => "Demo data",
    }
}

fn status_label(status: MemoryStatus) -> &'static str {
    match status {
        MemoryStatus::Candidate => "Needs review",
        MemoryStatus::Derived => "Derived",
        MemoryStatus::UserConfirmed => "Confirmed",
        MemoryStatus::Superseded => "Replaced",
        MemoryStatus::Rejected => "Rejected",
        MemoryStatus::Expired => "Expired",
    }
}

fn status_color(status: MemoryStatus) -> egui::Color32 {
    match status {
        MemoryStatus::Candidate => theme::WARNING,
        MemoryStatus::UserConfirmed | MemoryStatus::Derived => theme::SUCCESS,
        _ => theme::MUTED,
    }
}

fn source_label(m: &MemoryItem) -> String {
    match m.source_refs.len() {
        0 => "no source".into(),
        1 => "1 source".into(),
        n => format!("{n} sources"),
    }
}
