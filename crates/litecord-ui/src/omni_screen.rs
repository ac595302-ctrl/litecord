//! Omni (mock A10, "Memory" renamed): the assistant and the unified memory
//! it works from. Sidebar: chats, people and collections. Main: an ask bar,
//! a memory graph of the people and conversations memories are about,
//! memory cards, the source timeline (confirm/reject candidates), what Omni
//! may read, and assistance cards. Inspector: the selected memory, or
//! Omni's status and sign-in.

use std::collections::BTreeMap;

use eframe::egui::{self, Align2, Color32, Rect, Stroke, Ui};
use litecord_app::harness::LoginState;
use litecord_layout::Destination;
use litecord_types::entity::{EntityId, LocalEntityKind};
use litecord_types::memory::{MemoryItem, MemoryKind, MemoryStatus};
use litecord_types::provenance::Origin;
use litecord_types::trust::AgentVisibility;
use litecord_types::Timestamp;

use crate::bridge::{Command, OmniCommand, Snapshot};
use crate::context_ui::{kind_label, relative};
use crate::messages_ui::list_time;
use crate::workspace::Workspace;
use crate::{kit, ph, theme};

/// A person or conversation that memories are about.
#[derive(Clone)]
struct Node {
    entity: EntityId,
    name: String,
    count: usize,
    kind: MemoryKind,
    person: bool,
}

impl Workspace {
    // ---- Sidebar -------------------------------------------------------------

