//! Messages center panel: header, virtualized message list and composer.
//!
//! Row heights come from one measuring function used for both layout and
//! painting, so rows never overlap however long names, text or attachment
//! lists are.

use std::sync::Arc;

use eframe::egui::{self, Align2, Galley, Rect, Sense, Stroke, Ui};
use litecord_app::view::{ConversationViewModel, MessageRow};
use litecord_types::provenance::DiscordIdentity;
use litecord_types::social::MessageExtra;
use litecord_types::Timestamp;

use crate::bridge::Command;
use crate::workspace::Workspace;
use crate::{kit, ph, theme};

const AVATAR: f32 = 44.0;
/// Left inset of the message column inside the chat panel.
const INSET: f32 = 22.0;
/// Text column offset from the row's left edge (avatar + gap).
const GUTTER: f32 = AVATAR + 16.0;
const HEADER_LINE: f32 = 26.0;
const BODY_LINE: f32 = 23.0;
const ATTACHMENT: f32 = 66.0;
const DAY_SEPARATOR: f32 = 40.0;
/// Consecutive messages from one author within this window are grouped.
const GROUP_MS: i64 = 5 * 60_000;

struct Layout {
    day: Option<String>,
    continuation: bool,
    galley: Arc<Galley>,
    height: f32,
}

