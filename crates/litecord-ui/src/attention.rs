//! Inbox (mock A09): a pill-filtered sidebar of replies owed and
//! suggestions; the main view with a Today card, Omni assistance cards,
//! approvals (always showing the acting identity and the exact payload),
//! Omni's local-command requests, check-ins and the attention list.

use eframe::egui::{self, Align2, Rect, Ui};
use litecord_app::view::{InboxItem, PendingActionRow};
use litecord_layout::Destination;
use litecord_types::{
    actions::{ActionStatus, CapabilityClass},
    memory::MemoryStatus,
    provenance::{DiscordIdentity, Origin},
    Timestamp,
};

use crate::bridge::{Command, OmniCommand, Snapshot};
use crate::context_ui::{visible_attention, INBOX_FILTERS};
use crate::messages_ui::list_time;
use crate::workspace::Workspace;
use crate::{kit, ph, theme};

impl Workspace {
    // ---- Sidebar ---------------------------------------------------------------

    pub(crate) fn inbox_sidebar(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        if kit::sidebar_title(
            ui,
            "Inbox",
            Some((ph::SPARKLE, "Ask Omni what needs attention")),
        ) {
            self.omni_open = true;
            self.omni_draft = "What needs my attention first?".into();
        }
        ui.add_space(6.0);
        kit::search(ui, &mut self.filter, "Search inbox...");
        ui.add_space(10.0);
        let counts = inbox_counts(&s);
        let items: Vec<(&str, Option<usize>)> = INBOX_FILTERS
            .iter()
            .enumerate()
            .map(|(i, l)| (*l, (i > 0).then_some(counts[i])))
            .collect();
        kit::pill_row(ui, &items, &mut self.inbox_filter);
        ui.add_space(6.0);
        let filter = self.filter.to_lowercase();
        let replies: Vec<(litecord_types::ConversationId, String, String, Timestamp)> = s
            .inbox
            .needs_attention
            .iter()
            .filter_map(|i| match i {
                InboxItem::PendingReply {
                    conversation_id,
                    from,
                    preview,
                    at,
                } => Some((*conversation_id, from.clone(), preview.clone(), *at)),
                _ => None,
            })
            .filter(|(_, f, p, _)| {
                filter.is_empty()
                    || f.to_lowercase().contains(&filter)
                    || p.to_lowercase().contains(&filter)
            })
            .collect();
        egui::ScrollArea::vertical()
            .id_salt("inbox_sidebar")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                sidebar_heading(ui, "Needs reply", Some(replies.len()));
                if replies.is_empty() {
                    kit::label(
                        ui,
                        "You're all caught up.",
                        theme::regular(13.5),
                        theme::MUTED,
                    );
                }
                let now = Timestamp::now();
                let oldest = replies.iter().map(|r| r.3.as_millis()).min();
                for (conv, from, preview, at) in &replies {
                    let (rect, resp) = kit::row(ui, 64.0, false);
                    let name = self.display(from);
                    let painter = ui.painter();
                    // The oldest reply owed is marked high priority (A09's red calendar).
                    if Some(at.as_millis()) == oldest && replies.len() > 1 {
                        kit::icon(
                            painter,
                            egui::pos2(rect.left() + 8.0, rect.center().y),
                            ph::CALENDAR_BLANK,
                            13.0,
                            theme::PRIORITY,
                        );
                    } else {
                        kit::dot(
                            painter,
                            egui::pos2(rect.left() + 8.0, rect.center().y),
                            4.0,
                            theme::PRIMARY,
                        );
                    }
                    theme::paint_avatar(
                        painter,
                        egui::pos2(rect.left() + 42.0, rect.center().y),
                        42.0,
                        &name,
                        theme::Presence::None,
                        theme::SIDEBAR,
                    );
                    let x = rect.left() + 72.0;
                    let t = list_time(*at, now);
                    let tw = kit::text_width(painter, &t, theme::regular(12.5));
                    kit::text_at(
                        painter,
                        egui::pos2(x, rect.center().y - 10.0),
                        Align2::LEFT_CENTER,
                        &name,
                        theme::medium(15.0),
                        theme::TEXT,
                        rect.right() - x - tw - 16.0,
                    );
                    painter.text(
                        egui::pos2(rect.right() - 8.0, rect.center().y - 10.0),
                        Align2::RIGHT_CENTER,
                        t,
                        theme::regular(12.5),
                        theme::MUTED,
                    );
                    kit::text_at(
                        painter,
                        egui::pos2(x, rect.center().y + 11.0),
                        Align2::LEFT_CENTER,
                        &self.display(preview),
                        theme::regular(13.5),
                        theme::SECONDARY,
                        rect.right() - x - 12.0,
                    );
                    if resp.clicked() {
                        self.open_conversation(*conv);
                    }
                }
                let suggestions: Vec<String> = visible_attention(&s.inbox.needs_attention)
                    .into_iter()
                    .filter_map(|i| match i {
                        InboxItem::TaskCandidate { title, .. } => Some(title.clone()),
                        InboxItem::Commitment { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect();
                if !suggestions.is_empty() {
                    ui.add_space(10.0);
                    sidebar_heading(ui, "Suggestions", Some(suggestions.len()));
                    for t in suggestions.iter().take(6) {
                        let (rect, resp) = kit::row(ui, 48.0, false);
                        let painter = ui.painter();
                        kit::paint_bubble(
                            painter,
                            egui::pos2(rect.left() + 26.0, rect.center().y),
                            ph::CHECK_SQUARE,
                            kit::BLUE,
                            32.0,
                        );
                        kit::text_at(
                            painter,
                            egui::pos2(rect.left() + 52.0, rect.center().y),
                            Align2::LEFT_CENTER,
                            &self.display(t),
                            theme::regular(14.0),
                            theme::lerp(theme::TEXT, theme::SECONDARY, 0.25),
                            rect.width() - 60.0,
                        );
                        if resp.clicked() {
                            self.navigate(Destination::Tasks);
                        }
                    }
                }
                ui.add_space(12.0);
                sidebar_heading(ui, "Check-ins", None);
                let on = s.omni.heartbeat_enabled;
                kit::para(
                    ui,
                    if on {
                        "Omni checks in on its own when something changes."
                    } else {
                        "Scheduled check-ins are off."
                    },
                    theme::regular(13.0),
                    theme::MUTED,
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
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
            });
    }

    // ---- Main --------------------------------------------------------------------

    pub fn inbox_screen(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            ui.label("Loading inbox…");
            return;
        };
        let privacy = self.private();
        egui::ScrollArea::vertical()
            .id_salt("inbox_screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                kit::page_title(
                    ui,
                    "Inbox",
                    Some("Stay on top of the conversations and follow-ups that matter."),
                );
                self.inbox_today(ui, &s);
                ui.add_space(6.0);
                kit::section(ui, "Omni inbox assistance", None, None);
                self.inbox_assistance(ui, &s);
                ui.add_space(6.0);
                let filter = self.inbox_filter;
                let show = |f: usize| filter == 0 || filter == f;
                if show(2) && !s.omni.requests.is_empty() {
                    kit::section(
                        ui,
                        "Omni wants to run on this computer",
                        Some(s.omni.requests.len()),
                        None,
                    );
                    for req in s.omni.requests.clone() {
                        self.inbox_omni_request(ui, &req);
                    }
                    ui.add_space(6.0);
                }
                if show(2) && !s.inbox.pending_actions.is_empty() {
                    kit::section(
                        ui,
                        "Waiting for your approval",
                        Some(s.inbox.pending_actions.len()),
                        None,
                    );
                    for action in &s.inbox.pending_actions {
                        self.pending_action_card(ui, action, privacy);
                        ui.add_space(4.0);
                    }
                    ui.add_space(6.0);
                }
                if show(3) && !s.omni.checkins.is_empty() {
                    ui.horizontal(|ui| {
                        kit::label(ui, "From Omni", theme::semibold(17.0), theme::TEXT);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if kit::button_ex(
                                ui,
                                kit::Kind::Ghost,
                                None,
                                "Dismiss all",
                                28.0,
                                !self.busy,
                            )
                            .clicked()
                            {
                                self.send(Command::Omni(OmniCommand::DismissCheckins));
                            }
                        });
                    });
                    for c in s.omni.checkins.clone() {
                        self.checkin_card(ui, &c);
                    }
                    ui.add_space(6.0);
                }
                self.inbox_activity(ui, &s);
                ui.add_space(12.0);
            });
    }

    /// A09 "Today": what's due and who is waiting, with details.
    fn inbox_today(&mut self, ui: &mut Ui, s: &Snapshot) {
        let now = Timestamp::now();
        let end_of_day = (now.as_millis() / 86_400_000 + 1) * 86_400_000;
        let replies: Vec<String> = s
            .inbox
            .needs_attention
            .iter()
            .filter_map(|i| match i {
                InboxItem::PendingReply { from, .. } => Some(self.display(from)),
                _ => None,
            })
            .collect();
        let due: Vec<String> = s
            .tasks
            .open
            .iter()
            .filter(|t| t.due_at.is_some_and(|d| d.as_millis() < end_of_day))
            .map(|t| self.display(&t.title))
            .collect();
        let reminders: Vec<String> = s
            .tasks
            .reminders
            .iter()
            .filter(|r| r.trigger.due_at().as_millis() < end_of_day)
            .map(|r| self.display(&r.title))
            .collect();
        let join = |v: &[String], empty: &str| {
            if v.is_empty() {
                empty.to_owned()
            } else {
                v.iter().take(3).cloned().collect::<Vec<_>>().join(" · ")
            }
        };
        let rows = [
            (
                ph::CALENDAR_BLANK,
                theme::PRIORITY,
                format!("{} reminder{}", reminders.len(), plural(reminders.len())),
                join(&reminders, "Nothing scheduled"),
                0u8,
            ),
            (
                ph::CHECK_SQUARE,
                theme::SUCCESS,
                format!("{} task{} due", due.len(), plural(due.len())),
                join(&due, "No tasks due today"),
                1,
            ),
            (
                ph::CHAT_CIRCLE,
                theme::PRIMARY,
                format!("{} unread message{}", replies.len(), plural(replies.len())),
                if replies.is_empty() {
                    "Nobody is waiting on you".to_owned()
                } else {
                    format!("From {}", join(&replies, ""))
                },
                2,
            ),
        ];
        let mut go = None;
        kit::card(ui, |ui| {
            let (h, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 26.0), egui::Sense::hover());
            ui.painter().text(
                h.left_center(),
                Align2::LEFT_CENTER,
                "Today",
                theme::semibold(17.0),
                theme::TEXT,
            );
            let date = crate::messages_ui::day_label(now);
            let short = date
                .rsplit_once(' ')
                .map_or(date.as_str(), |(d, _)| d)
                .to_owned();
            ui.painter().text(
                h.right_center(),
                Align2::RIGHT_CENTER,
                short,
                theme::regular(13.0),
                theme::MUTED,
            );
            ui.add_space(2.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, (g, c, title, detail, id)) in rows.iter().enumerate() {
                if i > 0 {
                    kit::divider(ui);
                }
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 46.0),
                    egui::Sense::click(),
                );
                if resp.hovered() {
                    ui.painter().rect_filled(
                        rect,
                        8.0,
                        theme::lerp(theme::CARD, theme::HOVER, 0.6),
                    );
                }
                let painter = ui.painter();
                kit::icon(
                    painter,
                    rect.left_center() + egui::vec2(16.0, 0.0),
                    g,
                    22.0,
                    *c,
                );
                kit::text_at(
                    painter,
                    rect.left_center() + egui::vec2(44.0, 0.0),
                    Align2::LEFT_CENTER,
                    title,
                    theme::medium(15.0),
                    theme::TEXT,
                    170.0,
                );
                let dx = rect.left() + 44.0 + 180.0;
                kit::text_at(
                    painter,
                    egui::pos2(dx, rect.center().y),
                    Align2::LEFT_CENTER,
                    detail,
                    theme::regular(13.5),
                    theme::MUTED,
                    (rect.right() - dx - 30.0).max(30.0),
                );
                kit::chevron(
                    painter,
                    rect.right_center() - egui::vec2(12.0, 0.0),
                    theme::MUTED,
                );
                if resp.clicked() {
                    go = Some(*id);
                }
            }
        });
        match go {
            Some(2) => self.inbox_filter = 1,
            Some(_) => self.navigate(Destination::Tasks),
            None => {}
        }
    }

    /// Four assistance cards: approvals, a red follow-up, memories, catch-up.
    fn inbox_assistance(&mut self, ui: &mut Ui, s: &Snapshot) {
        let approvals = s.inbox.pending_actions.len() + s.omni.requests.len();
        let first_reply = s.inbox.needs_attention.iter().find_map(|i| match i {
            InboxItem::PendingReply {
                conversation_id,
                from,
                ..
            } => Some((*conversation_id, self.display(from))),
            _ => None,
        });
        let memories = crate::home_ui::candidates(s);
        struct Card {
            glyph: &'static str,
            red: bool,
            title: String,
            body: String,
            button: &'static str,
            enabled: bool,
        }
        let cards = [
            Card {
                glyph: ph::FILE_TEXT,
                red: false,
                title: format!("{approvals} waiting for approval"),
                body: "Messages and actions proposed by Omni or you.".into(),
                button: "Review",
                enabled: approvals > 0,
            },
            Card {
                glyph: ph::CALENDAR_BLANK,
                red: true,
                title: "Follow-up suggestion".into(),
                body: first_reply.as_ref().map_or_else(
                    || "Nobody is waiting on a reply.".to_owned(),
                    |(_, f)| format!("{f} is waiting on your response."),
                ),
                button: "Add reminder",
                enabled: first_reply.is_some() && !self.busy,
            },
            Card {
                glyph: ph::BRAIN,
                red: false,
                title: format!(
                    "{memories} new memor{} added",
                    if memories == 1 { "y" } else { "ies" }
                ),
                body: "Facts and commitments picked up from your messages.".into(),
                button: "View in Omni",
                enabled: true,
            },
            Card {
                glyph: ph::CHAT_CIRCLE_TEXT,
                red: false,
                title: "Conversation summary".into(),
                body: "Ask Omni what changed today and what to do first.".into(),
                button: "Ask Omni",
                enabled: true,
            },
        ];
        let mut clicked = None;
        kit::card_grid(ui, 4, 176.0, 12.0, 170.0, 4, |ui, i, rect| {
            let c = &cards[i];
            let accent = if c.red { theme::PRIORITY } else { theme::OMNI };
            kit::card_at(ui, rect, ui.id().with(("assist", i)), Some(accent));
            let painter = ui.painter();
            kit::paint_bubble(
                painter,
                egui::pos2(rect.left() + 32.0, rect.top() + 34.0),
                c.glyph,
                if c.red { kit::RED } else { kit::TEAL },
                46.0,
            );
            let x = rect.left() + 64.0;
            let w = rect.right() - x - 10.0;
            let g = kit::wrapped(painter, &c.title, theme::medium(14.0), theme::TEXT, w, 2);
            let th = g.size().y;
            painter.galley(egui::pos2(x, rect.top() + 16.0), g, theme::TEXT);
            let b = kit::wrapped(painter, &c.body, theme::regular(13.0), theme::MUTED, w, 3);
            painter.galley(egui::pos2(x, rect.top() + 20.0 + th), b, theme::MUTED);
            let bw = (rect.width() - 56.0).min(150.0);
            let br = Rect::from_center_size(
                egui::pos2(rect.center().x, rect.bottom() - 28.0),
                egui::vec2(bw, 32.0),
            );
            if kit::button_at(
                ui,
                br,
                ui.id().with(("assist_btn", i)),
                if c.red {
                    kit::Kind::Danger
                } else {
                    kit::Kind::Omni
                },
                None,
                c.button,
                c.enabled,
            )
            .clicked()
            {
                clicked = Some(i);
            }
        });
        match clicked {
            Some(0) => self.inbox_filter = 2,
            Some(1) => {
                if let Some((conv, from)) = first_reply {
                    self.send(Command::CreateTask(litecord_types::tasks::TaskDraft {
                        title: format!("Reply to {from}"),
                        description: None,
                        priority: litecord_types::tasks::TaskPriority::High,
                        due_at: Some(Timestamp::from_millis(
                            Timestamp::now().as_millis() + 3 * 3_600_000,
                        )),
                        related_users: Vec::new(),
                        conversation_id: Some(conv),
                        parent_id: None,
                        source: None,
                    }));
                }
            }
            Some(2) => {
                self.memory_status_filter = Some(MemoryStatus::Candidate);
                self.navigate(Destination::Omni);
            }
            Some(_) => {
                self.omni_open = true;
                self.omni_draft = "What changed today, and what needs my attention first?".into();
            }
            None => {}
        }
    }

    /// Replies, reminders and suggestions as an activity list with chips.
    fn inbox_activity(&mut self, ui: &mut Ui, s: &Snapshot) {
        let (title_rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::hover());
        ui.painter().text(
            title_rect.left_center(),
            Align2::LEFT_CENTER,
            "Needs attention",
            theme::semibold(17.0),
            theme::TEXT,
        );
        let chips_w: f32 = INBOX_FILTERS
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
            kit::chip_row(&mut cui, &INBOX_FILTERS, &mut self.inbox_filter);
        }
        ui.add_space(4.0);
        let filter = self.inbox_filter;
        let show = |f: usize| filter == 0 || filter == f;
        let items: Vec<&InboxItem> = visible_attention(&s.inbox.needs_attention)
            .into_iter()
            .filter(|i| match i {
                InboxItem::PendingReply { .. } | InboxItem::ReminderDue { .. } => show(1),
                InboxItem::TaskCandidate { .. } | InboxItem::Commitment { .. } => show(4),
            })
            .collect();
        if items.is_empty() {
            kit::empty(
                ui,
                Some(ph::CHECK_CIRCLE),
                "All clear",
                "Nothing here needs your attention right now.",
            );
            return;
        }
        let now = Timestamp::now();
        let mut act: Option<usize> = None;
        egui::Frame::new()
            .fill(theme::CARD)
            .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        kit::divider(ui);
                    }
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 58.0),
                        egui::Sense::click(),
                    );
                    if resp.hovered() {
                        ui.painter().rect_filled(
                            rect,
                            8.0,
                            theme::lerp(theme::CARD, theme::HOVER, 0.6),
                        );
                    }
                    let row = attention_line(self, item);
                    let painter = ui.painter();
                    kit::dot(
                        painter,
                        egui::pos2(rect.left() + 12.0, rect.center().y),
                        4.0,
                        theme::PRIMARY,
                    );
                    let c = egui::pos2(rect.left() + 44.0, rect.center().y);
                    match row.lead {
                        Lead::Person => theme::paint_avatar(
                            painter,
                            c,
                            38.0,
                            &row.who,
                            theme::Presence::None,
                            theme::CARD,
                        ),
                        Lead::Glyph(g, t) => kit::paint_bubble(painter, c, g, t, 38.0),
                    }
                    let bw = kit::button_width(painter, None, row.button, 30.0);
                    let br = Rect::from_min_size(
                        egui::pos2(rect.right() - bw - 12.0, rect.center().y - 15.0),
                        egui::vec2(bw, 30.0),
                    );
                    let x = rect.left() + 74.0;
                    let avail = br.left() - x - 16.0;
                    let who_r = kit::text_at(
                        painter,
                        egui::pos2(x, rect.center().y - 10.0),
                        Align2::LEFT_CENTER,
                        &row.who,
                        theme::medium(14.5),
                        theme::TEXT,
                        (avail * 0.6).max(60.0),
                    );
                    let verb_r = kit::text_at(
                        painter,
                        egui::pos2(who_r.right() + 6.0, rect.center().y - 10.0),
                        Align2::LEFT_CENTER,
                        row.verb,
                        theme::regular(13.0),
                        theme::MUTED,
                        (br.left() - who_r.right() - 90.0).max(20.0),
                    );
                    if let Some(at) = row.at {
                        painter.text(
                            egui::pos2(verb_r.right() + 8.0, rect.center().y - 10.0),
                            Align2::LEFT_CENTER,
                            list_time(at, now),
                            theme::regular(12.5),
                            theme::FAINT,
                        );
                    }
                    kit::text_at(
                        painter,
                        egui::pos2(x, rect.center().y + 11.0),
                        Align2::LEFT_CENTER,
                        &row.text,
                        theme::regular(13.5),
                        theme::SECONDARY,
                        avail,
                    );
                    if kit::button_at(
                        ui,
                        br,
                        ui.id().with(("attn_btn", i)),
                        row.kind,
                        None,
                        row.button,
                        true,
                    )
                    .clicked()
                        || resp.clicked()
                    {
                        act = Some(i);
                    }
                }
            });
        if let Some(i) = act {
            match items[i] {
                InboxItem::PendingReply {
                    conversation_id, ..
                } => self.open_conversation(*conversation_id),
                InboxItem::ReminderDue { .. } | InboxItem::TaskCandidate { .. } => {
                    self.navigate(Destination::Tasks)
                }
                InboxItem::Commitment { .. } => self.navigate(Destination::Omni),
            }
        }
    }

    fn checkin_card(&mut self, ui: &mut Ui, c: &litecord_app::OmniCheckin) {
        let (fill, stroke) = kit::accent_colors(theme::OMNI);
        egui::Frame::new()
            .fill(fill)
            .stroke(egui::Stroke::new(1.0_f32, stroke))
            .corner_radius(12)
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    kit::bubble(ui, ph::SPARKLE, kit::TEAL, 34.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        kit::label(ui, &c.source, theme::medium(13.0), theme::OMNI_TEXT);
                        kit::para(ui, self.display(&c.text), theme::regular(14.5), theme::TEXT);
                    });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if kit::button_ex(
                        ui,
                        kit::Kind::Omni,
                        Some(ph::CHAT_CIRCLE),
                        "Continue in Omni",
                        30.0,
                        true,
                    )
                    .clicked()
                    {
                        self.selection.omni_session = Some(c.session_id);
                        self.omni_open = true;
                        self.request();
                    }
                    if kit::button_ex(
                        ui,
                        kit::Kind::Secondary,
                        Some(ph::BOOKMARK_SIMPLE),
                        "Remember",
                        30.0,
                        !self.busy,
                    )
                    .clicked()
                    {
                        self.send(Command::Omni(OmniCommand::Remember(c.session_id, c.seq)));
                    }
                });
            });
        ui.add_space(4.0);
    }

    fn inbox_omni_request(&mut self, ui: &mut Ui, req: &litecord_app::omni::OmniRequestRow) {
        use litecord_app::harness::{Decision, RequestKind};
        let (glyph, detail) = match &req.request {
            RequestKind::Command { command, .. } => {
                (ph::TERMINAL_WINDOW, format!("Run: {command}"))
            }
            RequestKind::FileChange { summary } => {
                (ph::FILE_TEXT, format!("Change files: {summary}"))
            }
            RequestKind::Permission { title } => (ph::SHIELD, title.clone()),
        };
        let (fill, stroke) = kit::accent_colors(theme::WARNING);
        egui::Frame::new()
            .fill(fill)
            .stroke(egui::Stroke::new(1.0_f32, stroke))
            .corner_radius(12)
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    kit::bubble(ui, glyph, kit::YELLOW, 34.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        kit::status_pill(
                            ui,
                            Some(ph::DESKTOP),
                            "Local · this computer",
                            theme::WARNING,
                        );
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(detail).monospace().color(theme::TEXT),
                            )
                            .wrap(),
                        );
                        if let Some(reason) = &req.reason {
                            kit::para(ui, reason.as_str(), theme::regular(13.0), theme::MUTED);
                        }
                    });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let answer = |d| Command::Omni(OmniCommand::Answer(req.id.clone(), d));
                    if kit::button_ex(
                        ui,
                        kit::Kind::Primary,
                        Some(ph::CHECK),
                        "Allow once",
                        30.0,
                        !self.busy,
                    )
                    .clicked()
                    {
                        self.send(answer(Decision::Accept));
                    }
                    if kit::button_ex(
                        ui,
                        kit::Kind::Danger,
                        Some(ph::X),
                        "Decline",
                        30.0,
                        !self.busy,
                    )
                    .clicked()
                    {
                        self.send(answer(Decision::Decline));
                    }
                    if kit::button_ex(ui, kit::Kind::Ghost, None, "View chat", 30.0, true).clicked()
                    {
                        self.selection.omni_session = Some(req.session_id);
                        self.omni_open = true;
                        self.request();
                    }
                });
            });
        ui.add_space(4.0);
    }

    /// An approval card. The acting identity and the exact payload are
    /// always visible; approval is disabled when either can't be shown.
    fn pending_action_card(&mut self, ui: &mut Ui, action: &PendingActionRow, privacy: bool) {
        let summary = self.display(&action.summary);
        let actor = self.display(&action.actor);
        let target = action
            .target_label
            .as_deref()
            .map(|value| self.display(value));
        let rationale = action.rationale.as_deref().map(|value| self.display(value));
        let content = action.content.as_deref().map(|value| self.display(value));
        let status = action.status.as_str().replace('_', " ");
        let kind = action.kind.replace('_', " ");
        let is_pending = action.status == ActionStatus::PendingApproval;
        // The review snapshot carries a complete target and body only for
        // message sends. Incomplete payloads remain rejectable, never approvable.
        let payload_reviewable = action.kind == "send_message"
            && target.as_deref().is_some_and(|value| !value.is_empty())
            && content.as_deref().is_some_and(|value| !value.is_empty());
        let can_approve = !self.busy && is_pending && !privacy && payload_reviewable;
        let can_reject = !self.busy && is_pending;
        let mut decision = None;
        let action_id = action.action_id;
        let discord_write = matches!(
            action.class,
            CapabilityClass::DiscordWrite | CapabilityClass::Administrative
        );
        egui::Frame::new()
            .fill(theme::CARD)
            .stroke(egui::Stroke::new(
                1.0_f32,
                if discord_write {
                    theme::lerp(theme::BORDER, theme::PRIORITY, 0.35)
                } else {
                    theme::BORDER
                },
            ))
            .corner_radius(12)
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    kit::bubble(
                        ui,
                        if discord_write { ph::PAPER_PLANE_TILT } else { ph::CHECK_SQUARE },
                        if discord_write { kit::BLUE } else { kit::TEAL },
                        38.0,
                    );
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        kit::para(ui, &summary, theme::medium(15.0), theme::TEXT);
                        ui.horizontal_wrapped(|ui| {
                            match action.identity {
                                DiscordIdentity::ApplicationBot => {
                                    kit::status_pill(ui, Some(ph::ROBOT), "Acts as your bot", theme::WARNING)
                                }
                                DiscordIdentity::UserSocialSdk => {
                                    kit::status_pill(ui, Some(ph::USER), "Acts as you", theme::PRIMARY)
                                }
                                DiscordIdentity::UserSession => kit::status_pill(
                                    ui,
                                    Some(ph::LOCK_SIMPLE),
                                    "User session (read only)",
                                    theme::PRIMARY,
                                ),
                            };
                            kit::status_pill(ui, None, &class_label(action.class), action_class_color(action.class));
                            kit::status_pill(ui, None, &status, theme::MUTED);
                        });
                    });
                });
                ui.add_space(4.0);
                kit::label(ui, format!("Requested by {actor} · {kind}"), theme::regular(13.0), theme::SECONDARY);
                if let Some(target) = &target {
                    kit::label(ui, format!("Target: {target}"), theme::regular(13.0), theme::SECONDARY);
                }
                if let Some(rationale) = &rationale {
                    kit::para(ui, format!("Reason: {rationale}"), theme::regular(13.0), theme::MUTED);
                }
                ui.add_space(4.0);
                kit::label(ui, "Full action content", theme::medium(13.0), theme::SECONDARY);
                egui::Frame::new()
                    .fill(theme::FIELD)
                    .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match &content {
                            Some(content) => kit::para(ui, content, theme::regular(14.5), theme::TEXT),
                            None => kit::para(
                                ui,
                                "The exact action content is not available in this review snapshot.",
                                theme::regular(14.0),
                                theme::PRIORITY_TEXT,
                            ),
                        };
                    });
                if privacy {
                    kit::para(
                        ui,
                        "Privacy Mode hides the exact payload, so approval is disabled.",
                        theme::regular(12.5),
                        theme::PRIORITY_TEXT,
                    );
                } else if !payload_reviewable && is_pending {
                    kit::para(
                        ui,
                        "This snapshot omits part of the exact target or payload, so approval is disabled.",
                        theme::regular(12.5),
                        theme::MUTED,
                    );
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if kit::button_ex(ui, kit::Kind::Primary, Some(ph::CHECK), "Approve", 32.0, can_approve)
                        .clicked()
                    {
                        decision = Some(true);
                    }
                    if kit::button_ex(ui, kit::Kind::Danger, Some(ph::X), "Reject", 32.0, can_reject).clicked() {
                        decision = Some(false);
                    }
                });
            });
        if let Some(approve) = decision {
            self.send(if approve {
                Command::Approve(action_id)
            } else {
                Command::Reject(action_id)
            });
        }
    }

    // ---- Inspector -------------------------------------------------------------

    pub(crate) fn inbox_inspector(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("inbox_inspector")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.account_block(ui, &s);
                ui.add_space(12.0);
                self.inbox_activity_card(ui, &s);
                ui.add_space(10.0);
                self.online_now_card(ui, &s);
                ui.add_space(12.0);
                self.quick_actions(ui);
            });
    }

    /// A09 inspector "Activity": the latest events with times.
    fn inbox_activity_card(&mut self, ui: &mut Ui, s: &Snapshot) {
        let now = Timestamp::now();
        let mut rows: Vec<(
            &str,
            egui::Color32,
            String,
            egui::Color32,
            Option<Timestamp>,
        )> = Vec::new();
        for i in s.inbox.needs_attention.iter().take(5) {
            match i {
                InboxItem::PendingReply { from, at, .. } => rows.push((
                    ph::CALENDAR_BLANK,
                    theme::PRIORITY,
                    format!("Waiting for reply: {}", self.display(from)),
                    theme::PRIORITY_TEXT,
                    Some(*at),
                )),
                InboxItem::ReminderDue { title, due_at, .. } => rows.push((
                    ph::CLOCK,
                    theme::WARNING,
                    self.display(title),
                    theme::TEXT,
                    Some(*due_at),
                )),
                InboxItem::TaskCandidate { title, .. } => rows.push((
                    ph::FILE_TEXT,
                    theme::OMNI,
                    format!("Suggested: {}", self.display(title)),
                    theme::TEXT,
                    None,
                )),
                InboxItem::Commitment { text, .. } => rows.push((
                    ph::BRAIN,
                    theme::OMNI,
                    self.display(text),
                    theme::TEXT,
                    None,
                )),
            }
        }
        kit::card(ui, |ui| {
            kit::section(ui, "Activity", None, None);
            if rows.is_empty() {
                kit::label(ui, "Nothing new.", theme::regular(13.5), theme::MUTED);
            }
            for (g, c, t, tc, at) in rows {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 28.0),
                    egui::Sense::hover(),
                );
                let painter = ui.painter();
                kit::icon(
                    painter,
                    rect.left_center() + egui::vec2(10.0, 0.0),
                    g,
                    16.0,
                    c,
                );
                let time = at.map(|t| list_time(t, now)).unwrap_or_default();
                let tw = kit::text_width(painter, &time, theme::regular(12.5));
                kit::text_at(
                    painter,
                    rect.left_center() + egui::vec2(30.0, 0.0),
                    Align2::LEFT_CENTER,
                    &t,
                    theme::regular(13.5),
                    tc,
                    rect.width() - 40.0 - tw,
                );
                painter.text(
                    rect.right_center(),
                    Align2::RIGHT_CENTER,
                    time,
                    theme::regular(12.5),
                    theme::MUTED,
                );
            }
        });
    }
}