    pub(crate) fn omni_sidebar(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        if kit::sidebar_title(ui, "Omni", Some((ph::NOTE_PENCIL, "New Omni chat"))) {
            self.selection.omni_session = None;
            self.omni_open = true;
            self.request();
        }
        ui.add_space(6.0);
        kit::search(ui, &mut self.memory_search, "Search memories...");
        ui.add_space(10.0);
        let mut status = match self.memory_status_filter {
            None => 0,
            Some(MemoryStatus::Candidate) => 1,
            Some(_) => 2,
        };
        let review = crate::home_ui::candidates(&s);
        if kit::pill_row(
            ui,
            &[
                ("All", None),
                ("To review", Some(review)),
                ("Confirmed", None),
            ],
            &mut status,
        ) {
            self.memory_status_filter = match status {
                1 => Some(MemoryStatus::Candidate),
                2 => Some(MemoryStatus::UserConfirmed),
                _ => None,
            };
        }
        egui::ScrollArea::vertical()
            .id_salt("omni_sidebar")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                // Chats.
                let chats: Vec<_> = s
                    .omni
                    .sessions
                    .iter()
                    .filter(|x| x.kind == "chat")
                    .take(5)
                    .cloned()
                    .collect();
                heading(ui, "Chats", Some(chats.len()));
                if chats.is_empty() {
                    kit::label(
                        ui,
                        "No chats yet. Ask Omni anything.",
                        theme::regular(13.5),
                        theme::MUTED,
                    );
                }
                let now = Timestamp::now();
                for c in &chats {
                    let selected = self.selection.omni_session == Some(c.id) && self.omni_open;
                    let (rect, resp) = kit::row(ui, 54.0, selected);
                    let painter = ui.painter();
                    kit::paint_bubble(
                        painter,
                        egui::pos2(rect.left() + 26.0, rect.center().y),
                        ph::SPARKLE,
                        kit::TEAL,
                        36.0,
                    );
                    let x = rect.left() + 54.0;
                    let t = list_time(c.last_active_at, now);
                    let tw = kit::text_width(painter, &t, theme::regular(12.5));
                    let title = if c.title.trim().is_empty() {
                        "Untitled chat".to_owned()
                    } else {
                        self.display(&c.title)
                    };
                    kit::text_at(
                        painter,
                        egui::pos2(x, rect.center().y - 9.0),
                        Align2::LEFT_CENTER,
                        &title,
                        theme::medium(14.5),
                        theme::TEXT,
                        rect.right() - x - tw - 16.0,
                    );
                    painter.text(
                        egui::pos2(rect.right() - 8.0, rect.center().y - 9.0),
                        Align2::RIGHT_CENTER,
                        t,
                        theme::regular(12.5),
                        theme::MUTED,
                    );
                    let sub = format!(
                        "{} · {} turn{}",
                        c.harness,
                        c.turns,
                        if c.turns == 1 { "" } else { "s" }
                    );
                    kit::text_at(
                        painter,
                        egui::pos2(x, rect.center().y + 10.0),
                        Align2::LEFT_CENTER,
                        &sub,
                        theme::regular(12.5),
                        theme::MUTED,
                        rect.right() - x - 8.0,
                    );
                    if c.running {
                        kit::dot(
                            painter,
                            egui::pos2(rect.right() - 12.0, rect.center().y + 10.0),
                            4.0,
                            theme::OMNI,
                        );
                    }
                    if resp.clicked() {
                        self.selection.omni_session = Some(c.id);
                        self.omni_open = true;
                        self.request();
                    }
                }
                // People memories are about.
                let nodes = nodes(self, &s);
                let people: Vec<&Node> = nodes.iter().filter(|n| n.person).take(5).collect();
                if !people.is_empty() {
                    ui.add_space(10.0);
                    heading(ui, "People", Some(people.len()));
                    for n in people {
                        let selected = self.memory_entity_filter == Some(n.entity);
                        let (rect, resp) = kit::row(ui, 54.0, selected);
                        let painter = ui.painter();
                        theme::paint_avatar(
                            painter,
                            egui::pos2(rect.left() + 26.0, rect.center().y),
                            36.0,
                            &n.name,
                            theme::Presence::None,
                            if selected {
                                theme::SELECTED
                            } else {
                                theme::SIDEBAR
                            },
                        );
                        let x = rect.left() + 54.0;
                        kit::text_at(
                            painter,
                            egui::pos2(x, rect.center().y - 9.0),
                            Align2::LEFT_CENTER,
                            &n.name,
                            theme::medium(14.5),
                            theme::TEXT,
                            rect.right() - x - 8.0,
                        );
                        kit::text_at(
                            painter,
                            egui::pos2(x, rect.center().y + 10.0),
                            Align2::LEFT_CENTER,
                            &format!(
                                "Person · {} memor{}",
                                n.count,
                                if n.count == 1 { "y" } else { "ies" }
                            ),
                            theme::regular(12.5),
                            theme::MUTED,
                            rect.right() - x - 8.0,
                        );
                        if resp.clicked() {
                            self.memory_entity_filter =
                                if selected { None } else { Some(n.entity) };
                        }
                    }
                }
                // Collections by kind.
                let mut by_kind: BTreeMap<u8, (MemoryKind, usize)> = BTreeMap::new();
                for m in &s.memory.memories {
                    let e = by_kind.entry(kind_order(m.kind)).or_insert((m.kind, 0));
                    e.1 += 1;
                }
                ui.add_space(10.0);
                heading(ui, "Collections", Some(by_kind.len()));
                if by_kind.is_empty() {
                    kit::label(
                        ui,
                        "Nothing remembered yet.",
                        theme::regular(13.5),
                        theme::MUTED,
                    );
                }
                for (_, (kind, n)) in by_kind {
                    let selected = self.memory_kind_filter == Some(kind);
                    let (rect, resp) = kit::row(ui, 52.0, selected);
                    let painter = ui.painter();
                    let (g, t) = kind_style(kind);
                    kit::paint_tile(
                        painter,
                        Rect::from_center_size(
                            egui::pos2(rect.left() + 26.0, rect.center().y),
                            egui::vec2(36.0, 36.0),
                        ),
                        g,
                        t,
                        10.0,
                    );
                    let x = rect.left() + 54.0;
                    kit::text_at(
                        painter,
                        egui::pos2(x, rect.center().y - 9.0),
                        Align2::LEFT_CENTER,
                        kind_label(kind),
                        theme::medium(14.5),
                        theme::TEXT,
                        rect.right() - x - 8.0,
                    );
                    kit::text_at(
                        painter,
                        egui::pos2(x, rect.center().y + 10.0),
                        Align2::LEFT_CENTER,
                        &format!("{n} item{}", if n == 1 { "" } else { "s" }),
                        theme::regular(12.5),
                        theme::MUTED,
                        rect.right() - x - 8.0,
                    );
                    if resp.clicked() {
                        self.memory_kind_filter = if selected { None } else { Some(kind) };
                    }
                }
            });
    }

    // ---- Main --------------------------------------------------------------------

    pub(crate) fn omni_screen(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("omni_screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                // Title with actions on the right (A10).
                let (hdr, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 58.0),
                    egui::Sense::hover(),
                );
                let painter = ui.painter();
                painter.text(
                    hdr.left_top() + egui::vec2(0.0, 2.0),
                    Align2::LEFT_TOP,
                    "Omni",
                    theme::semibold(27.0),
                    theme::TEXT,
                );
                kit::text_at(
                    painter,
                    hdr.left_top() + egui::vec2(0.0, 38.0),
                    Align2::LEFT_TOP,
                    "Search across people, conversations, and context.",
                    theme::regular(15.5),
                    theme::lerp(theme::SECONDARY, theme::PRIMARY_TEXT, 0.25),
                    hdr.width() - 280.0,
                );
                let ask_w = kit::button_width(ui.painter(), Some(ph::SPARKLE), "Ask Omni", 36.0);
                let new_w = kit::button_width(ui.painter(), Some(ph::PLUS), "New chat", 36.0);
                let ask = Rect::from_min_size(
                    egui::pos2(hdr.right() - ask_w, hdr.top() + 4.0),
                    egui::vec2(ask_w, 36.0),
                );
                let new = Rect::from_min_size(
                    egui::pos2(ask.left() - 10.0 - new_w, hdr.top() + 4.0),
                    egui::vec2(new_w, 36.0),
                );
                if kit::button_at(
                    ui,
                    new,
                    ui.id().with("omni_new"),
                    kit::Kind::Secondary,
                    Some(ph::PLUS),
                    "New chat",
                    true,
                )
                .clicked()
                {
                    self.selection.omni_session = None;
                    self.omni_open = true;
                    self.request();
                }
                if kit::button_at(
                    ui,
                    ask,
                    ui.id().with("omni_ask"),
                    kit::Kind::Primary,
                    Some(ph::SPARKLE),
                    "Ask Omni",
                    true,
                )
                .clicked()
                {
                    self.omni_open = true;
                }
                self.omni_ask_bar(ui, &s);
                ui.add_space(4.0);
                let nodes = nodes(self, &s);
                let w = ui.available_width();
                if w > 700.0 {
                    let left_w = (w * 0.56).floor();
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        ui.allocate_ui_with_layout(
                            egui::vec2(left_w, 330.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_width(left_w);
                                self.memory_graph(ui, &s, &nodes, 330.0);
                            },
                        );
                        ui.allocate_ui_with_layout(
                            egui::vec2(w - left_w - 12.0, 330.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_width(w - left_w - 12.0);
                                self.memory_cards(ui, &s);
                            },
                        );
                    });
                } else {
                    self.memory_graph(ui, &s, &nodes, 300.0);
                    self.memory_cards(ui, &s);
                }
                ui.add_space(6.0);
                self.source_timeline(ui, &s);
                ui.add_space(6.0);
                self.omni_context(ui, &s);
                ui.add_space(6.0);
                kit::section(ui, "Omni memory assistance", None, None);
                self.memory_assistance(ui, &s);
                ui.add_space(12.0);
            });
    }

    /// A composer that starts an Omni chat, or sign-in guidance.
    fn omni_ask_bar(&mut self, ui: &mut Ui, s: &Snapshot) {
        let status = &s.omni.status;
        let ready = matches!(status.login, LoginState::Ready { .. } | LoginState::Stopped)
            && status.unavailable.is_none()
            && status.selected.is_some();
        let (fill, stroke) = kit::accent_colors(theme::OMNI);
        egui::Frame::new()
            .fill(fill)
            .stroke(Stroke::new(1.0_f32, stroke))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    kit::bubble(ui, ph::SPARKLE, kit::TEAL, 36.0);
                    if ready {
                        let w = ui.available_width() - 110.0;
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.omni_draft)
                                .frame(egui::Frame::NONE)
                                .font(theme::regular(15.5))
                                .text_color(theme::TEXT)
                                .desired_width(w)
                                .hint_text(egui::RichText::new("Ask about your people, messages and tasks…").font(theme::regular(15.5)).color(theme::MUTED)),
                        );
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let text = self.omni_draft.trim().to_owned();
                        let send = kit::button_ex(ui, kit::Kind::Omni, Some(ph::PAPER_PLANE_RIGHT), "Ask", 32.0, !text.is_empty() && !self.busy).clicked();
                        if (send || enter) && !text.is_empty() && !self.busy {
                            self.send(Command::Omni(OmniCommand::Send(None, text)));
                            self.omni_draft.clear();
                            self.omni_open = true;
                        }
                    } else {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            let (title, body) = match (&status.selected, &status.login) {
                                (_, _) if status.unavailable.is_some() => ("Omni is unavailable", status.unavailable.clone().unwrap_or_default()),
                                (None, _) => ("Connect Omni", "Sign in with ChatGPT through Codex, or with any OpenCode provider. Your harness keeps its own credentials.".to_owned()),
                                (Some(k), LoginState::NotInstalled) => ("Install your harness", format!("{} isn't installed on this computer yet.", k.label())),
                                (Some(k), _) => ("Sign in to finish setup", format!("{} needs you to sign in before Omni can answer.", k.label())),
                            };
                            kit::label(ui, title, theme::medium(15.0), theme::TEXT);
                            kit::para(ui, body, theme::regular(13.5), theme::SECONDARY);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if kit::button_ex(ui, kit::Kind::Omni, Some(ph::SIGN_IN), "Set up Omni", 32.0, true).clicked() {
                                self.settings_section = Some("Omni".into());
                                self.navigate(Destination::Settings);
                            }
                        });
                    }
                });
            });
    }

    /// Graph of the people and conversations memories are about (A10).
    fn memory_graph(&mut self, ui: &mut Ui, s: &Snapshot, nodes: &[Node], height: f32) {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        kit::paint_card(ui.painter(), rect, false);
        let painter = ui.painter().with_clip_rect(rect);
        painter.text(
            rect.left_top() + egui::vec2(16.0, 16.0),
            Align2::LEFT_TOP,
            "Memory graph",
            theme::semibold(16.0),
            theme::TEXT,
        );
        if self.memory_entity_filter.is_some() {
            let r = Rect::from_min_size(
                egui::pos2(rect.right() - 92.0, rect.top() + 12.0),
                egui::vec2(80.0, 24.0),
            );
            if kit::button_at(
                ui,
                r,
                ui.id().with("graph_clear"),
                kit::Kind::Ghost,
                None,
                "Show all",
                true,
            )
            .clicked()
            {
                self.memory_entity_filter = None;
            }
        }
        let shown: Vec<&Node> = nodes.iter().take(6).collect();
        let center = egui::pos2(rect.center().x, rect.center().y + 14.0);
        let me = self.display(&s.account.display_name);
        if shown.is_empty() {
            kit::text_at(
                &painter,
                center + egui::vec2(0.0, 50.0),
                Align2::CENTER_CENTER,
                "Memories link people and conversations here as they're found.",
                theme::regular(13.0),
                theme::MUTED,
                rect.width() - 40.0,
            );
        }
        let rx = rect.width() * 0.36;
        let ry = (height - 90.0) * 0.42;
        let mut positions = Vec::new();
        for (i, _) in shown.iter().enumerate() {
            let a = -std::f32::consts::FRAC_PI_2
                + i as f32 * std::f32::consts::TAU / shown.len().max(1) as f32
                + 0.35;
            positions.push(center + egui::vec2(a.cos() * rx, a.sin() * ry));
        }
        // Edges first.
        for (n, p) in shown.iter().zip(&positions) {
            let selected = self.memory_entity_filter == Some(n.entity);
            let color = if selected {
                theme::OMNI
            } else {
                theme::lerp(theme::BORDER, theme::OMNI, 0.45)
            };
            painter.line_segment(
                [center, *p],
                Stroke::new(if selected { 1.8 } else { 1.2 }, color),
            );
            let mid = center + (*p - center) * 0.5;
            let label = format!("{} {}", n.count, edge_label(n.kind));
            let g = painter.layout_no_wrap(label, theme::regular(11.5), theme::MUTED);
            let r = Rect::from_center_size(mid, g.size() + egui::vec2(10.0, 4.0));
            painter.rect_filled(r, 6.0, theme::CARD);
            painter.galley(r.min + egui::vec2(5.0, 2.0), g, theme::MUTED);
        }
        // Center: you.
        kit::glow(
            &painter,
            Rect::from_center_size(center, egui::vec2(64.0, 64.0)),
            32.0,
            kit::PURPLE.fg,
        );
        theme::gradient_disc(
            &painter,
            center,
            32.0,
            Color32::from_rgb(190, 120, 255),
            Color32::from_rgb(96, 60, 210),
        );
        kit::icon(&painter, center, ph::SPARKLE, 26.0, Color32::WHITE);
        painter.text(
            center + egui::vec2(0.0, 44.0),
            Align2::CENTER_CENTER,
            if me.is_empty() { "You" } else { &me },
            theme::medium(14.0),
            theme::TEXT,
        );
        // Nodes.
        let mut clicked = None;
        for (i, (n, p)) in shown.iter().zip(&positions).enumerate() {
            let selected = self.memory_entity_filter == Some(n.entity);
            if n.person {
                theme::paint_avatar(
                    &painter,
                    *p,
                    36.0,
                    &n.name,
                    theme::Presence::None,
                    theme::CARD,
                );
            } else {
                kit::paint_tile(
                    &painter,
                    Rect::from_center_size(*p, egui::vec2(36.0, 36.0)),
                    ph::CHAT_CIRCLE_TEXT,
                    kit::BLUE,
                    10.0,
                );
            }
            if selected {
                painter.circle_stroke(*p, 22.0, Stroke::new(2.0_f32, theme::OMNI));
            }
            let left = p.x < center.x;
            let tp = if left {
                *p - egui::vec2(24.0, 0.0)
            } else {
                *p + egui::vec2(24.0, 0.0)
            };
            let tr = kit::text_at(
                &painter,
                tp,
                if left {
                    Align2::RIGHT_CENTER
                } else {
                    Align2::LEFT_CENTER
                },
                &n.name,
                theme::medium(13.5),
                theme::TEXT,
                130.0,
            );
            let hit = Rect::from_center_size(*p, egui::vec2(40.0, 40.0)).union(tr);
            if ui
                .interact(hit, ui.id().with(("graph_node", i)), egui::Sense::click())
                .on_hover_text("Show memories about this")
                .clicked()
            {
                clicked = Some(n.entity);
            }
        }
        if let Some(e) = clicked {
            self.memory_entity_filter = if self.memory_entity_filter == Some(e) {
                None
            } else {
                Some(e)
            };
        }
    }

    /// The most important recent memories as cards (A10 "Related memory cards").
    fn memory_cards(&mut self, ui: &mut Ui, s: &Snapshot) {
        let mut items: Vec<&MemoryItem> = filtered(self, s);
        items.sort_by(|a, b| {
            b.importance
                .partial_cmp(&a.importance)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.created_at.as_millis().cmp(&a.created_at.as_millis()))
        });
        let (h, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), egui::Sense::hover());
        ui.painter().text(
            h.left_center(),
            Align2::LEFT_CENTER,
            "Memory cards",
            theme::semibold(16.0),
            theme::TEXT,
        );
        if items.is_empty() {
            kit::empty(
                ui,
                Some(ph::BRAIN),
                "Nothing here yet",
                "Commitments, dates and replies you owe are picked up as messages sync.",
            );
            return;
        }
        let now = Timestamp::now();
        let mut pick = None;
        for (i, m) in items.iter().take(4).enumerate() {
            let (rect, resp) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 70.0), egui::Sense::click());
            let selected = self.selected_memory == Some(m.id);
            let accent = if m.kind == MemoryKind::Commitment || m.kind == MemoryKind::ImportantDate
            {
                None
            } else {
                Some(theme::OMNI)
            };
            match (selected, accent) {
                (true, _) => {
                    ui.painter().rect(
                        rect,
                        12.0,
                        theme::lerp(theme::CARD, theme::PRIMARY, 0.1),
                        Stroke::new(1.0_f32, theme::PRIMARY),
                        egui::StrokeKind::Inside,
                    );
                }
                (false, Some(a)) => {
                    let (f, st) = kit::accent_colors(a);
                    ui.painter().rect(
                        rect,
                        12.0,
                        if resp.hovered() {
                            theme::lerp(f, a, 0.05)
                        } else {
                            f
                        },
                        Stroke::new(1.0_f32, st),
                        egui::StrokeKind::Inside,
                    );
                }
                (false, None) => kit::paint_card(ui.painter(), rect, resp.hovered()),
            }
            let painter = ui.painter();
            let (g, t) = kind_style(m.kind);
            kit::paint_bubble(
                painter,
                egui::pos2(rect.left() + 30.0, rect.center().y),
                g,
                t,
                40.0,
            );
            let x = rect.left() + 58.0;
            let when = list_time(m.observed_at.unwrap_or(m.created_at), now);
            let tw = kit::text_width(painter, &when, theme::regular(12.0));
            kit::text_at(
                painter,
                egui::pos2(x, rect.top() + 16.0),
                Align2::LEFT_CENTER,
                &format!("{} memory", kind_label(m.kind).trim_end_matches('s')),
                theme::medium(12.5),
                t.fg,
                rect.right() - x - tw - 20.0,
            );
            painter.text(
                egui::pos2(rect.right() - 12.0, rect.top() + 16.0),
                Align2::RIGHT_CENTER,
                when,
                theme::regular(12.0),
                theme::MUTED,
            );
            kit::text_at(
                painter,
                egui::pos2(x, rect.top() + 36.0),
                Align2::LEFT_CENTER,
                &self.display(&m.content),
                theme::medium(14.5),
                theme::TEXT,
                rect.right() - x - 12.0,
            );
            kit::text_at(
                painter,
                egui::pos2(x, rect.top() + 55.0),
                Align2::LEFT_CENTER,
                &source_line(s, m),
                theme::regular(12.5),
                theme::MUTED,
                rect.right() - x - 12.0,
            );
            if resp.clicked() {
                pick = Some(m.id);
            }
            if i < 3 {
                ui.add_space(0.0);
            }
        }
        if let Some(id) = pick {
            self.selected_memory = Some(id);
        }
    }

    /// Every memory in the current filter, newest first, with its source.
    /// Candidates carry Confirm/Reject, sized to fit (never clipped).
    fn source_timeline(&mut self, ui: &mut Ui, s: &Snapshot) {
        let (title_rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::hover());
        ui.painter().text(
            title_rect.left_center(),
            Align2::LEFT_CENTER,
            "Source timeline",
            theme::semibold(17.0),
            theme::TEXT,
        );
        let labels = ["All", "Commitments", "Waiting", "Dates", "Facts"];
        let mut sel = match self.memory_kind_filter {
            None => 0,
            Some(MemoryKind::Commitment) => 1,
            Some(MemoryKind::PendingReply) => 2,
            Some(MemoryKind::ImportantDate) => 3,
            Some(MemoryKind::Fact) => 4,
            Some(_) => 99,
        };
        let chips_w: f32 = labels
            .iter()
            .map(|l| kit::text_width(ui.painter(), l, theme::medium(13.0)) + 32.0)
            .sum();
        if title_rect.width() > chips_w + 180.0 {
            let chips = Rect::from_min_max(
                egui::pos2(title_rect.right() - chips_w, title_rect.top()),
                title_rect.max,
            );
            let mut cui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(chips)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            if kit::chip_row(&mut cui, &labels, &mut sel) {
                self.memory_kind_filter = match sel {
                    1 => Some(MemoryKind::Commitment),
                    2 => Some(MemoryKind::PendingReply),
                    3 => Some(MemoryKind::ImportantDate),
                    4 => Some(MemoryKind::Fact),
                    _ => None,
                };
            }
        }
        ui.add_space(2.0);
        let mut items = filtered(self, s);
        items.sort_by_key(|m| std::cmp::Reverse(m.observed_at.unwrap_or(m.created_at).as_millis()));
        if s.memory.memories.is_empty() {
            kit::empty(ui, Some(ph::BRAIN), "Nothing remembered yet", "Commitments, dates and replies you owe are picked up from your messages as they sync. You can also ask Omni to remember something from a chat.");
            return;
        }
        if items.is_empty() {
            kit::empty(
                ui,
                Some(ph::FUNNEL),
                "No matches",
                "Try another filter or clear the search.",
            );
            return;
        }
        let now = Timestamp::now();
        let mut pick = None;
        let mut decide: Option<(litecord_types::MemoryId, bool)> = None;
        egui::Frame::new()
            .fill(theme::CARD)
            .stroke(Stroke::new(1.0_f32, theme::BORDER))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, m) in items.iter().take(40).enumerate() {
                    if i > 0 {
                        kit::divider(ui);
                    }
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 60.0),
                        egui::Sense::click(),
                    );
                    let selected = self.selected_memory == Some(m.id);
                    if selected {
                        ui.painter().rect_filled(
                            rect,
                            8.0,
                            theme::lerp(theme::CARD, theme::SELECTED, 0.8),
                        );
                    } else if resp.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            8.0,
                            theme::lerp(theme::CARD, theme::HOVER, 0.6),
                        );
                    }
                    let candidate = m.status == MemoryStatus::Candidate;
                    let painter = ui.painter();
                    kit::dot(
                        painter,
                        egui::pos2(rect.left() + 12.0, rect.center().y),
                        4.0,
                        if candidate {
                            theme::PRIMARY
                        } else {
                            theme::BORDER
                        },
                    );
                    let (g, t) = kind_style(m.kind);
                    let red = m.kind == MemoryKind::ImportantDate;
                    kit::paint_bubble(
                        painter,
                        egui::pos2(rect.left() + 44.0, rect.center().y),
                        g,
                        t,
                        38.0,
                    );
                    // Right side, laid out from the edge inward.
                    let mut right = rect.right() - 10.0;
                    let mut buttons = Vec::new();
                    if candidate {
                        for (label, kind, confirm) in [
                            ("Reject", kit::Kind::Ghost, false),
                            ("Confirm", kit::Kind::Primary, true),
                        ] {
                            let w = kit::button_width(painter, None, label, 28.0);
                            let r = Rect::from_min_size(
                                egui::pos2(right - w, rect.center().y - 14.0),
                                egui::vec2(w, 28.0),
                            );
                            buttons.push((r, label, kind, confirm));
                            right -= w + 6.0;
                        }
                        right -= 4.0;
                    }
                    let when = list_time(m.observed_at.unwrap_or(m.created_at), now);
                    let ww = kit::text_width(painter, &when, theme::regular(12.5));
                    painter.text(
                        egui::pos2(right, rect.center().y - 10.0),
                        Align2::RIGHT_CENTER,
                        &when,
                        theme::regular(12.5),
                        theme::MUTED,
                    );
                    let conf = format!("{:.0}% sure", m.confidence.get() * 100.0);
                    painter.text(
                        egui::pos2(right, rect.center().y + 11.0),
                        Align2::RIGHT_CENTER,
                        &conf,
                        theme::regular(12.0),
                        theme::OMNI_TEXT,
                    );
                    let meta_w = ww.max(kit::text_width(painter, &conf, theme::regular(12.0)));
                    let x = rect.left() + 74.0;
                    let text_w = (right - meta_w - 16.0 - x).max(40.0);
                    kit::text_at(
                        painter,
                        egui::pos2(x, rect.center().y - 10.0),
                        Align2::LEFT_CENTER,
                        &self.display(&m.content),
                        theme::medium(14.5),
                        if red {
                            theme::PRIORITY_TEXT
                        } else {
                            theme::TEXT
                        },
                        text_w,
                    );
                    kit::text_at(
                        painter,
                        egui::pos2(x, rect.center().y + 11.0),
                        Align2::LEFT_CENTER,
                        &format!("{} · {}", origin_label(m.origin), source_line(s, m)),
                        theme::regular(13.0),
                        theme::MUTED,
                        text_w,
                    );
                    for (k, (r, label, kind, confirm)) in buttons.into_iter().enumerate() {
                        if kit::button_at(
                            ui,
                            r,
                            ui.id().with(("mem_btn", i, k)),
                            kind,
                            None,
                            label,
                            !self.busy,
                        )
                        .clicked()
                        {
                            decide = Some((m.id, confirm));
                        }
                    }
                    let label = format!("{}: {}", kind_label(m.kind), self.display(&m.content));
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            &label,
                        )
                    });
                    if resp.clicked() {
                        pick = Some(m.id);
                    }
                }
            });
        if let Some(id) = pick {
            self.selected_memory = Some(id);
        }
        match decide {
            Some((id, true)) => self.send(Command::ConfirmMemory(id)),
            Some((id, false)) => self.send(Command::RejectMemory(id)),
            None => {}
        }
    }

    /// What Omni may read (A10 "Context currently selected").
    fn omni_context(&mut self, ui: &mut Ui, s: &Snapshot) {
        let readable: Vec<_> = s
            .conversations
            .conversations
            .iter()
            .filter(|c| c.agent_visibility == AgentVisibility::Allowed)
            .take(8)
            .cloned()
            .collect();
        let total = s
            .conversations
            .conversations
            .iter()
            .filter(|c| c.agent_visibility == AgentVisibility::Allowed)
            .count();
        kit::section(
            ui,
            &format!("Conversations Omni can read ({total})"),
            None,
            None,
        );
        if readable.is_empty() {
            kit::label(
                ui,
                "Omni can't read any conversations. Change this per chat in its details.",
                theme::regular(13.5),
                theme::MUTED,
            );
            return;
        }
        let mut hide = None;
        kit::card_grid(ui, readable.len(), 54.0, 10.0, 180.0, 4, |ui, i, rect| {
            let c = &readable[i];
            kit::paint_card(ui.painter(), rect, false);
            let title = self.display(&c.title);
            crate::home_ui::paint_conversation_avatar(
                ui.painter(),
                egui::pos2(rect.left() + 26.0, rect.center().y),
                32.0,
                c,
                &title,
                theme::CARD,
            );
            let x = rect.left() + 50.0;
            kit::text_at(
                ui.painter(),
                egui::pos2(x, rect.center().y - 8.0),
                Align2::LEFT_CENTER,
                &title,
                theme::medium(14.0),
                theme::TEXT,
                rect.right() - x - 34.0,
            );
            kit::text_at(
                ui.painter(),
                egui::pos2(x, rect.center().y + 10.0),
                Align2::LEFT_CENTER,
                "Messages readable",
                theme::regular(12.0),
                theme::MUTED,
                rect.right() - x - 34.0,
            );
            let xr = Rect::from_center_size(
                egui::pos2(rect.right() - 18.0, rect.center().y),
                egui::vec2(24.0, 24.0),
            );
            if kit::icon_button_at(
                ui,
                xr,
                ui.id().with(("ctx_hide", i)),
                ph::X,
                "Hide this conversation from Omni",
                theme::SECONDARY,
            )
            .clicked()
            {
                hide = Some(c.conversation_id);
            }
        });
        if let Some(id) = hide {
            self.send(Command::Visibility(id, AgentVisibility::Hidden));
        }
    }

    fn memory_assistance(&mut self, ui: &mut Ui, s: &Snapshot) {
        let review = crate::home_ui::candidates(s);
        let commitments = s
            .memory
            .memories
            .iter()
            .filter(|m| m.kind == MemoryKind::Commitment)
            .count();
        let cards: [(&str, String, &str, &str); 4] = [
            (
                ph::BRAIN,
                format!(
                    "{review} memor{} to review",
                    if review == 1 { "y" } else { "ies" }
                ),
                "Confirm what's right; reject the rest.",
                "Review",
            ),
            (
                ph::CHECK_SQUARE,
                format!(
                    "{commitments} commitment{} tracked",
                    if commitments == 1 { "" } else { "s" }
                ),
                "Promises you made in chat, kept in one place.",
                "Open tasks",
            ),
            (
                ph::MAGNIFYING_GLASS,
                "Find anything".into(),
                "Search across people, conversations and memory.",
                "Search",
            ),
            (
                ph::SPARKLE,
                "Ask about someone".into(),
                "Omni summarizes what you've discussed with a person.",
                "Ask Omni",
            ),
        ];
        let mut clicked = None;
        kit::card_grid(ui, 4, 150.0, 12.0, 170.0, 4, |ui, i, rect| {
            let (g, title, body, button) = &cards[i];
            kit::card_at(ui, rect, ui.id().with(("mem_assist", i)), Some(theme::OMNI));
            let painter = ui.painter();
            kit::paint_bubble(
                painter,
                egui::pos2(rect.left() + 30.0, rect.top() + 32.0),
                g,
                kit::TEAL,
                42.0,
            );
            let x = rect.left() + 60.0;
            let w = rect.right() - x - 10.0;
            let tg = kit::wrapped(painter, title, theme::medium(14.0), theme::TEXT, w, 2);
            let th = tg.size().y;
            painter.galley(egui::pos2(x, rect.top() + 14.0), tg, theme::TEXT);
            let bg = kit::wrapped(painter, body, theme::regular(12.5), theme::MUTED, w, 3);
            painter.galley(egui::pos2(x, rect.top() + 18.0 + th), bg, theme::MUTED);
            let bw = (rect.width() - 56.0).min(150.0);
            let br = Rect::from_center_size(
                egui::pos2(rect.center().x, rect.bottom() - 26.0),
                egui::vec2(bw, 30.0),
            );
            if kit::button_at(
                ui,
                br,
                ui.id().with(("mem_assist_btn", i)),
                kit::Kind::Omni,
                None,
                button,
                true,
            )
            .clicked()
            {
                clicked = Some(i);
            }
        });
        match clicked {
            Some(0) => self.memory_status_filter = Some(MemoryStatus::Candidate),
            Some(1) => self.navigate(Destination::Tasks),
            Some(2) => {
                self.palette_open = true;
                self.palette_focus_requested = true;
            }
            Some(_) => {
                self.omni_open = true;
                self.omni_draft =
                    "Who have I been talking to most this week, and what's open with each of them?"
                        .into();
            }
            None => {}
        }
    }

    // ---- Inspector -------------------------------------------------------------

    pub(crate) fn omni_inspector(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        let selected = s
            .memory
            .memories
            .iter()
            .find(|m| Some(m.id) == self.selected_memory)
            .cloned();
        egui::ScrollArea::vertical()
            .id_salt("omni_inspector")
            .auto_shrink([false, false])
            .show(ui, |ui| match selected {
                Some(m) => self.memory_detail(ui, &s, &m),
                None => self.omni_status_panel(ui, &s),
            });
    }

    fn memory_detail(&mut self, ui: &mut Ui, s: &Snapshot, m: &MemoryItem) {
        let (g, t) = kind_style(m.kind);
        let (r, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 84.0), egui::Sense::hover());
        kit::paint_tile(
            ui.painter(),
            Rect::from_min_size(r.min, egui::vec2(76.0, 76.0)),
            g,
            t,
            38.0,
        );
        if m.status == MemoryStatus::Candidate {
            let pill = Rect::from_min_size(
                egui::pos2(r.right() - 110.0, r.top() + 4.0),
                egui::vec2(110.0, 26.0),
            );
            kit::paint_status_pill(
                ui.painter(),
                pill,
                Some(ph::WARNING_CIRCLE),
                "Needs review",
                theme::WARNING,
            );
        }
        ui.add_space(6.0);
        kit::para(
            ui,
            self.display(&m.content),
            theme::semibold(20.0),
            theme::TEXT,
        );
        kit::label(
            ui,
            format!("{} memory", kind_label(m.kind).trim_end_matches('s')),
            theme::regular(15.0),
            theme::SECONDARY,
        );
        ui.add_space(8.0);
        let when = relative(
            m.observed_at.unwrap_or(m.created_at).as_millis() - Timestamp::now().as_millis(),
        );
        for (glyph, text) in [
            (
                ph::STACK,
                format!(
                    "{} source{}",
                    m.source_refs.len(),
                    if m.source_refs.len() == 1 { "" } else { "s" }
                ),
            ),
            (
                ph::TARGET,
                format!("{:.0}% sure", m.confidence.get() * 100.0),
            ),
            (ph::CLOCK, format!("Noticed {when}")),
        ] {
            let (row, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 26.0), egui::Sense::hover());
            kit::icon_o(
                ui.painter(),
                row.left_center() + egui::vec2(10.0, 0.0),
                glyph,
                18.0,
                theme::SECONDARY,
            );
            kit::text_at(
                ui.painter(),
                row.left_center() + egui::vec2(32.0, 0.0),
                Align2::LEFT_CENTER,
                &text,
                theme::regular(15.0),
                theme::lerp(theme::TEXT, theme::SECONDARY, 0.3),
                row.width() - 36.0,
            );
        }
        if m.status == MemoryStatus::Candidate {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if kit::button_ex(
                    ui,
                    kit::Kind::Primary,
                    Some(ph::CHECK),
                    "Confirm",
                    32.0,
                    !self.busy,
                )
                .clicked()
                {
                    self.send(Command::ConfirmMemory(m.id));
                }
                if kit::button_ex(
                    ui,
                    kit::Kind::Secondary,
                    Some(ph::X),
                    "Reject",
                    32.0,
                    !self.busy,
                )
                .clicked()
                {
                    self.send(Command::RejectMemory(m.id));
                }
            });
        }
        ui.add_space(12.0);
        kit::divider(ui);
        ui.add_space(10.0);
        kit::section(ui, "Sources", None, None);
        if m.source_refs.is_empty() {
            kit::label(
                ui,
                "No source recorded.",
                theme::regular(13.5),
                theme::MUTED,
            );
        }
        for (i, src) in m.source_refs.iter().enumerate() {
            let (text, target) = describe(s, &src.entity);
            let (rect, resp) = kit::row(ui, 50.0, false);
            let painter = ui.painter();
            kit::paint_bubble(
                painter,
                egui::pos2(rect.left() + 22.0, rect.center().y),
                ph::CHAT_CIRCLE_TEXT,
                kit::BLUE,
                34.0,
            );
            kit::text_at(
                painter,
                egui::pos2(rect.left() + 48.0, rect.center().y - 8.0),
                Align2::LEFT_CENTER,
                &self.display(&text),
                theme::medium(14.0),
                theme::TEXT,
                rect.width() - 76.0,
            );
            if let Some(note) = &src.note {
                kit::text_at(
                    painter,
                    egui::pos2(rect.left() + 48.0, rect.center().y + 10.0),
                    Align2::LEFT_CENTER,
                    &self.display(note),
                    theme::regular(12.5),
                    theme::MUTED,
                    rect.width() - 76.0,
                );
            }
            if target.is_some() {
                kit::chevron(
                    painter,
                    rect.right_center() - egui::vec2(12.0, 0.0),
                    theme::MUTED,
                );
            }
            if resp.clicked() {
                match target {
                    Some(Target::Conversation(id)) => self.open_conversation(id),
                    Some(Target::Session(id)) => {
                        self.selection.omni_session = Some(id);
                        self.omni_open = true;
                        self.request();
                    }
                    None => {}
                }
            }
            let _ = i;
        }
        ui.add_space(10.0);
        kit::divider(ui);
        ui.add_space(10.0);
        kit::section(ui, "Labels", None, None);
        ui.horizontal_wrapped(|ui| {
            kit::status_pill(
                ui,
                Some(ph::EYE),
                status_label(m.status),
                status_color(m.status),
            );
            kit::status_pill(ui, Some(ph::FILE_TEXT), kind_label(m.kind), t.fg);
            kit::status_pill(ui, None, origin_label(m.origin), theme::MUTED);
        });
        if !m.entities.is_empty() {
            ui.add_space(10.0);
            kit::divider(ui);
            ui.add_space(10.0);
            kit::section(ui, "About", None, None);
            for e in &m.entities {
                let (text, _) = describe(s, e);
                kit::label(
                    ui,
                    self.display(&text),
                    theme::regular(14.0),
                    theme::SECONDARY,
                );
            }
        }
        ui.add_space(10.0);
        kit::para(
            ui,
            if m.origin == Origin::AgentDerived {
                "Suggested by an agent. Omni uses it only after you confirm it, or when you ask."
            } else {
                "Used as context for Omni and search."
            },
            theme::regular(12.5),
            theme::MUTED,
        );
        if let Some(id) = m.superseded_by {
            kit::label(
                ui,
                format!("Replaced by a newer record (#{id})."),
                theme::regular(12.5),
                theme::MUTED,
            );
        }
    }

    /// Omni's harness, account and quick actions when no memory is selected.
    fn omni_status_panel(&mut self, ui: &mut Ui, s: &Snapshot) {
        let status = &s.omni.status;
        let (r, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 84.0), egui::Sense::hover());
        let c = r.left_center() + egui::vec2(38.0, 0.0);
        kit::glow(
            ui.painter(),
            Rect::from_center_size(c, egui::vec2(76.0, 76.0)),
            38.0,
            theme::OMNI,
        );
        theme::gradient_disc(
            ui.painter(),
            c,
            38.0,
            Color32::from_rgb(64, 226, 240),
            Color32::from_rgb(30, 110, 170),
        );
        kit::icon(ui.painter(), c, ph::SPARKLE, 34.0, Color32::WHITE);
        ui.add_space(6.0);
        kit::label(ui, "Omni", theme::semibold(22.0), theme::TEXT);
        let (state, color) = match (&status.selected, &status.login) {
            (_, _) if status.unavailable.is_some() => ("Unavailable".to_owned(), theme::PRIORITY),
            (None, _) => ("No harness connected".to_owned(), theme::MUTED),
            (Some(k), LoginState::Ready { account, .. }) => (
                format!(
                    "Ready on {}{}",
                    k.label(),
                    account
                        .as_deref()
                        .map(|a| format!(" · {a}"))
                        .unwrap_or_default()
                ),
                theme::SUCCESS,
            ),
            (Some(k), LoginState::Stopped) => (
                format!("{} · starts when you ask", k.label()),
                theme::SUCCESS,
            ),
            (Some(k), LoginState::SigningIn { .. }) => {
                (format!("{} · signing in…", k.label()), theme::WARNING)
            }
            (Some(k), LoginState::NotInstalled) => {
                (format!("{} isn't installed", k.label()), theme::PRIORITY)
            }
            (Some(k), LoginState::SignedOut) => {
                (format!("{} · signed out", k.label()), theme::WARNING)
            }
            (Some(k), LoginState::Error { .. }) => {
                (format!("{} · error", k.label()), theme::PRIORITY)
            }
        };
        ui.horizontal(|ui| {
            let (d, _) = ui.allocate_exact_size(egui::vec2(10.0, 18.0), egui::Sense::hover());
            kit::dot(ui.painter(), d.center(), 4.5, color);
            kit::label(ui, state, theme::regular(14.0), theme::SECONDARY);
        });
        if let Some(m) = &status.model {
            kit::label(
                ui,
                format!("Model · {m}"),
                theme::regular(13.5),
                theme::MUTED,
            );
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if kit::button_ex(
                ui,
                kit::Kind::Primary,
                Some(ph::SPARKLE),
                "Ask Omni",
                34.0,
                true,
            )
            .clicked()
            {
                self.omni_open = true;
            }
            if kit::button_ex(
                ui,
                kit::Kind::Secondary,
                Some(ph::GEAR_SIX),
                "Settings",
                34.0,
                true,
            )
            .clicked()
            {
                self.settings_section = Some("Omni".into());
                self.navigate(Destination::Settings);
            }
        });
        ui.add_space(12.0);
        kit::divider(ui);
        ui.add_space(10.0);
        kit::section(ui, "Automations", Some(s.omni.automations.len()), None);
        if s.omni.automations.is_empty() {
            kit::para(
                ui,
                "Morning brief, reply radar and more run on their own. Set them up in Settings.",
                theme::regular(13.0),
                theme::MUTED,
            );
        }
        for a in s.omni.automations.iter().take(5) {
            let (rect, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::hover());
            let painter = ui.painter();
            kit::icon(
                painter,
                rect.left_center() + egui::vec2(10.0, 0.0),
                ph::LIGHTNING,
                16.0,
                if a.enabled { theme::OMNI } else { theme::FAINT },
            );
            kit::text_at(
                painter,
                rect.left_center() + egui::vec2(30.0, 0.0),
                Align2::LEFT_CENTER,
                &a.name,
                theme::regular(14.0),
                if a.enabled { theme::TEXT } else { theme::MUTED },
                rect.width() - 36.0,
            );
        }
        ui.add_space(10.0);
        kit::divider(ui);
        ui.add_space(10.0);
        kit::section(ui, "Check-ins", None, None);
        kit::para(
            ui,
            if s.omni.heartbeat_enabled {
                "Omni checks in when something changes, at most twice an hour."
            } else {
                "Scheduled check-ins are off."
            },
            theme::regular(13.0),
            theme::MUTED,
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let on = s.omni.heartbeat_enabled;
            if kit::button_ex(
                ui,
                kit::Kind::Secondary,
                None,
                if on { "Turn off" } else { "Turn on" },
                30.0,
                !self.busy,
            )
            .clicked()
            {
                self.send(Command::Omni(OmniCommand::Heartbeats(!on)));
            }
            if kit::button_ex(
                ui,
                kit::Kind::Omni,
                Some(ph::SPARKLE),
                "Check now",
                30.0,
                !self.busy,
            )
            .clicked()
            {
                self.send(Command::Omni(OmniCommand::CheckNow));
            }
        });
    }
}