impl Workspace {
    pub(crate) fn chat_view(&mut self, ui: &mut Ui) {
        let Some(s) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        let Some(chat) = s.chat.clone() else {
            let mut inner =
                ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect().shrink(22.0)));
            kit::page_title(&mut inner, "Messages", None);
            kit::empty(
                &mut inner,
                Some(ph::CHAT_CIRCLE),
                "No conversation selected",
                "Choose a conversation on the left to read and reply.",
            );
            return;
        };
        if s.selection.conversation != self.selection.conversation
            || s.selection.before != self.selection.before
        {
            let mut inner =
                ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect().shrink(22.0)));
            inner.spinner();
            inner.label(theme::meta("Loading conversation…"));
            return;
        }
        let private = self.private();
        let id = chat.conversation_id;
        egui::Panel::top(egui::Id::new(("chat_header", id)))
            .exact_size(76.0)
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| self.chat_header(ui, &s, &chat, private));
        egui::Panel::bottom(egui::Id::new(("chat_composer", id)))
            .frame(egui::Frame::NONE.inner_margin(egui::Margin {
                left: 22,
                right: 22,
                top: 6,
                bottom: 12,
            }))
            .show_inside(ui, |ui| self.composer(ui, &chat));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                if chat.has_more || self.selection.before.is_some() {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.add_space(INSET);
                        if chat.has_more
                            && kit::button_ex(ui, kit::Kind::Secondary, Some(ph::ARROW_UP_RIGHT), "Load older messages", 28.0, true)
                                .clicked()
                        {
                            self.selection.before = chat.messages.first().map(|m| m.sent_at);
                            self.request();
                        }
                        if self.selection.before.is_some()
                            && kit::button_ex(ui, kit::Kind::Ghost, None, "Jump to latest", 28.0, true)
                                .clicked()
                        {
                            self.selection.before = None;
                            self.request();
                        }
                    });
                }
                egui::ScrollArea::vertical()
                    .id_salt(("messages", id, self.selection.before))
                    .auto_shrink([false, false])
                    .stick_to_bottom(self.selection.before.is_none())
                    .show_viewport(ui, |ui, viewport| {
                        let width = ui.available_width();
                        let top = ui.cursor().min;
                        let layouts = measure(ui, &chat, width, private);
                        let mut y = 10.0;
                        for (row, layout) in chat.messages.iter().zip(layouts) {
                            let h = layout.height;
                            if y + h >= viewport.min.y && y <= viewport.max.y {
                                let rect = Rect::from_min_size(
                                    top + egui::vec2(0.0, y),
                                    egui::vec2(width, h),
                                );
                                self.paint_row(ui, rect, &chat, row, layout, private);
                            }
                            y += h;
                        }
                        ui.allocate_space(egui::vec2(width, y + 10.0));
                        if chat.messages.is_empty() {
                            ui.add_space(12.0);
                            ui.horizontal(|ui| {
                                ui.add_space(INSET);
                                ui.vertical(|ui| {
                                    kit::empty(
                                        ui,
                                        Some(ph::CHAT_CIRCLE_DOTS),
                                        "No messages cached yet",
                                        "Recent history loads in the background when Discord provides it.",
                                    )
                                });
                            });
                        }
                    });
            });
    }

    /// A01 chat header: avatar, title and status, icon actions on the right.
    fn chat_header(
        &mut self,
        ui: &mut Ui,
        s: &crate::bridge::Snapshot,
        chat: &ConversationViewModel,
        private: bool,
    ) {
        let rect = ui.max_rect();
        ui.painter().rect_filled(
            Rect::from_min_max(
                egui::pos2(rect.left(), rect.bottom() - 1.0),
                rect.right_bottom(),
            ),
            0.0,
            theme::DIVIDER,
        );
        let title = self.display(&chat.title);
        let group = s
            .conversations
            .conversations
            .iter()
            .find(|r| r.conversation_id == chat.conversation_id)
            .map(|r| r.kind);
        let presence = s
            .contact
            .as_ref()
            .map(|c| theme::Presence::from_status(c.presence.status.as_str()))
            .unwrap_or(theme::Presence::None);
        let avatar_c = egui::pos2(rect.left() + INSET + 24.0, rect.center().y);
        match group {
            Some(litecord_types::social::ConversationKind::GroupDm) => {
                kit::paint_group(ui.painter(), avatar_c, 48.0, ph::USERS, theme::SECONDARY)
            }
            Some(litecord_types::social::ConversationKind::GuildChannel) => {
                kit::paint_group(ui.painter(), avatar_c, 48.0, ph::HASH, theme::SECONDARY)
            }
            _ => theme::paint_avatar(
                ui.painter(),
                avatar_c,
                48.0,
                &title,
                presence,
                theme::WORKSPACE,
            ),
        }
        let status = s.contact.as_ref().map_or_else(
            || match group {
                Some(litecord_types::social::ConversationKind::GuildChannel) => {
                    "Server channel".to_owned()
                }
                Some(litecord_types::social::ConversationKind::GroupDm) => {
                    "Group conversation".to_owned()
                }
                _ => "Conversation".to_owned(),
            },
            |c| match (&c.presence.activity, private) {
                (Some(a), false) => format!("{} · {}", presence.title(), a.name),
                _ => presence.title().to_owned(),
            },
        );
        // Right-side actions first so the title can elide before them.
        let mut actions = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(Rect::from_min_max(
                    egui::pos2(rect.center().x, rect.top()),
                    egui::pos2(rect.right() - INSET + 6.0, rect.bottom()),
                ))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        actions.spacing_mut().item_spacing.x = 8.0;
        let more = kit::icon_button_ex(
            &mut actions,
            ph::DOTS_THREE,
            "More",
            38.0,
            theme::SECONDARY,
            true,
        );
        egui::Popup::menu(&more).show(|ui| {
            ui.set_min_width(220.0);
            if ui.button("Open in Discord").clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(
                    chat.capabilities.open_in_discord_url.clone(),
                ));
            }
            if ui.button("Ask Omni to catch me up").clicked() {
                self.omni_open = true;
                self.omni_draft = format!("Catch me up on my conversation with {}.", chat.title);
            }
            if chat.has_more && ui.button("Load older messages").clicked() {
                self.selection.before = chat.messages.first().map(|m| m.sent_at);
                self.request();
            }
        });
        if kit::icon_button_ex(
            &mut actions,
            ph::MAGNIFYING_GLASS,
            "Search (Ctrl+K)",
            38.0,
            theme::SECONDARY,
            true,
        )
        .clicked()
        {
            self.palette_open = true;
            self.palette_focus_requested = true;
        }
        if kit::icon_button_ex(
            &mut actions,
            ph::SPARKLE,
            "Ask Omni about this conversation",
            38.0,
            theme::OMNI,
            true,
        )
        .clicked()
        {
            self.omni_open = true;
            self.omni_draft = format!("Catch me up on my conversation with {}.", chat.title);
        }
        // Calls are not part of Litecord's supported surface: they open in
        // Discord, and say so.
        if kit::icon_button_ex(
            &mut actions,
            ph::VIDEO_CAMERA,
            "Video calls open in Discord",
            38.0,
            theme::SECONDARY,
            true,
        )
        .clicked()
            || kit::icon_button_ex(
                &mut actions,
                ph::PHONE,
                "Voice calls open in Discord",
                38.0,
                theme::SECONDARY,
                true,
            )
            .clicked()
        {
            ui.ctx().open_url(egui::OpenUrl::new_tab(
                chat.capabilities.open_in_discord_url.clone(),
            ));
        }
        if chat.capabilities.send_identity == Some(DiscordIdentity::ApplicationBot) {
            kit::status_pill(
                &mut actions,
                Some(ph::ROBOT),
                "Posting as your bot",
                theme::WARNING,
            );
        }
        let used = actions.min_rect().left();
        let text_x = avatar_c.x + 38.0;
        let text_w = (used - text_x - 12.0).max(40.0);
        let painter = ui.painter();
        kit::text_at(
            painter,
            egui::pos2(text_x, rect.center().y - 10.0),
            Align2::LEFT_CENTER,
            &title,
            theme::semibold(18.0),
            theme::TEXT,
            text_w,
        );
        kit::text_at(
            painter,
            egui::pos2(text_x, rect.center().y + 13.0),
            Align2::LEFT_CENTER,
            &status,
            theme::regular(14.0),
            theme::SECONDARY,
            text_w,
        );
    }

    fn paint_row(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        chat: &ConversationViewModel,
        row: &MessageRow,
        layout: Layout,
        private: bool,
    ) {
        let mut body = rect;
        if let Some(day) = &layout.day {
            let y = rect.top() + DAY_SEPARATOR / 2.0;
            let painter = ui.painter();
            let text = painter.layout_no_wrap(day.clone(), theme::medium(12.0), theme::MUTED);
            let w = text.size().x + 24.0;
            let l = rect.left() + INSET;
            let r = rect.right() - INSET;
            let cx = rect.center().x;
            painter.line_segment(
                [egui::pos2(l, y), egui::pos2(cx - w * 0.5 - 6.0, y)],
                Stroke::new(1.0_f32, theme::DIVIDER),
            );
            painter.line_segment(
                [egui::pos2(cx + w * 0.5 + 6.0, y), egui::pos2(r, y)],
                Stroke::new(1.0_f32, theme::DIVIDER),
            );
            painter.galley(
                egui::pos2(cx - text.size().x * 0.5, y - text.size().y * 0.5),
                text,
                theme::MUTED,
            );
            body.min.y += DAY_SEPARATOR;
        }
        let row_rect = Rect::from_min_max(
            egui::pos2(body.left() + 8.0, body.top()),
            egui::pos2(body.right() - 8.0, body.bottom()),
        );
        let response = ui.interact(row_rect, ui.id().with(row.message_id), Sense::click());
        if response.hovered() || row.render.highlighted {
            let fill = if row.render.highlighted {
                theme::WARNING.gamma_multiply(0.08)
            } else {
                theme::lerp(theme::WORKSPACE, theme::HOVER, 0.45)
            };
            ui.painter().rect_filled(row_rect, 8.0, fill);
        }
        let name = if private {
            "Hidden user".to_owned()
        } else if row.is_mine {
            "You".to_owned()
        } else {
            row.render.author_display.clone()
        };
        let left = body.left() + INSET;
        let text_x = left + GUTTER;
        let mut y = body.top() + if layout.continuation { 2.0 } else { 12.0 };
        let painter = ui.painter();
        if !layout.continuation {
            theme::paint_avatar(
                painter,
                egui::pos2(left + AVATAR * 0.5, y + AVATAR * 0.5),
                AVATAR,
                if row.is_mine && !private {
                    &row.render.author_display
                } else {
                    &name
                },
                theme::Presence::None,
                theme::WORKSPACE,
            );
            let time = clock(row.sent_at) + if row.edited { " · edited" } else { "" };
            let time_w = kit::text_width(painter, &time, theme::regular(13.0));
            let max_name = (body.right() - INSET - text_x - time_w - 14.0).max(24.0);
            let name_rect = kit::text_at(
                painter,
                egui::pos2(text_x, y + 10.0),
                Align2::LEFT_CENTER,
                &name,
                theme::semibold(16.0),
                theme::TEXT,
                max_name,
            );
            painter.text(
                egui::pos2(name_rect.right() + 10.0, y + 11.0),
                Align2::LEFT_CENTER,
                time,
                theme::regular(13.0),
                theme::FAINT,
            );
            y += HEADER_LINE;
        } else if response.hovered() {
            painter.text(
                egui::pos2(left + AVATAR * 0.5, y + BODY_LINE * 0.5),
                Align2::CENTER_CENTER,
                short_clock(row.sent_at),
                theme::regular(11.0),
                theme::FAINT,
            );
        }
        let text_height = layout.galley.size().y;
        painter.galley(egui::pos2(text_x, y), layout.galley, theme::BODY);
        y += text_height + 6.0;
        if !private {
            for extra in &row.extras {
                let card = Rect::from_min_size(
                    egui::pos2(text_x, y),
                    egui::vec2(
                        (body.right() - INSET - text_x).min(460.0),
                        ATTACHMENT - 10.0,
                    ),
                );
                self.extra_card(ui, card, extra, &message_url(chat, row));
                y += ATTACHMENT;
            }
        }
        if row.bookmarked {
            kit::icon(
                ui.painter(),
                egui::pos2(body.right() - INSET - 8.0, body.top() + 22.0),
                ph::BOOKMARK_SIMPLE,
                15.0,
                theme::OMNI,
            );
        }
        self.message_menu(&response, chat, row, private);
    }

    /// Attachment/embed metadata. Litecord does not download media, so this
    /// is a description with a link, never a fake preview.
    fn extra_card(&mut self, ui: &mut Ui, rect: Rect, extra: &MessageExtra, url: &str) {
        let (title, detail) = extra_description(extra);
        let response = ui.interact(
            rect,
            ui.id()
                .with(("extra", rect.min.x as i64, rect.min.y as i64)),
            Sense::click(),
        );
        let painter = ui.painter();
        kit::paint_card(painter, rect, response.hovered());
        let tile = Rect::from_center_size(
            egui::pos2(rect.left() + 30.0, rect.center().y),
            egui::vec2(38.0, 38.0),
        );
        let (glyph, tint) = match extra {
            MessageExtra::Attachment { content_type, .. }
                if content_type
                    .as_deref()
                    .is_some_and(|c| c.starts_with("image/")) =>
            {
                (ph::IMAGE, kit::PURPLE)
            }
            MessageExtra::Attachment { .. } => (ph::FILE_TEXT, kit::BLUE),
            MessageExtra::Embed { .. } => (ph::LINK, kit::TEAL),
            MessageExtra::VoiceMessage => (ph::WAVEFORM, kit::GREEN),
            MessageExtra::Poll => (ph::LIST_BULLETS, kit::ORANGE),
            _ => (ph::ARROW_SQUARE_OUT, kit::GREY),
        };
        kit::paint_tile(painter, tile, glyph, tint, 9.0);
        let tx = tile.right() + 14.0;
        let tw = (rect.right() - tx - 50.0).max(30.0);
        kit::text_at(
            painter,
            egui::pos2(tx, rect.center().y - 9.0),
            Align2::LEFT_CENTER,
            &title,
            theme::medium(15.0),
            theme::TEXT,
            tw,
        );
        kit::text_at(
            painter,
            egui::pos2(tx, rect.center().y + 11.0),
            Align2::LEFT_CENTER,
            &detail,
            theme::regular(13.0),
            theme::MUTED,
            tw,
        );
        kit::icon_o(
            painter,
            egui::pos2(rect.right() - 26.0, rect.center().y),
            ph::ARROW_SQUARE_OUT,
            18.0,
            if response.hovered() {
                theme::PRIMARY_TEXT
            } else {
                theme::SECONDARY
            },
        );
        if response
            .on_hover_text("Not downloaded by Litecord; opens the message in Discord")
            .clicked()
        {
            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
        }
    }
    fn message_menu(
        &mut self,
        response: &egui::Response,
        chat: &ConversationViewModel,
        row: &MessageRow,
        private: bool,
    ) {
        response.context_menu(|ui| {
            let reply = ui
                .add_enabled(
                    chat.capabilities.can_reply && !self.busy,
                    egui::Button::new("Reply"),
                )
                .on_disabled_hover_text(
                    "Replies need the bot identity; the Social SDK sends plain messages only",
                );
            if reply.clicked() {
                self.replying = Some((
                    chat.conversation_id,
                    row.message_id,
                    row.render.author_display.clone(),
                ));
                ui.memory_mut(|m| {
                    m.request_focus(egui::Id::new(("composer", chat.conversation_id)))
                });
                ui.close();
            }
            if ui.button("Open in Discord").clicked() {
                ui.ctx()
                    .open_url(egui::OpenUrl::new_tab(message_url(chat, row)));
                ui.close();
            }
            for action in &row.actions {
                let reveals = action.intents.iter().any(|i| {
                    matches!(
                        i,
                        litecord_features::intent::AppIntent::CopyToClipboard { .. }
                    )
                });
                if ui
                    .add_enabled(
                        !(self.busy || private && reveals),
                        egui::Button::new(&action.label),
                    )
                    .clicked()
                {
                    self.send(Command::Intents(action.intents.clone()));
                    ui.close();
                }
            }
            if row.is_mine && !private {
                ui.separator();
                if ui
                    .add_enabled(
                        chat.capabilities.can_edit && !self.busy,
                        egui::Button::new("Edit message"),
                    )
                    .clicked()
                {
                    self.editing_message = Some((row.message_id, row.render.content.clone()));
                    ui.close();
                }
                if ui
                    .add_enabled(
                        chat.capabilities.can_delete && !self.busy,
                        egui::Button::new("Delete message"),
                    )
                    .clicked()
                {
                    self.deleting_message = Some(row.message_id);
                    ui.close();
                }
            }
        });
    }

    fn composer(&mut self, ui: &mut Ui, chat: &ConversationViewModel) {
        let private = self.private();
        let id = chat.conversation_id;
        let identity = chat.capabilities.send_identity;
        let can_send_here = identity.is_some() && chat.capabilities.can_send;
        let title = chat.title.clone();
        let hint = match (private, can_send_here) {
            (true, _) => "Message (privacy mode)".to_owned(),
            (false, false) => "Sending is not available here".to_owned(),
            (false, true) => format!("Message {title}…"),
        };
        let busy = self.busy;
        let reply_to = self
            .replying
            .as_ref()
            .filter(|r| r.0 == id && chat.capabilities.can_reply)
            .map(|r| (r.1, r.2.clone()));
        if let Some((_, author)) = &reply_to {
            let (bar, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), Sense::hover());
            let painter = ui.painter();
            kit::icon(
                painter,
                bar.left_center() + egui::vec2(10.0, 0.0),
                ph::ARROW_BEND_UP_LEFT,
                15.0,
                theme::PRIMARY_TEXT,
            );
            let who = if private {
                "a message"
            } else {
                author.as_str()
            };
            kit::text_at(
                painter,
                bar.left_center() + egui::vec2(26.0, 0.0),
                Align2::LEFT_CENTER,
                &format!("Replying to {who}"),
                theme::regular(13.0),
                theme::SECONDARY,
                bar.width() - 70.0,
            );
            let close = Rect::from_center_size(
                bar.right_center() - egui::vec2(14.0, 0.0),
                egui::vec2(26.0, 26.0),
            );
            let resp = ui.interact(close, ui.id().with("cancel_reply"), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(close, 6.0, theme::HOVER);
            }
            kit::icon(ui.painter(), close.center(), ph::X, 13.0, theme::SECONDARY);
            if resp.on_hover_text("Cancel reply").clicked() {
                self.replying = None;
            }
        }
        let mut submit = false;
        let mut clicked = false;
        let mut omni = false;
        let draft_len;
        {
            let draft = self.drafts.entry(id).or_default();
            draft_len = draft.trim().chars().count();
            egui::Frame::new()
                .fill(theme::FIELD)
                .stroke(Stroke::new(1.0_f32, theme::BORDER))
                .corner_radius(12)
                .inner_margin(egui::Margin {
                    left: 8,
                    right: 8,
                    top: 7,
                    bottom: 7,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let plus = kit::disc_button(
                            ui,
                            ph::PLUS,
                            "Attachments are sent from Discord",
                            32.0,
                        );
                        if plus.clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(
                                chat.capabilities.open_in_discord_url.clone(),
                            ));
                        }
                        let w = ui.available_width() - 84.0;
                        let response = ui.add_enabled(
                            !busy && can_send_here,
                            egui::TextEdit::multiline(draft)
                                .frame(egui::Frame::NONE)
                                .font(theme::regular(16.0))
                                .text_color(theme::TEXT)
                                .password(private)
                                .id_salt(("composer", id))
                                .hint_text(
                                    egui::RichText::new(hint)
                                        .font(theme::regular(16.0))
                                        .color(theme::MUTED),
                                )
                                .desired_rows(1)
                                .desired_width(w),
                        );
                        // Enter sends; Shift+Enter inserts a newline.
                        submit = response.has_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
                        let ready = !busy && can_send_here && draft_len > 0 && draft_len <= 2000;
                        omni = kit::icon_button_ex(
                            ui,
                            ph::SPARKLE,
                            "Ask Omni to draft a reply",
                            34.0,
                            theme::OMNI,
                            true,
                        )
                        .clicked();
                        clicked = kit::icon_button_ex(
                            ui,
                            ph::PAPER_PLANE_RIGHT,
                            "Send (Enter)",
                            34.0,
                            if ready {
                                theme::PRIMARY_TEXT
                            } else {
                                theme::MUTED
                            },
                            ready,
                        )
                        .clicked();
                    });
                });
        }
        if omni {
            self.omni_open = true;
            self.omni_draft = format!("Draft a reply for my conversation with {title}.");
        }
        let text = self
            .drafts
            .get(&id)
            .map(|d| d.trim_end_matches('\n').to_owned())
            .unwrap_or_default();
        let count = text.chars().count();
        // The sending identity is always shown; bot and user are never
        // interchangeable.
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let (glyph, who) = match identity {
                Some(DiscordIdentity::ApplicationBot) => (ph::ROBOT, "Sending as your bot"),
                Some(DiscordIdentity::UserSocialSdk) => (ph::USER, "Sending as you"),
                Some(DiscordIdentity::UserSession) => (ph::LOCK_SIMPLE, "User session (read only)"),
                None => (ph::LOCK_SIMPLE, "Read only here · Open in Discord to reply"),
            };
            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
            kit::icon(ui.painter(), r.center(), glyph, 12.0, theme::FAINT);
            ui.label(
                egui::RichText::new(who)
                    .font(theme::regular(12.0))
                    .color(theme::FAINT),
            );
            if count > 1800 {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!("{count}/2000"))
                            .font(theme::regular(12.0))
                            .color(if count > 2000 {
                                theme::PRIORITY
                            } else {
                                theme::MUTED
                            }),
                    );
                });
            }
        });
        let can_send = !busy && can_send_here && !text.trim().is_empty() && count <= 2000;
        if can_send && (clicked || submit) {
            if let Some(identity) = identity {
                match reply_to {
                    Some((message_id, _)) => {
                        self.send(Command::ReplyAs(id, message_id, text, identity))
                    }
                    None => self.send(Command::SendAs(id, text, identity)),
                }
            }
        } else if submit {
            if let Some(d) = self.drafts.get_mut(&id) {
                *d = d.trim_end_matches('\n').to_owned();
            }
        }
    }
}

