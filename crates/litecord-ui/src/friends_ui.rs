//! Friends (mock A03): pill-filtered sidebar (Online / All / Pending /
//! Blocked) with favorites and online lists; a main view with metric cards,
//! top connections, an online grid, suggested follow-ups and recent
//! activity. The inspector is the shared contact profile.

use eframe::egui::{self, Align2, Rect, Ui};
use litecord_app::view::{FriendRow, InboxItem};
use litecord_layout::Destination;
use litecord_types::actions::RelationshipAction;
use litecord_types::capability::Capability;
use litecord_types::social::{ConversationKind, RelationshipKind};
use litecord_types::Timestamp;

use crate::bridge::{Command, Snapshot};
use crate::messages_ui::list_time;
use crate::workspace::Workspace;
use crate::{kit, ph, theme};

impl Workspace {
    // ---- Sidebar ---------------------------------------------------------------

    pub(crate) fn friends_sidebar(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            return;
        };
        if kit::sidebar_title(
            ui,
            "Friends",
            Some((ph::USER_PLUS, "Add a friend in Discord")),
        ) {
            ui.ctx()
                .open_url(egui::OpenUrl::new_tab("https://discord.com/channels/@me"));
        }
        ui.add_space(6.0);
        kit::search(ui, &mut self.filter, "Search friends...");
        ui.add_space(10.0);
        let f = &s.friends;
        let pending = f.pending_incoming.len() + f.pending_outgoing.len();
        // Online badge is blue, Pending red (A03).
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 6.0);
            for (i, (label, n, color)) in [
                ("Online", f.online.len(), theme::PRIMARY),
                ("All", 0, theme::PRIMARY),
                ("Pending", pending, theme::PRIORITY),
                ("Blocked", 0, theme::PRIMARY),
            ]
            .into_iter()
            .enumerate()
            {
                if pill_colored(ui, label, n, color, self.friends_tab == i).clicked() {
                    self.friends_tab = i;
                }
            }
        });
        ui.add_space(6.0);
        let filter = self.filter.to_lowercase();
        let matches = |r: &&FriendRow| {
            filter.is_empty()
                || r.display_name.to_lowercase().contains(&filter)
                || r.username.to_lowercase().contains(&filter)
        };
        egui::ScrollArea::vertical()
            .id_salt("friends_sidebar")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                match self.friends_tab {
                    2 => {
                        kit::group_label(ui, "Pending requests");
                        let rows: Vec<FriendRow> = f
                            .pending_incoming
                            .iter()
                            .chain(&f.pending_outgoing)
                            .filter(matches)
                            .cloned()
                            .collect();
                        if rows.is_empty() {
                            kit::label(
                                ui,
                                "No pending requests.",
                                theme::regular(13.5),
                                theme::MUTED,
                            );
                        }
                        for r in &rows {
                            self.friend_list_row(ui, r);
                        }
                    }
                    3 => {
                        kit::group_label(ui, "Blocked");
                        let rows: Vec<FriendRow> =
                            f.blocked.iter().filter(matches).cloned().collect();
                        if rows.is_empty() {
                            kit::label(
                                ui,
                                "Nobody is blocked.",
                                theme::regular(13.5),
                                theme::MUTED,
                            );
                        }
                        for r in &rows {
                            self.friend_list_row(ui, r);
                        }
                    }
                    tab => {
                        let favorites: Vec<FriendRow> = f
                            .online
                            .iter()
                            .chain(&f.offline)
                            .filter(|r| r.favorite)
                            .filter(matches)
                            .cloned()
                            .collect();
                        if !favorites.is_empty() {
                            kit::group_label(ui, "Favorites");
                            for r in &favorites {
                                self.friend_list_row(ui, r);
                            }
                            ui.add_space(8.0);
                        }
                        kit::group_label(
                            ui,
                            if tab == 0 {
                                "Online now"
                            } else {
                                "All friends"
                            },
                        );
                        let rows: Vec<FriendRow> = if tab == 0 {
                            f.online.iter().filter(matches).cloned().collect()
                        } else {
                            f.online
                                .iter()
                                .chain(&f.offline)
                                .filter(matches)
                                .cloned()
                                .collect()
                        };
                        if rows.is_empty() {
                            kit::label(
                                ui,
                                "Nobody here right now.",
                                theme::regular(13.5),
                                theme::MUTED,
                            );
                        }
                        for r in &rows {
                            self.friend_list_row(ui, r);
                        }
                        let groups: Vec<_> = s
                            .conversations
                            .conversations
                            .iter()
                            .filter(|c| c.kind == ConversationKind::GroupDm)
                            .take(4)
                            .cloned()
                            .collect();
                        if !groups.is_empty() {
                            ui.add_space(8.0);
                            kit::group_label(ui, "Recently active");
                            for g in &groups {
                                self.sidebar_conversation(ui, g, false);
                            }
                        }
                    }
                }
            });
    }

    fn friend_list_row(&mut self, ui: &mut Ui, r: &FriendRow) {
        let selected = self.selection.contact == Some(r.user_id);
        let (rect, response) = kit::row(ui, 62.0, selected);
        let name = self.display(r.alias.as_deref().unwrap_or(&r.display_name));
        let presence = if r.relationship == RelationshipKind::Friend {
            theme::Presence::from_status(r.status.as_str())
        } else {
            theme::Presence::None
        };
        let painter = ui.painter();
        theme::paint_avatar_url(
            painter,
            egui::pos2(rect.left() + 32.0, rect.center().y),
            44.0,
            &name,
            r.avatar_url.as_deref().filter(|_| !self.private()),
            presence,
            if selected {
                theme::SELECTED
            } else {
                theme::SIDEBAR
            },
        );
        let x = rect.left() + 64.0;
        kit::text_at(
            painter,
            egui::pos2(x, rect.center().y - 10.0),
            Align2::LEFT_CENTER,
            &name,
            theme::medium(15.0),
            theme::TEXT,
            rect.right() - x - 36.0,
        );
        kit::text_at(
            painter,
            egui::pos2(x, rect.center().y + 11.0),
            Align2::LEFT_CENTER,
            &self.friend_status(r),
            theme::regular(13.5),
            theme::SECONDARY,
            rect.right() - x - 36.0,
        );
        if r.favorite {
            kit::icon(
                painter,
                egui::pos2(rect.right() - 16.0, rect.center().y + 10.0),
                ph::STAR,
                14.0,
                theme::MUTED,
            );
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &name)
        });
        if response.clicked() {
            self.selection.contact = Some(r.user_id);
            self.note_draft = None;
            self.request();
        }
    }

    /// One-line status: activity, pending direction, or presence.
    pub(crate) fn friend_status(&self, r: &FriendRow) -> String {
        match r.relationship {
            RelationshipKind::PendingIncoming => "Incoming friend request".into(),
            RelationshipKind::PendingOutgoing => "Outgoing friend request".into(),
            RelationshipKind::Blocked => "Blocked".into(),
            _ => match (&r.activity, self.private()) {
                (Some(a), false) => match &a.details {
                    Some(d) => format!("Playing {} · {d}", a.name),
                    None => format!("Playing {}", a.name),
                },
                _ => theme::Presence::from_status(r.status.as_str())
                    .title()
                    .to_owned(),
            },
        }
    }

    // ---- Main --------------------------------------------------------------------

    pub(crate) fn friends_screen(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("friends_screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                kit::page_title(
                    ui,
                    "Friends",
                    Some("Keep up with the people you talk to most."),
                );
                self.friends_metrics(ui, &s);
                ui.add_space(6.0);
                match self.friends_tab {
                    2 => {
                        kit::section(ui, "Pending requests", None, None);
                        let rows: Vec<FriendRow> = s
                            .friends
                            .pending_incoming
                            .iter()
                            .chain(&s.friends.pending_outgoing)
                            .cloned()
                            .collect();
                        self.request_cards(ui, &s, &rows);
                    }
                    3 => {
                        kit::section(ui, "Blocked", None, None);
                        let rows: Vec<FriendRow> = s.friends.blocked.to_vec();
                        self.request_cards(ui, &s, &rows);
                    }
                    _ => {
                        if kit::section(ui, "Top connections", None, Some("See all")) {
                            self.friends_tab = 1;
                        }
                        self.top_connections(ui, &s);
                        ui.add_space(6.0);
                        let all = self.friends_tab == 1;
                        if kit::section(
                            ui,
                            if all { "All friends" } else { "Online friends" },
                            None,
                            Some("See all"),
                        ) {
                            self.friends_tab = 1;
                        }
                        self.online_grid(ui, &s, all);
                        ui.add_space(6.0);
                        kit::section(ui, "Suggested follow-ups", None, None);
                        self.follow_ups(ui, &s);
                    }
                }
                ui.add_space(12.0);
            });
    }

    fn friends_metrics(&mut self, ui: &mut Ui, s: &Snapshot) {
        let pending = s.friends.pending_incoming.len() + s.friends.pending_outgoing.len();
        let replies = s
            .inbox
            .needs_attention
            .iter()
            .filter(|i| matches!(i, InboxItem::PendingReply { .. }))
            .count();
        let cards: [(&str, kit::Tint, usize, &str, u8); 4] = [
            (
                ph::USERS,
                kit::GREEN,
                s.friends.online.len(),
                "Online friends",
                0,
            ),
            (ph::USER_PLUS, kit::PURPLE, pending, "Pending requests", 1),
            (
                ph::SQUARES_FOUR,
                kit::PURPLE,
                s.guilds.guilds.len(),
                "Active servers",
                2,
            ),
            (ph::SPARKLE, kit::TEAL, replies, "People to reply to", 3),
        ];
        let mut go = None;
        kit::card_grid(ui, 4, 72.0, 10.0, 170.0, 4, |ui, i, rect| {
            let (g, t, n, label, id) = cards[i];
            if kit::metric_card_at(
                ui,
                rect,
                ui.id().with(("fmetric", i)),
                g,
                t,
                &n.to_string(),
                label,
            )
            .clicked()
            {
                go = Some(id);
            }
        });
        match go {
            Some(0) => self.friends_tab = 0,
            Some(1) => self.friends_tab = 2,
            Some(2) => self.navigate(Destination::Servers),
            Some(_) => {
                self.inbox_filter = 1;
                self.navigate(Destination::Inbox);
            }
            None => {}
        }
    }

    /// Friends you talk to most recently, as large cards.
    fn top_connections(&mut self, ui: &mut Ui, s: &Snapshot) {
        let mut ranked: Vec<(FriendRow, Option<Timestamp>)> = s
            .friends
            .online
            .iter()
            .chain(&s.friends.offline)
            .map(|f| {
                let last = f.dm_conversation_id.and_then(|id| {
                    s.conversations
                        .conversations
                        .iter()
                        .find(|c| c.conversation_id == id)
                        .and_then(|c| c.last_activity_at)
                });
                (f.clone(), last)
            })
            .collect();
        ranked.sort_by_key(|(_, t)| std::cmp::Reverse(t.map(|t| t.as_millis())));
        ranked.truncate(4);
        if ranked.is_empty() {
            kit::empty(
                ui,
                Some(ph::USERS),
                "No friends yet",
                "Friends appear here as Discord reports them.",
            );
            return;
        }
        let now = Timestamp::now();
        let mut act: Option<(u8, usize)> = None;
        kit::card_grid(ui, ranked.len(), 188.0, 12.0, 170.0, 4, |ui, i, rect| {
            let (f, last) = &ranked[i];
            let resp = kit::card_at(ui, rect, ui.id().with(("top", i)), None);
            let name = self.display(f.alias.as_deref().unwrap_or(&f.display_name));
            let painter = ui.painter();
            theme::paint_avatar_url(
                painter,
                egui::pos2(rect.left() + 42.0, rect.top() + 42.0),
                56.0,
                &name,
                f.avatar_url.as_deref().filter(|_| !self.private()),
                theme::Presence::from_status(f.status.as_str()),
                theme::CARD,
            );
            let w = rect.width() - 28.0;
            kit::text_at(
                painter,
                egui::pos2(rect.left() + 14.0, rect.top() + 88.0),
                Align2::LEFT_CENTER,
                &name,
                theme::medium(16.0),
                theme::TEXT,
                w,
            );
            kit::text_at(
                painter,
                egui::pos2(rect.left() + 14.0, rect.top() + 110.0),
                Align2::LEFT_CENTER,
                &self.friend_status(f),
                theme::regular(13.5),
                theme::SECONDARY,
                w,
            );
            // Last conversation as a chip (A03 shows a shared server here).
            let chip = match last {
                Some(t) => list_time(*t, now),
                None => "No messages yet".into(),
            };
            let cw = (kit::text_width(painter, &chip, theme::regular(12.5)) + 36.0).min(w);
            let cr = Rect::from_min_size(
                egui::pos2(rect.left() + 14.0, rect.top() + 124.0),
                egui::vec2(cw, 24.0),
            );
            painter.rect(
                cr,
                7.0,
                theme::lerp(theme::CARD, theme::RAISED, 0.7),
                egui::Stroke::new(1.0_f32, theme::BORDER),
                egui::StrokeKind::Inside,
            );
            kit::icon(
                painter,
                cr.left_center() + egui::vec2(13.0, 0.0),
                ph::CHAT_CIRCLE,
                12.0,
                kit::BLUE.fg,
            );
            kit::text_at(
                painter,
                cr.left_center() + egui::vec2(26.0, 0.0),
                Align2::LEFT_CENTER,
                &chip,
                theme::regular(12.5),
                theme::SECONDARY,
                cw - 32.0,
            );
            let by = rect.bottom() - 22.0;
            let mw = kit::button_width(painter, Some(ph::CHAT_CIRCLE), "Message", 28.0)
                .min(rect.width() - 100.0);
            if kit::button_at(
                ui,
                Rect::from_min_size(
                    egui::pos2(rect.left() + 14.0, by - 14.0),
                    egui::vec2(mw, 28.0),
                ),
                ui.id().with(("top_msg", i)),
                kit::Kind::Secondary,
                Some(ph::CHAT_CIRCLE),
                "Message",
                f.dm_conversation_id.is_some(),
            )
            .clicked()
            {
                act = Some((0, i));
            }
            let mut x = rect.left() + 14.0 + mw + 8.0 + 16.0;
            for (k, (g, tip)) in [
                (ph::PHONE, "Calls open in Discord"),
                (ph::DOTS_THREE, "More"),
            ]
            .into_iter()
            .enumerate()
            {
                if kit::disc_at(
                    ui,
                    egui::pos2(x, by),
                    28.0,
                    ui.id().with(("top_act", i, k)),
                    g,
                    tip,
                )
                .clicked()
                {
                    act = Some((k as u8 + 1, i));
                }
                x += 36.0;
            }
            if resp.clicked() {
                act = Some((9, i));
            }
        });
        match act {
            Some((0, i)) => {
                if let Some(c) = ranked[i].0.dm_conversation_id {
                    self.open_conversation(c);
                }
            }
            Some((1, i)) => {
                let url = format!("https://discord.com/users/{}", ranked[i].0.user_id);
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
            Some((_, i)) => {
                self.selection.contact = Some(ranked[i].0.user_id);
                self.note_draft = None;
                self.request();
            }
            None => {}
        }
    }

    /// Compact two-column-plus grid of online (or all) friends.
    fn online_grid(&mut self, ui: &mut Ui, s: &Snapshot, all: bool) {
        let rows: Vec<FriendRow> = if all {
            s.friends
                .online
                .iter()
                .chain(&s.friends.offline)
                .cloned()
                .collect()
        } else {
            s.friends.online.to_vec()
        };
        if rows.is_empty() {
            kit::empty(
                ui,
                Some(ph::USERS),
                "Nobody is online",
                "Friends appear here when they come online.",
            );
            return;
        }
        let mut pick = None;
        kit::card_grid(
            ui,
            rows.len().min(8),
            58.0,
            10.0,
            180.0,
            4,
            |ui, i, rect| {
                let f = &rows[i];
                let resp = kit::card_at(ui, rect, ui.id().with(("ogrid", i)), None);
                let name = self.display(f.alias.as_deref().unwrap_or(&f.display_name));
                let painter = ui.painter();
                theme::paint_avatar_url(
                    painter,
                    egui::pos2(rect.left() + 28.0, rect.center().y),
                    38.0,
                    &name,
                    f.avatar_url.as_deref().filter(|_| !self.private()),
                    theme::Presence::from_status(f.status.as_str()),
                    theme::CARD,
                );
                let x = rect.left() + 56.0;
                let w = rect.right() - x - 26.0;
                kit::text_at(
                    painter,
                    egui::pos2(x, rect.center().y - 9.0),
                    Align2::LEFT_CENTER,
                    &name,
                    theme::medium(14.0),
                    theme::TEXT,
                    w,
                );
                kit::text_at(
                    painter,
                    egui::pos2(x, rect.center().y + 10.0),
                    Align2::LEFT_CENTER,
                    &self.friend_status(f),
                    theme::regular(12.5),
                    theme::MUTED,
                    w,
                );
                kit::chevron(
                    painter,
                    egui::pos2(rect.right() - 14.0, rect.center().y),
                    theme::MUTED,
                );
                if resp.clicked() {
                    pick = Some(f.user_id);
                }
            },
        );
        if let Some(u) = pick {
            self.selection.contact = Some(u);
            self.note_draft = None;
            self.request();
        }
    }

    /// Teal suggestions from replies you owe; red for the oldest one.
    fn follow_ups(&mut self, ui: &mut Ui, s: &Snapshot) {
        let now = Timestamp::now();
        let mut replies: Vec<(litecord_types::ConversationId, String, String, Timestamp)> = s
            .inbox
            .needs_attention
            .iter()
            .filter_map(|i| match i {
                InboxItem::PendingReply {
                    conversation_id,
                    from,
                    preview,
                    at,
                } => Some((
                    *conversation_id,
                    self.display(from),
                    self.display(preview),
                    *at,
                )),
                _ => None,
            })
            .collect();
        replies.sort_by_key(|r| std::cmp::Reverse(r.3.as_millis()));
        if replies.is_empty() {
            kit::empty(
                ui,
                Some(ph::CHECK_CIRCLE),
                "No follow-ups",
                "Nobody is waiting on a reply from you.",
            );
            return;
        }
        // The oldest reply owed gets the red high-priority treatment.
        let oldest = replies.len() - 1;
        let n = replies.len().min(4);
        let shown: Vec<usize> = if replies.len() > 4 {
            vec![0, 1, 2, oldest]
        } else {
            (0..n).collect()
        };
        let mut act: Option<(bool, usize)> = None;
        kit::card_grid(ui, shown.len(), 150.0, 12.0, 170.0, 4, |ui, k, rect| {
            let i = shown[k];
            let (_, from, preview, at) = &replies[i];
            let red = i == oldest && replies.len() > 1;
            let accent = if red { theme::PRIORITY } else { theme::OMNI };
            let resp = kit::card_at(ui, rect, ui.id().with(("follow", k)), Some(accent));
            let painter = ui.painter();
            kit::paint_bubble(
                painter,
                egui::pos2(rect.left() + 30.0, rect.top() + 32.0),
                if red {
                    ph::CALENDAR_BLANK
                } else {
                    ph::CHAT_CIRCLE
                },
                if red { kit::RED } else { kit::TEAL },
                42.0,
            );
            let x = rect.left() + 60.0;
            let w = rect.right() - x - 10.0;
            let title = if red {
                format!("Follow up with {from}")
            } else {
                from.clone()
            };
            let tc = if red {
                theme::PRIORITY_TEXT
            } else {
                theme::TEXT
            };
            kit::text_at(
                painter,
                egui::pos2(x, rect.top() + 22.0),
                Align2::LEFT_CENTER,
                &title,
                theme::medium(14.0),
                tc,
                w,
            );
            let sub = if red {
                format!("Waiting on you since {}", list_time(*at, now))
            } else {
                format!("messaged you · {}", list_time(*at, now))
            };
            kit::text_at(
                painter,
                egui::pos2(x, rect.top() + 42.0),
                Align2::LEFT_CENTER,
                &sub,
                theme::regular(12.5),
                theme::MUTED,
                w,
            );
            let q = format!("\u{201c}{preview}\u{201d}");
            let g = kit::wrapped(
                painter,
                &q,
                theme::regular(13.0),
                theme::SECONDARY,
                rect.width() - 28.0,
                2,
            );
            painter.galley(
                egui::pos2(rect.left() + 14.0, rect.top() + 60.0),
                g,
                theme::SECONDARY,
            );
            let label = if red { "Add reminder" } else { "Message" };
            let bw = (rect.width() - 60.0).min(160.0);
            let br = Rect::from_min_size(
                egui::pos2(rect.center().x - bw * 0.5 - 8.0, rect.bottom() - 42.0),
                egui::vec2(bw, 30.0),
            );
            if kit::button_at(
                ui,
                br,
                ui.id().with(("follow_btn", k)),
                if red {
                    kit::Kind::Danger
                } else {
                    kit::Kind::Omni
                },
                None,
                label,
                !self.busy,
            )
            .clicked()
            {
                act = Some((red, i));
            } else if resp.clicked() {
                act = Some((false, i));
            }
            kit::chevron(
                ui.painter(),
                egui::pos2(br.right() + 16.0, br.center().y),
                if red {
                    theme::PRIORITY_TEXT
                } else {
                    theme::MUTED
                },
            );
        });
        match act {
            Some((true, i)) => {
                let (conv, from, _, _) = &replies[i];
                self.send(Command::CreateTask(litecord_types::tasks::TaskDraft {
                    title: format!("Reply to {from}"),
                    description: None,
                    priority: litecord_types::tasks::TaskPriority::High,
                    due_at: Some(Timestamp::from_millis(now.as_millis() + 3 * 3_600_000)),
                    related_users: Vec::new(),
                    conversation_id: Some(*conv),
                    parent_id: None,
                    source: None,
                }));
            }
            Some((false, i)) => self.open_conversation(replies[i].0),
            None => {}
        }
    }

    /// Pending requests (accept/decline) and blocked users (unblock).
    fn request_cards(&mut self, ui: &mut Ui, s: &Snapshot, rows: &[FriendRow]) {
        if rows.is_empty() {
            kit::empty(
                ui,
                Some(ph::USERS),
                "Nobody here",
                "People in this view appear as Discord reports them.",
            );
            return;
        }
        let enabled = !self.busy && !self.private() && s.diagnostics.session.is_online();
        let requests = enabled
            && s.diagnostics
                .capabilities
                .is_usable(Capability::FriendRequests);
        let blocking = enabled && s.diagnostics.capabilities.is_usable(Capability::Blocking);
        let incoming = rows
            .iter()
            .any(|r| r.relationship == RelationshipKind::PendingIncoming);
        if incoming && !requests && !self.busy {
            self.requests_disabled_note(ui, s);
            ui.add_space(8.0);
        }
        let mut act: Option<(usize, RelationshipAction, bool)> = None;
        kit::card_grid(ui, rows.len(), 76.0, 10.0, 300.0, 2, |ui, i, rect| {
            let r = &rows[i];
            let resp = kit::card_at(ui, rect, ui.id().with(("req", i)), None);
            let name = self.display(r.alias.as_deref().unwrap_or(&r.display_name));
            let painter = ui.painter();
            theme::paint_avatar_url(
                painter,
                egui::pos2(rect.left() + 34.0, rect.center().y),
                44.0,
                &name,
                r.avatar_url.as_deref().filter(|_| !self.private()),
                theme::Presence::None,
                theme::CARD,
            );
            let x = rect.left() + 66.0;
            let buttons: Vec<(&str, kit::Kind, RelationshipAction, bool, bool)> =
                match r.relationship {
                    RelationshipKind::PendingIncoming => vec![
                        (
                            "Accept",
                            kit::Kind::Primary,
                            RelationshipAction::AcceptFriendRequest,
                            requests,
                            false,
                        ),
                        (
                            "Decline",
                            kit::Kind::Secondary,
                            RelationshipAction::RejectFriendRequest,
                            requests,
                            true,
                        ),
                    ],
                    RelationshipKind::Blocked => vec![(
                        "Unblock",
                        kit::Kind::Secondary,
                        RelationshipAction::Unblock,
                        blocking,
                        true,
                    )],
                    _ => vec![],
                };
            let mut bx = rect.right() - 14.0;
            let mut rects = Vec::new();
            for (label, _, _, _, _) in &buttons {
                let w = kit::button_width(painter, None, label, 30.0);
                rects.push(Rect::from_min_size(
                    egui::pos2(bx - w, rect.center().y - 15.0),
                    egui::vec2(w, 30.0),
                ));
                bx -= w + 8.0;
            }
            let w = bx - x - 8.0;
            kit::text_at(
                painter,
                egui::pos2(x, rect.center().y - 9.0),
                Align2::LEFT_CENTER,
                &name,
                theme::medium(15.0),
                theme::TEXT,
                w,
            );
            kit::text_at(
                painter,
                egui::pos2(x, rect.center().y + 11.0),
                Align2::LEFT_CENTER,
                &self.friend_status(r),
                theme::regular(13.0),
                theme::MUTED,
                w,
            );
            for (k, ((label, kind, action, en, confirm), br)) in
                buttons.iter().zip(rects).enumerate()
            {
                if kit::button_at(
                    ui,
                    br,
                    ui.id().with(("req_btn", i, k)),
                    *kind,
                    None,
                    label,
                    *en,
                )
                .clicked()
                {
                    act = Some((i, *action, *confirm));
                }
            }
            if resp.clicked() {
                self.selection.contact = Some(r.user_id);
                self.note_draft = None;
                self.request();
            }
        });
        if let Some((i, action, confirm)) = act {
            let r = &rows[i];
            if confirm {
                self.relationship_confirmation = Some((r.user_id, action, r.display_name.clone()));
            } else {
                self.send(Command::Relationship(r.user_id, action));
            }
        }
    }
}