fn heading(ui: &mut Ui, title: &str, count: Option<usize>) {
    let (h, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::hover());
    ui.painter().text(
        h.left_center(),
        Align2::LEFT_CENTER,
        title,
        theme::semibold(16.0),
        theme::TEXT,
    );
    if let Some(n) = count {
        ui.painter().text(
            h.right_center() - egui::vec2(6.0, 0.0),
            Align2::RIGHT_CENTER,
            n.to_string(),
            theme::medium(14.0),
            theme::MUTED,
        );
    }
}

/// Memories in the current status/kind/search/entity filter.
fn filtered<'a>(ws: &Workspace, s: &'a Snapshot) -> Vec<&'a MemoryItem> {
    let needle = ws.memory_search.trim().to_lowercase();
    s.memory
        .memories
        .iter()
        .filter(|m| ws.memory_kind_filter.is_none_or(|k| m.kind == k))
        .filter(|m| ws.memory_status_filter.is_none_or(|st| m.status == st))
        .filter(|m| needle.is_empty() || m.content.to_lowercase().contains(&needle))
        .filter(|m| {
            ws.memory_entity_filter.is_none_or(|e| {
                m.entities.contains(&e) || m.source_refs.iter().any(|r| r.entity == e)
            })
        })
        .collect()
}