fn measure(ui: &Ui, chat: &ConversationViewModel, width: f32, private: bool) -> Vec<Layout> {
    let mut out = Vec::with_capacity(chat.messages.len());
    let mut prev: Option<&MessageRow> = None;
    let text_w = (width - INSET * 2.0 - GUTTER).max(40.0);
    for row in &chat.messages {
        let day = day_label(row.sent_at);
        let new_day = prev.is_none_or(|p| day_label(p.sent_at) != day);
        let continuation = !new_day
            && prev.is_some_and(|p| {
                p.author_id == row.author_id
                    && row.sent_at.as_millis() - p.sent_at.as_millis() < GROUP_MS
            });
        let text = if private {
            "Hidden in privacy mode".to_owned()
        } else {
            row.render.content.clone()
        };
        let mut job = egui::text::LayoutJob::single_section(
            text,
            egui::TextFormat {
                font_id: theme::regular(16.0),
                color: theme::BODY,
                line_height: Some(BODY_LINE),
                ..Default::default()
            },
        );
        job.wrap.max_width = text_w;
        let galley = ui.painter().layout_job(job);
        let extras = if private { 0 } else { row.extras.len() };
        let mut height = galley.size().y + 8.0 + extras as f32 * ATTACHMENT;
        if !continuation {
            height += HEADER_LINE + 14.0;
            height = height.max(AVATAR + 22.0);
        }
        if new_day {
            height += DAY_SEPARATOR;
        }
        out.push(Layout {
            day: new_day.then_some(day),
            continuation,
            galley,
            height,
        });
        prev = Some(row);
    }
    out
}