impl Workspace {
    /// Why Accept/Decline are unavailable, so answering a request never
    /// looks like it has to happen in Discord.
    fn requests_disabled_note(&mut self, ui: &mut Ui, s: &Snapshot) {
        let account =
            s.diagnostics.backend_mode == litecord_types::capability::BackendMode::UserSession;
        let (text, settings) = if self.private() {
            (
                "Privacy mode is on. Turn it off to answer friend requests.",
                false,
            )
        } else if !s.diagnostics.session.is_online() {
            (
                "Litecord is offline. Requests can be answered once it reconnects.",
                false,
            )
        } else if account && s.settings.can_enable_account_writes {
            (
                "Answering friend requests from Litecord needs account writes, which are off. \
                 You can allow them in Settings (read the warning there first).",
                true,
            )
        } else if account {
            (
                "This build opens your account read-only, so friend requests are answered in \
                 Discord: select a request and choose Open profile in Discord.",
                false,
            )
        } else {
            (
                "This Discord connection cannot answer friend requests.",
                false,
            )
        };
        kit::card(ui, |ui| {
            ui.horizontal(|ui| {
                kit::bubble(ui, ph::INFO, kit::BLUE, 32.0);
                ui.vertical(|ui| {
                    ui.set_width((ui.available_width() - 130.0).max(120.0));
                    kit::para(ui, text, theme::regular(14.0), theme::BODY);
                });
                if settings {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if kit::button_ex(
                            ui,
                            kit::Kind::Secondary,
                            Some(ph::GEAR_SIX),
                            "Settings",
                            32.0,
                            true,
                        )
                        .clicked()
                        {
                            self.navigate(Destination::Settings);
                        }
                    });
                }
            });
        });
    }
}