/// People and conversations memories are about, most-mentioned first.
fn nodes(ws: &Workspace, s: &Snapshot) -> Vec<Node> {
    let mut acc: BTreeMap<String, Node> = BTreeMap::new();
    for m in &s.memory.memories {
        let mut seen = Vec::new();
        for e in m
            .entities
            .iter()
            .chain(m.source_refs.iter().map(|r| &r.entity))
        {
            if seen.contains(e) {
                continue;
            }
            seen.push(*e);
            let (name, person) = match *e {
                EntityId::User(id) => (
                    s.friends
                        .online
                        .iter()
                        .chain(&s.friends.offline)
                        .find(|f| f.user_id == id)
                        .map(|f| f.alias.clone().unwrap_or_else(|| f.display_name.clone())),
                    true,
                ),
                EntityId::Conversation(id) => (
                    s.conversations
                        .conversations
                        .iter()
                        .find(|c| c.conversation_id == id)
                        .map(|c| c.title.clone()),
                    // A DM is about its person; show it with their avatar.
                    s.conversations
                        .conversations
                        .iter()
                        .any(|c| c.conversation_id == id && c.recipient_id.is_some()),
                ),
                _ => (None, false),
            };
            let Some(name) = name else { continue };
            let name = ws.display(&name);
            let node = acc.entry(name.clone()).or_insert(Node {
                entity: *e,
                name,
                count: 0,
                kind: m.kind,
                person,
            });
            node.count += 1;
        }
    }
    let mut v: Vec<Node> = acc.into_values().collect();
    v.sort_by_key(|n| std::cmp::Reverse(n.count));
    v
}