/// Short text for an attachment/embed card: (title, detail).
pub(crate) fn extra_description(extra: &MessageExtra) -> (String, String) {
    match extra {
        MessageExtra::Attachment {
            filename,
            content_type,
            size_bytes,
        } => (
            filename.clone(),
            format!(
                "{} · {}",
                content_type.as_deref().unwrap_or("file"),
                human_size(*size_bytes)
            ),
        ),
        MessageExtra::Embed { title, url } => (
            title.clone().unwrap_or_else(|| "Link preview".into()),
            url.clone().unwrap_or_else(|| "Embed".into()),
        ),
        MessageExtra::Poll => ("Poll".into(), "Vote in Discord".into()),
        MessageExtra::VoiceMessage => ("Voice message".into(), "Play in Discord".into()),
        MessageExtra::Sticker { name } => (format!("Sticker: {name}"), "Shown in Discord".into()),
        MessageExtra::Thread => ("Thread".into(), "Continue in Discord".into()),
        MessageExtra::Unsupported { kind } => {
            (format!("{kind} content"), "Not rendered by Litecord".into())
        }
    }
}

pub(crate) fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{:.1} MB", b / KB / KB)
    }
}

/// Deep link to one message, from the conversation's link.
fn message_url(chat: &ConversationViewModel, row: &MessageRow) -> String {
    format!(
        "{}/{}",
        chat.capabilities.open_in_discord_url.trim_end_matches('/'),
        row.message_id
    )
}