enum Lead {
    Person,
    Glyph(&'static str, kit::Tint),
}

struct AttentionLine {
    who: String,
    verb: &'static str,
    at: Option<Timestamp>,
    text: String,
    button: &'static str,
    kind: kit::Kind,
    lead: Lead,
}

fn attention_line(ws: &Workspace, item: &InboxItem) -> AttentionLine {
    match item {
        InboxItem::PendingReply {
            from, preview, at, ..
        } => AttentionLine {
            who: ws.display(from),
            verb: "sent you a message",
            at: Some(*at),
            text: ws.display(preview),
            button: "Reply",
            kind: kit::Kind::Secondary,
            lead: Lead::Person,
        },
        InboxItem::ReminderDue { title, due_at, .. } => AttentionLine {
            who: ws.display(title),
            verb: "reminder due",
            at: Some(*due_at),
            text: "A saved reminder needs attention.".into(),
            button: "Open tasks",
            kind: kit::Kind::Danger,
            lead: Lead::Glyph(ph::CALENDAR_BLANK, kit::RED),
        },
        InboxItem::TaskCandidate { title, origin, .. } => AttentionLine {
            who: ws.display(title),
            verb: origin_label(*origin),
            at: None,
            text: "Confirm it to add it to your tasks.".into(),
            button: "Review",
            kind: kit::Kind::Omni,
            lead: Lead::Glyph(ph::CHECK_SQUARE, kit::TEAL),
        },
        InboxItem::Commitment { text, due_at, .. } => AttentionLine {
            who: ws.display(text),
            verb: "commitment",
            at: *due_at,
            text: "Saved in Omni's memory.".into(),
            button: "Open Omni",
            kind: kit::Kind::Omni,
            lead: Lead::Glyph(ph::BRAIN, kit::TEAL),
        },
    }
}

fn sidebar_heading(ui: &mut Ui, title: &str, count: Option<usize>) {
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

/// Sidebar/pill counts: [all, replies, approvals, omni, suggestions].
fn inbox_counts(s: &Snapshot) -> [usize; 5] {
    let items = visible_attention(&s.inbox.needs_attention);
    let replies = items
        .iter()
        .filter(|i| {
            matches!(
                i,
                InboxItem::PendingReply { .. } | InboxItem::ReminderDue { .. }
            )
        })
        .count();
    let suggestions = items.len() - replies;
    [
        items.len() + s.inbox.pending_actions.len() + s.omni.checkins.len() + s.omni.requests.len(),
        replies,
        s.inbox.pending_actions.len() + s.omni.requests.len(),
        s.omni.checkins.len(),
        suggestions,
    ]
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn origin_label(origin: Origin) -> &'static str {
    match origin {
        Origin::DiscordSocialSdk | Origin::DiscordUserSession | Origin::DiscordBotGateway => {
            "observed from Discord"
        }
        Origin::UserProvided => "added by you",
        Origin::LocalApplication => "suggested task",
        Origin::AgentDerived => "from Omni",
        Origin::Imported => "imported suggestion",
        Origin::Synthetic => "synthetic demo item",
    }
}

fn class_label(class: CapabilityClass) -> String {
    match class {
        CapabilityClass::Read => "Read only".into(),
        CapabilityClass::LocalWrite => "Local change".into(),
        CapabilityClass::DiscordWrite => "Discord change".into(),
        CapabilityClass::Administrative => "Administrative".into(),
    }
}

fn action_class_color(class: CapabilityClass) -> egui::Color32 {
    match class {
        CapabilityClass::Read => theme::MUTED,
        CapabilityClass::LocalWrite => theme::OMNI,
        CapabilityClass::DiscordWrite | CapabilityClass::Administrative => theme::PRIORITY,
    }
}
