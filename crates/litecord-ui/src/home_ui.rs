//! Home (mock A02): greeting, metric cards, pick-up cards, Omni cards,
//! online friends, server activity and recent activity; a favorites/recent
//! sidebar; and a profile / Today / Omni / quick-actions inspector.
//!
//! Everything shown comes from the snapshot. Where the mock shows data
//! Litecord does not have (meetings, member counts), the card says what it
//! can show instead of inventing numbers.

use eframe::egui::{self, Align2, Rect, Ui};
use litecord_app::view::{ConversationRow, InboxItem};
use litecord_layout::Destination;
use litecord_types::memory::MemoryStatus;
use litecord_types::social::ConversationKind;
use litecord_types::Timestamp;

use crate::bridge::{Command, Snapshot};
use crate::messages_ui::{list_time, local_hour};
use crate::workspace::Workspace;
use crate::{kit, ph, theme};

const GAP: f32 = 12.0;

impl Workspace {
    pub fn home_screen(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("home_screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let first = self
                    .display(&s.account.display_name)
                    .split_whitespace()
                    .next()
                    .unwrap_or("there")
                    .to_owned();
                let hour = local_hour(Timestamp::now());
                let greeting = match hour {
                    5..=11 => "Good morning",
                    12..=17 => "Good afternoon",
                    _ => "Good evening",
                };
                kit::page_title(ui, "Home", Some(&format!("{greeting}, {first}.")));
                self.home_metrics(ui, &s);
                ui.add_space(6.0);
                if kit::section(ui, "Pick up where you left off", None, Some("See all")) {
                    self.navigate(Destination::Messages);
                }
                self.home_pickup(ui, &s);
                ui.add_space(6.0);
                if kit::section(ui, "Omni", None, Some("See all")) {
                    self.navigate(Destination::Omni);
                }
                self.home_omni_cards(ui, &s);
                ui.add_space(6.0);
                if kit::section(ui, "Online friends", None, Some("See all")) {
                    self.friends_tab = 0;
                    self.navigate(Destination::Friends);
                }
                self.home_online(ui, &s);
                if !s.guilds.guilds.is_empty() {
                    ui.add_space(6.0);
                    if kit::section(ui, "Server activity", None, Some("See all")) {
                        self.navigate(Destination::Servers);
                    }
                    self.home_servers(ui, &s);
                }
                ui.add_space(6.0);
                self.home_activity(ui, &s);
                ui.add_space(12.0);
            });
    }

    fn home_metrics(&mut self, ui: &mut Ui, s: &Snapshot) {
        let unread = s
            .conversations
            .conversations
            .iter()
            .filter(|r| r.awaiting_reply)
            .count();
        let omni_items = s.omni.requests.len()
            + s.omni.checkins.len()
            + candidates(s)
            + s.inbox.pending_actions.len();
        let cards: [(&str, kit::Tint, String, &str, Destination); 4] = [
            (
                ph::CHAT_CIRCLE,
                kit::BLUE,
                unread.to_string(),
                "Unread messages",
                Destination::Messages,
            ),
            (
                ph::USERS,
                kit::GREEN,
                s.friends.online.len().to_string(),
                "Online friends",
                Destination::Friends,
            ),
            (
                ph::SQUARES_FOUR,
                kit::PURPLE,
                s.guilds.guilds.len().to_string(),
                "Active servers",
                Destination::Servers,
            ),
            (
                ph::SPARKLE,
                kit::TEAL,
                omni_items.to_string(),
                "Omni items",
                Destination::Omni,
            ),
        ];
        let mut go = None;
        kit::card_grid(ui, 4, 72.0, 10.0, 170.0, 4, |ui, i, rect| {
            let (glyph, tint, value, caption, dest) = &cards[i];
            let r = ui.interact(rect, ui.id().with(("metric", i)), egui::Sense::click());
            let painter = ui.painter();
            kit::paint_card(painter, rect, r.hovered());
            kit::paint_bubble(
                painter,
                egui::pos2(rect.left() + 34.0, rect.center().y),
                glyph,
                *tint,
                46.0,
            );
            painter.text(
                egui::pos2(rect.left() + 68.0, rect.center().y - 1.0),
                Align2::LEFT_BOTTOM,
                value,
                theme::semibold(21.0),
                theme::TEXT,
            );
            let wide = rect.width() >= 205.0;
            kit::text_at(
                painter,
                egui::pos2(rect.left() + 68.0, rect.center().y + 3.0),
                Align2::LEFT_TOP,
                caption,
                theme::regular(13.0),
                theme::MUTED,
                rect.width() - if wide { 96.0 } else { 74.0 },
            );
            if wide {
                kit::chevron(
                    painter,
                    egui::pos2(rect.right() - 16.0, rect.center().y),
                    theme::MUTED,
                );
            }
            let label = format!("{value} {caption}");
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
            if r.clicked() {
                go = Some(*dest);
            }
        });
        if let Some(d) = go {
            if d == Destination::Messages {
                self.conversation_tab = 1;
            }
            self.navigate(d);
        }
    }

    /// Three most recent conversations as cards (A02 "Pick up where you
    /// left off").
    fn home_pickup(&mut self, ui: &mut Ui, s: &Snapshot) {
        let rows: Vec<ConversationRow> = s
            .conversations
            .conversations
            .iter()
            .take(3)
            .cloned()
            .collect();
        if rows.is_empty() {
            kit::empty(
                ui,
                Some(ph::CHAT_CIRCLE),
                "Nothing to pick up",
                "Recent conversations appear here.",
            );
            return;
        }
        let now = Timestamp::now();
        let private = self.private();
        let mut open = None;
        kit::card_grid(ui, rows.len(), 124.0, GAP, 220.0, 3, |ui, i, rect| {
            let r = &rows[i];
            let resp = kit::card_at(ui, rect, ui.id().with(("pickup", i)), None);
            let title = if private {
                "Hidden in privacy mode".to_owned()
            } else {
                r.title.clone()
            };
            let painter = ui.painter();
            let c = egui::pos2(rect.left() + 38.0, rect.top() + 38.0);
            paint_conversation_avatar(painter, c, 46.0, r, &title, theme::CARD);
            let x = rect.left() + 74.0;
            let time = r
                .last_activity_at
                .map(|t| list_time(t, now))
                .unwrap_or_default();
            let tw = kit::text_width(painter, &time, theme::regular(13.0));
            kit::text_at(
                painter,
                egui::pos2(x, rect.top() + 26.0),
                Align2::LEFT_CENTER,
                &title,
                theme::medium(15.5),
                theme::TEXT,
                rect.right() - x - tw - 26.0,
            );
            painter.text(
                egui::pos2(rect.right() - 14.0, rect.top() + 26.0),
                Align2::RIGHT_CENTER,
                time,
                theme::regular(13.0),
                theme::MUTED,
            );
            let preview = if private {
                String::new()
            } else {
                r.last_message_preview
                    .clone()
                    .unwrap_or_else(|| "No messages cached yet".into())
            };
            let g = kit::wrapped(
                painter,
                &preview,
                theme::regular(14.0),
                theme::SECONDARY,
                rect.right() - x - 14.0,
                2,
            );
            painter.galley(egui::pos2(x, rect.top() + 40.0), g, theme::SECONDARY);
            let (glyph, label) = match r.kind {
                ConversationKind::GuildChannel => (ph::HASH, "Go to channel"),
                ConversationKind::GroupDm => (ph::CHATS, "Open group"),
                _ => (ph::CHAT_CIRCLE, "Direct message"),
            };
            let bw = kit::button_width(painter, Some(glyph), label, 28.0);
            let b = Rect::from_min_size(egui::pos2(x, rect.bottom() - 40.0), egui::vec2(bw, 28.0));
            let clicked_b = kit::button_at(
                ui,
                b,
                ui.id().with(("pickup_btn", i)),
                kit::Kind::Secondary,
                Some(glyph),
                label,
                true,
            )
            .clicked();
            kit::chevron(
                ui.painter(),
                egui::pos2(rect.right() - 18.0, b.center().y),
                theme::MUTED,
            );
            if resp.clicked() || clicked_b {
                open = Some(r.conversation_id);
            }
        });
        if let Some(id) = open {
            self.open_conversation(id);
        }
    }

    /// Omni cards: replies owed, approvals, a red follow-up, new memories.
    fn home_omni_cards(&mut self, ui: &mut Ui, s: &Snapshot) {
        let replies: Vec<String> = s
            .inbox
            .needs_attention
            .iter()
            .filter_map(|i| match i {
                InboxItem::PendingReply { from, .. } => Some(self.display(from)),
                _ => None,
            })
            .collect();
        let approvals = s.inbox.pending_actions.len() + s.omni.requests.len();
        let follow_up = s.inbox.needs_attention.iter().find_map(|i| match i {
            InboxItem::TaskCandidate { task_id, title, .. } => {
                Some((Some(*task_id), self.display(title)))
            }
            InboxItem::Commitment { text, .. } => Some((None, self.display(text))),
            _ => None,
        });
        let new_memories = candidates(s);
        let names = match replies.len() {
            0 => "You're all caught up.".to_owned(),
            1 => replies[0].clone(),
            2 => format!("{} and {}", replies[0], replies[1]),
            n => format!("{}, {} and {} more", replies[0], replies[1], n - 2),
        };
        struct Card {
            glyph: &'static str,
            accent: egui::Color32,
            title: String,
            body: String,
            button: &'static str,
            kind: kit::Kind,
        }
        let cards = [
            Card {
                glyph: ph::CHAT_CIRCLE_DOTS,
                accent: theme::OMNI,
                title: format!(
                    "{} conversation{} need a reply",
                    replies.len(),
                    if replies.len() == 1 { "" } else { "s" }
                ),
                body: names,
                button: "Open inbox",
                kind: kit::Kind::Omni,
            },
            Card {
                glyph: ph::FILE_TEXT,
                accent: theme::OMNI,
                title: format!("{approvals} waiting for approval"),
                body: "Messages and actions proposed by Omni or you.".into(),
                button: "Review",
                kind: kit::Kind::Omni,
            },
            match &follow_up {
                Some((_, text)) => Card {
                    glyph: ph::CALENDAR_BLANK,
                    accent: theme::PRIORITY,
                    title: "Follow up suggested".into(),
                    body: text.clone(),
                    button: "Add to tasks",
                    kind: kit::Kind::Danger,
                },
                None => Card {
                    glyph: ph::CALENDAR_CHECK,
                    accent: theme::OMNI,
                    title: "No follow-ups".into(),
                    body: "Commitments you make in chat show up here.".into(),
                    button: "Open tasks",
                    kind: kit::Kind::Omni,
                },
            },
            Card {
                glyph: ph::BRAIN,
                accent: theme::OMNI,
                title: "New context added".into(),
                body: format!(
                    "{new_memories} new memor{} from your conversations to review.",
                    if new_memories == 1 { "y" } else { "ies" }
                ),
                button: "View in Omni",
                kind: kit::Kind::Omni,
            },
        ];
        let mut clicked = None;
        kit::card_grid(ui, cards.len(), 142.0, GAP, 170.0, 4, |ui, i, rect| {
            let c = &cards[i];
            let resp = kit::card_at(ui, rect, ui.id().with(("omni_card", i)), Some(c.accent));
            let painter = ui.painter();
            let tint = if c.accent == theme::PRIORITY {
                kit::RED
            } else {
                kit::TEAL
            };
            kit::paint_bubble(
                painter,
                egui::pos2(rect.left() + 30.0, rect.top() + 32.0),
                c.glyph,
                tint,
                42.0,
            );
            let x = rect.left() + 60.0;
            let tw = rect.right() - x - 10.0;
            let title_color = if c.accent == theme::PRIORITY {
                theme::PRIORITY_TEXT
            } else {
                theme::TEXT
            };
            let g = kit::wrapped(painter, &c.title, theme::medium(13.5), title_color, tw, 2);
            let th = g.size().y;
            painter.galley(egui::pos2(x, rect.top() + 14.0), g, title_color);
            let body_color = if c.accent == theme::PRIORITY {
                theme::lerp(theme::PRIORITY_TEXT, theme::SECONDARY, 0.5)
            } else {
                theme::MUTED
            };
            let b = kit::wrapped(
                painter,
                &c.body,
                theme::regular(12.5),
                body_color,
                tw,
                if th > 20.0 { 2 } else { 3 },
            );
            painter.galley(egui::pos2(x, rect.top() + 18.0 + th), b, body_color);
            let bw = kit::button_width(painter, None, c.button, 28.0);
            let br = Rect::from_min_size(
                egui::pos2(x, rect.bottom() - 40.0),
                egui::vec2(bw.min(rect.right() - x - 36.0), 28.0),
            );
            let clicked_b = kit::button_at(
                ui,
                br,
                ui.id().with(("omni_card_btn", i)),
                c.kind,
                None,
                c.button,
                true,
            )
            .clicked();
            kit::chevron(
                ui.painter(),
                egui::pos2(rect.right() - 18.0, br.center().y),
                if c.accent == theme::PRIORITY {
                    theme::PRIORITY_TEXT
                } else {
                    theme::MUTED
                },
            );
            if clicked_b || resp.clicked() {
                clicked = Some(i);
            }
        });
        match clicked {
            Some(0) => {
                self.inbox_filter = 1;
                self.navigate(Destination::Inbox);
            }
            Some(1) => {
                self.inbox_filter = 2;
                self.navigate(Destination::Inbox);
            }
            Some(2) => match follow_up {
                Some((Some(task), _)) => self.send(Command::ConfirmTask(task)),
                _ => self.navigate(Destination::Tasks),
            },
            Some(_) => {
                self.memory_status_filter = Some(MemoryStatus::Candidate);
                self.navigate(Destination::Omni);
            }
            None => {}
        }
    }

    fn home_online(&mut self, ui: &mut Ui, s: &Snapshot) {
        let friends: Vec<_> = s.friends.online.iter().take(5).cloned().collect();
        if friends.is_empty() {
            kit::empty(
                ui,
                Some(ph::USERS),
                "Nobody is online",
                "Friends appear here when they come online.",
            );
            return;
        }
        let mut open = None;
        kit::card_grid(ui, friends.len(), 62.0, 10.0, 140.0, 5, |ui, i, rect| {
            let f = &friends[i];
            let resp = kit::card_at(ui, rect, ui.id().with(("online", i)), None);
            let name = self.display(f.alias.as_deref().unwrap_or(&f.display_name));
            let painter = ui.painter();
            theme::paint_avatar(
                painter,
                egui::pos2(rect.left() + 30.0, rect.center().y),
                38.0,
                &name,
                theme::Presence::from_status(f.status.as_str()),
                theme::CARD,
            );
            let x = rect.left() + 58.0;
            let w = rect.right() - x - 8.0;
            kit::text_at(
                painter,
                egui::pos2(x, rect.center().y - 9.0),
                Align2::LEFT_CENTER,
                &name,
                theme::medium(14.0),
                theme::TEXT,
                w,
            );
            let status = f.activity.as_ref().filter(|_| !self.private()).map_or_else(
                || {
                    theme::Presence::from_status(f.status.as_str())
                        .title()
                        .to_owned()
                },
                |a| a.name.clone(),
            );
            kit::text_at(
                painter,
                egui::pos2(x, rect.center().y + 10.0),
                Align2::LEFT_CENTER,
                &status,
                theme::regular(12.5),
                theme::MUTED,
                w,
            );
            if resp.clicked() {
                open = Some((f.user_id, f.dm_conversation_id));
            }
        });
        if let Some((user, dm)) = open {
            match dm {
                Some(conv) => self.open_conversation(conv),
                None => {
                    self.selection.contact = Some(user);
                    self.navigate(Destination::Friends);
                }
            }
        }
    }

    fn home_servers(&mut self, ui: &mut Ui, s: &Snapshot) {
        let guilds: Vec<_> = s.guilds.guilds.iter().take(3).cloned().collect();
        let mut open = None;
        kit::card_grid(ui, guilds.len(), 96.0, GAP, 220.0, 3, |ui, i, rect| {
            let g = &guilds[i];
            let resp = kit::card_at(ui, rect, ui.id().with(("guild", i)), None);
            let name = self.display(&g.guild.name);
            let painter = ui.painter();
            let tile = Rect::from_center_size(
                egui::pos2(rect.left() + 38.0, rect.top() + 38.0),
                egui::vec2(46.0, 46.0),
            );
            kit::paint_tile(painter, tile, ph::SQUARES_FOUR, kit::tint_for(&name), 12.0);
            let x = rect.left() + 76.0;
            let w = rect.right() - x - 12.0;
            kit::text_at(
                painter,
                egui::pos2(x, rect.top() + 24.0),
                Align2::LEFT_CENTER,
                &name,
                theme::medium(15.0),
                theme::TEXT,
                w,
            );
            let first = g
                .channels
                .first()
                .map(|c| format!("#{}", c.channel.name))
                .unwrap_or_default();
            kit::text_at(
                painter,
                egui::pos2(x, rect.top() + 46.0),
                Align2::LEFT_CENTER,
                &first,
                theme::regular(13.5),
                theme::PRIMARY_TEXT,
                w,
            );
            let detail = format!(
                "{} channel{}",
                g.channels.len(),
                if g.channels.len() == 1 { "" } else { "s" }
            );
            kit::text_at(
                painter,
                egui::pos2(x, rect.top() + 68.0),
                Align2::LEFT_CENTER,
                &detail,
                theme::regular(13.0),
                theme::MUTED,
                w,
            );
            if resp.clicked() {
                open = Some(g.guild.id);
            }
        });
        if let Some(id) = open {
            self.selected_guild = Some(id);
            self.selected_channel = None;
            self.navigate(Destination::Servers);
        }
    }

    /// A02 "Recent activity": filter chips and one row per recent event.
    fn home_activity(&mut self, ui: &mut Ui, s: &Snapshot) {
        let (title_rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), egui::Sense::hover());
        ui.painter().text(
            title_rect.left_center(),
            Align2::LEFT_CENTER,
            "Recent activity",
            theme::semibold(17.0),
            theme::TEXT,
        );
        let labels = ["All", "Messages", "Servers", "Omni"];
        let chips_w: f32 = labels
            .iter()
            .map(|l| kit::text_width(ui.painter(), l, theme::medium(13.0)) + 32.0)
            .sum();
        let chips = Rect::from_min_max(
            egui::pos2(title_rect.right() - chips_w, title_rect.top()),
            title_rect.max,
        );
        let mut cui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(chips)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        kit::chip_row(&mut cui, &labels, &mut self.home_activity_filter);
        ui.add_space(4.0);
        struct Line {
            conversation: Option<litecord_types::ConversationId>,
            who: String,
            verb: &'static str,
            at: Option<Timestamp>,
            text: String,
            unread: bool,
            omni: bool,
            row: Option<ConversationRow>,
        }
        let private = self.private();
        let mut lines: Vec<Line> = Vec::new();
        for r in s.conversations.conversations.iter().take(8) {
            let guild = r.kind == ConversationKind::GuildChannel;
            if (self.home_activity_filter == 1 && guild)
                || (self.home_activity_filter == 2 && !guild)
                || self.home_activity_filter == 3
            {
                continue;
            }
            lines.push(Line {
                conversation: Some(r.conversation_id),
                who: if private {
                    "Hidden".into()
                } else {
                    r.title.clone()
                },
                verb: match r.kind {
                    ConversationKind::GuildChannel => "posted in the channel",
                    ConversationKind::GroupDm => "new message in the group",
                    _ if r.awaiting_reply => "sent you a message",
                    _ => "latest message",
                },
                at: r.last_activity_at,
                text: if private {
                    "Hidden in privacy mode".into()
                } else {
                    r.last_message_preview.clone().unwrap_or_default()
                },
                unread: r.awaiting_reply,
                omni: false,
                row: Some(r.clone()),
            });
        }
        if self.home_activity_filter == 0 || self.home_activity_filter == 3 {
            for c in s.omni.checkins.iter().take(3) {
                lines.push(Line {
                    conversation: None,
                    who: "Omni".into(),
                    verb: "checked in",
                    at: None,
                    text: self.display(&c.text),
                    unread: true,
                    omni: true,
                    row: None,
                });
            }
        }
        if lines.is_empty() {
            kit::empty(
                ui,
                Some(ph::CLOCK),
                "No recent activity",
                "New messages and Omni check-ins show up here.",
            );
            return;
        }
        let now = Timestamp::now();
        let mut open = None;
        let mut open_omni = false;
        egui::Frame::new()
            .fill(theme::CARD)
            .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, l) in lines.iter().enumerate() {
                    if i > 0 {
                        kit::divider(ui);
                    }
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 50.0),
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
                    if l.unread {
                        kit::dot(
                            painter,
                            egui::pos2(rect.left() + 12.0, rect.center().y),
                            4.0,
                            if l.omni { theme::OMNI } else { theme::PRIMARY },
                        );
                    }
                    let c = egui::pos2(rect.left() + 42.0, rect.center().y);
                    match &l.row {
                        Some(r) => {
                            paint_conversation_avatar(painter, c, 34.0, r, &l.who, theme::CARD)
                        }
                        None => kit::paint_bubble(painter, c, ph::SPARKLE, kit::TEAL, 34.0),
                    }
                    let x = rect.left() + 70.0;
                    let who = kit::text_at(
                        painter,
                        egui::pos2(x, rect.center().y - 10.0),
                        Align2::LEFT_CENTER,
                        &l.who,
                        theme::medium(14.5),
                        theme::TEXT,
                        220.0,
                    );
                    let verb = kit::text_at(
                        painter,
                        egui::pos2(who.right() + 6.0, rect.center().y - 10.0),
                        Align2::LEFT_CENTER,
                        l.verb,
                        theme::regular(13.5),
                        theme::MUTED,
                        200.0,
                    );
                    if let Some(at) = l.at {
                        painter.text(
                            egui::pos2(verb.right() + 10.0, rect.center().y - 10.0),
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
                        &l.text,
                        theme::regular(14.0),
                        theme::SECONDARY,
                        rect.right() - x - 48.0,
                    );
                    kit::icon(
                        painter,
                        rect.right_center() - egui::vec2(20.0, 0.0),
                        ph::DOTS_THREE,
                        16.0,
                        theme::MUTED,
                    );
                    if resp.clicked() {
                        if l.omni {
                            open_omni = true;
                        } else {
                            open = l.conversation;
                        }
                    }
                }
            });
        if let Some(id) = open {
            self.open_conversation(id);
        }
        if open_omni {
            self.omni_open = true;
        }
    }

    // ---- Sidebar -------------------------------------------------------------

    pub(crate) fn home_sidebar(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        kit::sidebar_title(ui, "Home", None);
        egui::ScrollArea::vertical()
            .id_salt("home_sidebar")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                kit::group_label(ui, "Favorites");
                let favorites: Vec<ConversationRow> = s
                    .friends
                    .online
                    .iter()
                    .chain(&s.friends.offline)
                    .filter(|f| f.favorite)
                    .filter_map(|f| {
                        s.conversations
                            .conversations
                            .iter()
                            .find(|r| Some(r.conversation_id) == f.dm_conversation_id)
                            .cloned()
                    })
                    .collect();
                if favorites.is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let (g, _) =
                            ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
                        kit::icon(ui.painter(), g.center(), ph::STAR, 15.0, theme::MUTED);
                        kit::para(
                            ui,
                            "Add people to favorites from their profile to keep them here.",
                            theme::regular(13.0),
                            theme::MUTED,
                        );
                    });
                    ui.add_space(6.0);
                }
                for r in &favorites {
                    self.sidebar_conversation(ui, r, true);
                }
                ui.add_space(10.0);
                kit::group_label(ui, "Recent");
                let recent: Vec<ConversationRow> = s
                    .conversations
                    .conversations
                    .iter()
                    .take(12)
                    .cloned()
                    .collect();
                for r in &recent {
                    self.sidebar_conversation(ui, r, false);
                }
            });
    }

    /// A 64px conversation row for contextual sidebars (A02/A09 lists).
    pub(crate) fn sidebar_conversation(&mut self, ui: &mut Ui, r: &ConversationRow, pinned: bool) {
        let selected = self.selection.destination == Destination::Messages
            && Some(r.conversation_id) == self.selection.conversation;
        let (rect, response) = kit::row(ui, 64.0, selected);
        let title = self.display(&r.title);
        let painter = ui.painter();
        paint_conversation_avatar(
            painter,
            egui::pos2(rect.left() + 32.0, rect.center().y),
            44.0,
            r,
            &title,
            if selected {
                theme::SELECTED
            } else {
                theme::SIDEBAR
            },
        );
        let x = rect.left() + 64.0;
        let time = r
            .last_activity_at
            .map(|t| list_time(t, Timestamp::now()))
            .unwrap_or_default();
        let tw = kit::text_width(painter, &time, theme::regular(12.5));
        kit::text_at(
            painter,
            egui::pos2(x, rect.center().y - 10.0),
            Align2::LEFT_CENTER,
            &title,
            theme::medium(15.0),
            theme::TEXT,
            rect.right() - x - tw - 18.0,
        );
        painter.text(
            egui::pos2(rect.right() - 10.0, rect.center().y - 10.0),
            Align2::RIGHT_CENTER,
            time,
            theme::regular(12.5),
            theme::MUTED,
        );
        let preview = if self.private() {
            "Hidden in privacy mode".to_owned()
        } else {
            r.last_message_preview.clone().unwrap_or_default()
        };
        kit::text_at(
            painter,
            egui::pos2(x, rect.center().y + 11.0),
            Align2::LEFT_CENTER,
            &preview,
            theme::regular(13.5),
            theme::SECONDARY,
            rect.right() - x - 32.0,
        );
        if pinned {
            kit::icon(
                painter,
                egui::pos2(rect.right() - 16.0, rect.center().y + 11.0),
                ph::PUSH_PIN,
                14.0,
                theme::MUTED,
            );
        } else if r.awaiting_reply {
            kit::dot(
                painter,
                egui::pos2(rect.right() - 16.0, rect.center().y + 11.0),
                4.5,
                theme::PRIMARY,
            );
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &title)
        });
        if response.clicked() {
            self.open_conversation(r.conversation_id);
        }
    }

    // ---- Inspector -------------------------------------------------------------

    pub(crate) fn home_inspector(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("home_inspector")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.account_block(ui, &s);
                ui.add_space(12.0);
                self.today_card(ui, &s);
                ui.add_space(10.0);
                self.omni_memory_card(ui, &s);
                ui.add_space(10.0);
                self.online_now_card(ui, &s);
                ui.add_space(12.0);
                self.quick_actions(ui);
            });
    }

    /// The signed-in account (A02 inspector header).
    pub(crate) fn account_block(&mut self, ui: &mut Ui, s: &Snapshot) {
        let name = self.display(&s.account.display_name);
        let (r, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 88.0), egui::Sense::hover());
        theme::paint_avatar(
            ui.painter(),
            egui::pos2(r.left() + 42.0, r.top() + 42.0),
            84.0,
            &name,
            if s.diagnostics.session.is_online() {
                theme::Presence::Online
            } else {
                theme::Presence::Offline
            },
            theme::INSPECTOR,
        );
        ui.add_space(6.0);
        kit::label(ui, &name, theme::semibold(22.0), theme::TEXT);
        if let Some(u) = &s.account.username {
            ui.add_space(-4.0);
            kit::label(
                ui,
                format!("@{}", self.display(u)),
                theme::regular(15.0),
                theme::SECONDARY,
            );
        }
        let source = match s.diagnostics.backend_mode {
            litecord_types::capability::BackendMode::Demo => "Demo account · synthetic data",
            litecord_types::capability::BackendMode::UserSession => "User session (read only)",
            _ => "Signed in with Discord",
        };
        kit::label(ui, source, theme::regular(13.5), theme::MUTED);
    }

    fn today_card(&mut self, ui: &mut Ui, s: &Snapshot) {
        let now = Timestamp::now();
        let end_of_day = (now.as_millis() / 86_400_000 + 1) * 86_400_000;
        let due = s
            .tasks
            .open
            .iter()
            .filter(|t| t.due_at.is_some_and(|d| d.as_millis() < end_of_day))
            .count();
        let reminders = s
            .tasks
            .reminders
            .iter()
            .filter(|r| r.trigger.due_at().as_millis() < end_of_day)
            .count();
        let unread = s
            .conversations
            .conversations
            .iter()
            .filter(|r| r.awaiting_reply)
            .count();
        let mut go = None;
        kit::card(ui, |ui| {
            let (h, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 24.0), egui::Sense::hover());
            ui.painter().text(
                h.left_center(),
                Align2::LEFT_CENTER,
                "Today",
                theme::semibold(16.0),
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
            let rows = [
                (
                    ph::CALENDAR_BLANK,
                    theme::PRIORITY,
                    format!("{due} task{} due", if due == 1 { "" } else { "s" }),
                    Destination::Tasks,
                ),
                (
                    ph::CHECK_SQUARE,
                    theme::SUCCESS,
                    format!(
                        "{reminders} reminder{}",
                        if reminders == 1 { "" } else { "s" }
                    ),
                    Destination::Tasks,
                ),
                (
                    ph::CHAT_CIRCLE,
                    theme::PRIMARY,
                    format!(
                        "{unread} unread message{}",
                        if unread == 1 { "" } else { "s" }
                    ),
                    Destination::Messages,
                ),
            ];
            for (i, (g, c, t, d)) in rows.iter().enumerate() {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 30.0),
                    egui::Sense::hover(),
                );
                if kit::list_line(
                    ui,
                    rect,
                    ui.id().with(("today", i)),
                    g,
                    *c,
                    t,
                    theme::lerp(theme::TEXT, theme::SECONDARY, 0.2),
                )
                .clicked()
                {
                    go = Some(*d);
                }
            }
        });
        if let Some(d) = go {
            if d == Destination::Messages {
                self.conversation_tab = 1;
            }
            self.navigate(d);
        }
    }

    fn omni_memory_card(&mut self, ui: &mut Ui, s: &Snapshot) {
        let replies = s
            .inbox
            .needs_attention
            .iter()
            .filter(|i| matches!(i, InboxItem::PendingReply { .. }))
            .count();
        let approvals = s.inbox.pending_actions.len() + s.omni.requests.len();
        let follow = s.inbox.needs_attention.iter().find_map(|i| match i {
            InboxItem::TaskCandidate { title, .. } => Some(self.display(title)),
            InboxItem::Commitment { text, .. } => Some(self.display(text)),
            _ => None,
        });
        let memories = candidates(s);
        let mut go: Option<u8> = None;
        kit::card(ui, |ui| {
            if kit::section(ui, "Omni & Memory", None, Some("See all")) {
                go = Some(3);
            }
            let mut rows: Vec<(&str, egui::Color32, String, egui::Color32, u8)> = vec![
                (
                    ph::CHAT_CIRCLE_DOTS,
                    theme::OMNI,
                    format!(
                        "{replies} conversation{} need a reply",
                        if replies == 1 { "" } else { "s" }
                    ),
                    theme::lerp(theme::TEXT, theme::SECONDARY, 0.2),
                    0,
                ),
                (
                    ph::FILE_TEXT,
                    theme::OMNI,
                    format!("{approvals} waiting for approval"),
                    theme::lerp(theme::TEXT, theme::SECONDARY, 0.2),
                    1,
                ),
            ];
            if let Some(f) = &follow {
                rows.push((
                    ph::CALENDAR_BLANK,
                    theme::PRIORITY,
                    format!("Follow up: {f}"),
                    theme::PRIORITY_TEXT,
                    2,
                ));
            }
            rows.push((
                ph::BRAIN,
                theme::OMNI,
                format!(
                    "{memories} new memor{} to review",
                    if memories == 1 { "y" } else { "ies" }
                ),
                theme::lerp(theme::TEXT, theme::SECONDARY, 0.2),
                3,
            ));
            for (i, (g, c, t, tc, d)) in rows.iter().enumerate() {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 30.0),
                    egui::Sense::hover(),
                );
                if kit::list_line(ui, rect, ui.id().with(("omni_mem", i)), g, *c, t, *tc).clicked()
                {
                    go = Some(*d);
                }
            }
        });
        match go {
            Some(0) => {
                self.inbox_filter = 1;
                self.navigate(Destination::Inbox);
            }
            Some(1) => {
                self.inbox_filter = 2;
                self.navigate(Destination::Inbox);
            }
            Some(2) => self.navigate(Destination::Tasks),
            Some(_) => {
                self.memory_status_filter = Some(MemoryStatus::Candidate);
                self.navigate(Destination::Omni);
            }
            None => {}
        }
    }

    pub(crate) fn online_now_card(&mut self, ui: &mut Ui, s: &Snapshot) {
        let friends: Vec<_> = s.friends.online.iter().take(4).cloned().collect();
        let mut action: Option<(u8, usize)> = None;
        kit::card(ui, |ui| {
            if kit::section(ui, "Online now", None, Some("See all")) {
                action = Some((9, 0));
            }
            if friends.is_empty() {
                kit::label(
                    ui,
                    "Nobody is online right now.",
                    theme::regular(13.5),
                    theme::MUTED,
                );
            }
            for (i, f) in friends.iter().enumerate() {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 44.0),
                    egui::Sense::hover(),
                );
                let name = self.display(f.alias.as_deref().unwrap_or(&f.display_name));
                let presence = theme::Presence::from_status(f.status.as_str());
                let painter = ui.painter();
                theme::paint_avatar(
                    painter,
                    egui::pos2(rect.left() + 18.0, rect.center().y),
                    34.0,
                    &name,
                    presence,
                    theme::CARD,
                );
                let x = rect.left() + 44.0;
                let w = rect.width() - 44.0 - 110.0;
                kit::text_at(
                    painter,
                    egui::pos2(x, rect.center().y - 8.0),
                    Align2::LEFT_CENTER,
                    &name,
                    theme::medium(14.0),
                    theme::TEXT,
                    w,
                );
                let status = f
                    .activity
                    .as_ref()
                    .filter(|_| !self.private())
                    .map_or_else(|| presence.title().to_owned(), |a| a.name.clone());
                painter.circle_filled(
                    egui::pos2(x + 4.0, rect.center().y + 10.0),
                    3.5,
                    presence.color(),
                );
                kit::text_at(
                    painter,
                    egui::pos2(x + 12.0, rect.center().y + 10.0),
                    Align2::LEFT_CENTER,
                    &status,
                    theme::regular(12.5),
                    theme::MUTED,
                    w - 12.0,
                );
                let mut cx = rect.right() - 16.0;
                for (k, (g, tip)) in [
                    (ph::DOTS_THREE, "More"),
                    (ph::PHONE, "Calls open in Discord"),
                    (ph::CHAT_CIRCLE, "Message"),
                ]
                .into_iter()
                .enumerate()
                {
                    if kit::disc_at(
                        ui,
                        egui::pos2(cx, rect.center().y),
                        30.0,
                        ui.id().with(("online_now", i, k)),
                        g,
                        tip,
                    )
                    .clicked()
                    {
                        action = Some((k as u8, i));
                    }
                    cx -= 36.0;
                }
            }
        });
        match action {
            Some((9, _)) => {
                self.friends_tab = 0;
                self.navigate(Destination::Friends);
            }
            Some((2, i)) => {
                if let Some(conv) = friends[i].dm_conversation_id {
                    self.open_conversation(conv);
                }
            }
            Some((1, i)) => {
                let url = format!("https://discord.com/users/{}", friends[i].user_id);
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
            Some((_, i)) => {
                self.selection.contact = Some(friends[i].user_id);
                self.note_draft = None;
                self.navigate(Destination::Friends);
            }
            None => {}
        }
    }

    pub(crate) fn quick_actions(&mut self, ui: &mut Ui) {
        kit::label(ui, "Quick actions", theme::semibold(16.0), theme::TEXT);
        ui.add_space(2.0);
        let items = [
            (ph::CHAT_CIRCLE, "New message", kit::Kind::Secondary),
            (ph::WAVEFORM, "Start voice", kit::Kind::Secondary),
            (ph::SPARKLE, "Open Omni", kit::Kind::Omni),
            (ph::TRAY, "Agent inbox", kit::Kind::Secondary),
        ];
        let mut clicked = None;
        kit::card_grid(ui, 4, 38.0, 8.0, 120.0, 2, |ui, i, rect| {
            let (g, l, k) = items[i];
            if kit::button_at(ui, rect, ui.id().with(("quick", i)), k, Some(g), l, true).clicked() {
                clicked = Some(i);
            }
        });
        match clicked {
            Some(0) => self.navigate(Destination::Friends),
            Some(1) => self.navigate(Destination::Voice),
            Some(2) => self.navigate(Destination::Omni),
            Some(_) => self.navigate(Destination::Inbox),
            None => {}
        }
    }
}

/// Avatar for a conversation: person, group glyph or channel hash.
pub(crate) fn paint_conversation_avatar(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    r: &ConversationRow,
    title: &str,
    ring: egui::Color32,
) {
    match r.kind {
        ConversationKind::GroupDm => {
            kit::paint_group(painter, center, size, ph::USERS, theme::SECONDARY)
        }
        ConversationKind::GuildChannel => {
            kit::paint_group(painter, center, size, ph::HASH, theme::SECONDARY)
        }
        _ => theme::paint_avatar(
            painter,
            center,
            size,
            title,
            r.recipient_status.map_or(theme::Presence::None, |p| {
                theme::Presence::from_status(p.as_str())
            }),
            ring,
        ),
    }
}

/// Memories waiting for review.
pub(crate) fn candidates(s: &Snapshot) -> usize {
    s.memory
        .counts_by_status
        .iter()
        .find(|(st, _)| *st == MemoryStatus::Candidate)
        .map_or(0, |(_, n)| *n as usize)
}