/// Offset of the local time zone from UTC at `t`, in milliseconds.
pub(crate) fn local_offset_ms(t: Timestamp) -> i64 {
    use chrono::{Offset, TimeZone};
    chrono::DateTime::from_timestamp_millis(t.as_millis())
        .map(|utc| {
            i64::from(
                chrono::Local
                    .offset_from_utc_datetime(&utc.naive_utc())
                    .fix()
                    .local_minus_utc(),
            ) * 1000
        })
        .unwrap_or(0)
}

/// "10:14 AM" in local time.
pub(crate) fn clock(t: Timestamp) -> String {
    clock_at(t, local_offset_ms(t))
}

fn clock_at(t: Timestamp, offset_ms: i64) -> String {
    let minutes = ((t.as_millis() + offset_ms) / 60_000).rem_euclid(1440);
    let (h, m) = (minutes / 60, minutes % 60);
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    format!("{h12}:{m:02} {}", if h < 12 { "AM" } else { "PM" })
}

fn short_clock(t: Timestamp) -> String {
    clock(t)
}

/// Local hour of day (0–23), for greetings.
pub(crate) fn local_hour(t: Timestamp) -> i64 {
    ((t.as_millis() + local_offset_ms(t)) / 3_600_000).rem_euclid(24)
}

/// Compact list time (A01): "10:24 AM" today, "Yesterday", weekday within
/// a week, else "Sep 4". Local time.
pub(crate) fn list_time(t: Timestamp, now: Timestamp) -> String {
    list_time_at(t, now, local_offset_ms(now))
}