enum Target {
    Conversation(litecord_types::ConversationId),
    Session(i64),
}

fn describe(s: &Snapshot, e: &EntityId) -> (String, Option<Target>) {
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
                Some(Target::Conversation(id)),
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
        EntityId::Local(LocalEntityKind::OmniSession, sid) => {
            ("An Omni chat".into(), Some(Target::Session(sid.0)))
        }
        other => (other.kind_str().replace('_', " "), None),
    }
}

/// Short source description: the first source's conversation title.
fn source_line(s: &Snapshot, m: &MemoryItem) -> String {
    m.source_refs
        .iter()
        .find_map(|r| match r.entity {
            EntityId::Conversation(id) => s
                .conversations
                .conversations
                .iter()
                .find(|c| c.conversation_id == id)
                .map(|c| format!("Conversation with {}", c.title)),
            EntityId::Local(LocalEntityKind::OmniSession, _) => Some("From an Omni chat".into()),
            _ => None,
        })
        .unwrap_or_else(|| {
            format!(
                "{} source{}",
                m.source_refs.len(),
                if m.source_refs.len() == 1 { "" } else { "s" }
            )
        })
}

fn kind_style(kind: MemoryKind) -> (&'static str, kit::Tint) {
    match kind {
        MemoryKind::Commitment => (ph::CHECK_SQUARE, kit::BLUE),
        MemoryKind::PendingReply => (ph::CHAT_CIRCLE_DOTS, kit::TEAL),
        MemoryKind::ImportantDate => (ph::CALENDAR_BLANK, kit::RED),
        MemoryKind::Observation => (ph::EYE, kit::PURPLE),
        MemoryKind::Summary => (ph::FILE_TEXT, kit::GREEN),
        MemoryKind::Preference => (ph::HEART, kit::PINK),
        MemoryKind::Fact => (ph::BRAIN, kit::PURPLE),
        MemoryKind::Note => (ph::NOTE, kit::YELLOW),
        MemoryKind::Operational => (ph::ROBOT, kit::GREY),
    }
}