/// Pill with a colored count badge (A03: blue Online, red Pending).
fn pill_colored(
    ui: &mut Ui,
    label: &str,
    n: usize,
    color: egui::Color32,
    selected: bool,
) -> egui::Response {
    let font = theme::medium(14.0);
    let text_w = kit::text_width(ui.painter(), label, font.clone());
    let badge_w = if n > 0 { 26.0 } else { 0.0 };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(text_w + 28.0 + badge_w, 32.0),
        egui::Sense::click(),
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let (fill, stroke) = if selected {
            (
                theme::lerp(theme::SELECTED_SOFT, theme::PRIMARY, 0.18),
                theme::lerp(theme::SELECTED_SOFT, theme::PRIMARY, 0.55),
            )
        } else if response.hovered() {
            (theme::HOVER, theme::BORDER)
        } else {
            (
                theme::lerp(theme::SIDEBAR, theme::RAISED, 0.5),
                theme::BORDER,
            )
        };
        painter.rect(
            rect,
            9.0,
            fill,
            egui::Stroke::new(1.0_f32, stroke),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.left_center() + egui::vec2(14.0, 0.0),
            Align2::LEFT_CENTER,
            label,
            font,
            if selected {
                theme::TEXT
            } else {
                theme::SECONDARY
            },
        );
        if n > 0 {
            kit::badge(
                painter,
                egui::pos2(rect.right() - 22.0, rect.center().y),
                n,
                color,
            );
        }
    }
    let label = label.to_owned();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label)
    });
    response
}