fn list_time_at(t: Timestamp, now: Timestamp, offset_ms: i64) -> String {
    let day = |x: Timestamp| (x.as_millis() + offset_ms).div_euclid(86_400_000);
    let diff = day(now) - day(t);
    match diff {
        i64::MIN..=0 => clock_at(t, offset_ms),
        1 => "Yesterday".into(),
        2..=6 => day_label_at(t, offset_ms)
            .split(',')
            .next()
            .unwrap_or_default()
            .to_owned(),
        _ => {
            let label = day_label_at(t, offset_ms);
            let mut parts = label.split(' ').skip(1);
            let d = parts.next().unwrap_or_default().to_owned();
            format!("{} {d}", parts.next().unwrap_or_default())
        }
    }
}

/// Local calendar day, e.g. "Thu, 24 Sep 2026".
pub(crate) fn day_label(t: Timestamp) -> String {
    day_label_at(t, local_offset_ms(t))
}

fn day_label_at(t: Timestamp, offset_ms: i64) -> String {
    let days = (t.as_millis() + offset_ms).div_euclid(86_400_000);
    // Civil-from-days (Howard Hinnant), valid for the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let wd = WEEKDAYS[days.rem_euclid(7) as usize];
    format!("{wd}, {d} {} {y}", MONTHS[(m - 1) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;
    use litecord_types::social::PresenceStatus;

    #[test]
    fn day_labels_are_utc_calendar_days() {
        assert_eq!(
            day_label_at(Timestamp::from_millis(0), 0),
            "Thu, 1 Jan 1970"
        );
        // 2026-09-24T15:00:00Z
        assert_eq!(
            day_label_at(Timestamp::from_millis(1_790_262_000_000), 0),
            "Thu, 24 Sep 2026"
        );
        assert_eq!(
            day_label_at(Timestamp::from_millis(951_782_400_000), 0),
            "Tue, 29 Feb 2000"
        );
    }

    #[test]
    fn list_times_are_compact() {
        let now = Timestamp::from_millis(1_790_262_000_000); // Thu 24 Sep 15:00
        assert_eq!(
            list_time_at(
                Timestamp::from_millis(1_790_262_000_000 - 3_600_000),
                now,
                0
            ),
            "2:00 PM"
        );
        assert_eq!(
            list_time_at(
                Timestamp::from_millis(1_790_262_000_000 - 86_400_000),
                now,
                0
            ),
            "Yesterday"
        );
        assert_eq!(
            list_time_at(
                Timestamp::from_millis(1_790_262_000_000 - 3 * 86_400_000),
                now,
                0
            ),
            "Mon"
        );
        assert_eq!(
            list_time_at(
                Timestamp::from_millis(1_790_262_000_000 - 20 * 86_400_000),
                now,
                0
            ),
            "Sep 4"
        );
    }

    #[test]
    fn clock_is_twelve_hour_and_offset_aware() {
        let t = Timestamp::from_millis(1_790_262_000_000); // 15:00 UTC
        assert_eq!(clock_at(t, 0), "3:00 PM");
        assert_eq!(clock_at(t, -5 * 3_600_000), "10:00 AM");
        assert_eq!(clock_at(Timestamp::from_millis(0), 0), "12:00 AM");
        // The day rolls over with the offset too.
        assert_eq!(
            day_label_at(Timestamp::from_millis(1_790_262_000_000), 10 * 3_600_000),
            "Fri, 25 Sep 2026"
        );
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(5 * 1024 * 1024 + 512 * 1024), "5.5 MB");
    }

    #[test]
    fn presence_is_mapped_from_status() {
        assert_eq!(
            theme::Presence::from_status(PresenceStatus::Online.as_str()),
            theme::Presence::Online
        );
        assert_eq!(theme::Presence::from_status("dnd"), theme::Presence::Dnd);
    }
}