fn kind_order(kind: MemoryKind) -> u8 {
    match kind {
        MemoryKind::Commitment => 0,
        MemoryKind::PendingReply => 1,
        MemoryKind::ImportantDate => 2,
        MemoryKind::Fact => 3,
        MemoryKind::Preference => 4,
        MemoryKind::Note => 5,
        MemoryKind::Summary => 6,
        MemoryKind::Observation => 7,
        MemoryKind::Operational => 8,
    }
}

fn edge_label(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Commitment => "promised",
        MemoryKind::PendingReply => "waiting",
        MemoryKind::ImportantDate => "dates",
        MemoryKind::Summary => "summaries",
        _ => "noted",
    }
}

fn origin_label(origin: Origin) -> &'static str {
    match origin {
        Origin::DiscordSocialSdk
        | Origin::DiscordUserSession
        | Origin::DiscordBotGateway
        | Origin::Synthetic => "From your messages",
        Origin::UserProvided => "Added by you",
        Origin::LocalApplication => "Found by Litecord",
        Origin::AgentDerived => "Suggested by Omni",
        Origin::Imported => "Imported",
    }
}

fn status_label(status: MemoryStatus) -> &'static str {
    match status {
        MemoryStatus::Candidate => "Needs review",
        MemoryStatus::UserConfirmed => "Confirmed",
        MemoryStatus::Rejected => "Rejected",
        MemoryStatus::Superseded => "Replaced",
        MemoryStatus::Expired => "Expired",
        MemoryStatus::Derived => "Active",
    }
}

fn status_color(status: MemoryStatus) -> Color32 {
    match status {
        MemoryStatus::Candidate => theme::WARNING,
        MemoryStatus::UserConfirmed => theme::SUCCESS,
        MemoryStatus::Rejected | MemoryStatus::Expired => theme::PRIORITY,
        _ => theme::MUTED,
    }
}
