//! Messages center panel: header, virtualized message list and composer.
//!
//! Row heights come from one measuring function used for both layout and
//! painting, so rows never overlap however long names, text or attachment
//! lists are.

use std::sync::Arc;

use eframe::egui::{self, Align2, FontId, Galley, Rect, RichText, Sense, Stroke, Ui};
use litecord_app::view::{ConversationViewModel, MessageRow};
use litecord_types::provenance::DiscordIdentity;
use litecord_types::social::MessageExtra;
use litecord_types::Timestamp;

use crate::bridge::Command;
use crate::theme;
use crate::workspace::Workspace;

const AVATAR: f32 = 34.0;
const GUTTER: f32 = 50.0;
const HEADER_LINE: f32 = 22.0;
const ATTACHMENT: f32 = 40.0;
const DAY_SEPARATOR: f32 = 30.0;
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
        let Some(chat) = &s.chat else {
            theme::page_header(ui, "Messages", None);
            theme::empty_state(
                ui,
                "No conversation selected",
                "Choose a conversation on the left to read and reply.",
            );
            return;
        };
        if s.selection.conversation != self.selection.conversation
            || s.selection.before != self.selection.before
        {
            ui.spinner();
            ui.label(theme::meta("Loading conversation…"));
            return;
        }
        let private = self.private();
        let presence = s
            .contact
            .as_ref()
            .map(|c| theme::Presence::from_status(c.presence.status.as_str()))
            .unwrap_or(theme::Presence::None);
        ui.horizontal(|ui| {
            theme::avatar_presence(ui, &self.display(&chat.title), 36.0, presence);
            ui.vertical(|ui| {
                ui.label(RichText::new(self.display(&chat.title)).size(16.0).strong());
                let status = s.contact.as_ref().map_or_else(
                    || "Conversation".to_owned(),
                    |c| match (&c.presence.activity, private) {
                        (Some(a), false) => format!("{} · {}", presence.label(), a.name),
                        _ => presence.label().to_owned(),
                    },
                );
                ui.label(theme::meta(status));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Open in Discord")
                    .on_hover_text("Open this conversation in the Discord app or website")
                    .clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(
                        chat.capabilities.open_in_discord_url.clone(),
                    ));
                }
                if chat.capabilities.send_identity == Some(DiscordIdentity::ApplicationBot) {
                    theme::chip(ui, "Posting as your bot", theme::WARNING);
                }
            });
        });
        ui.separator();
        ui.horizontal(|ui| {
            if chat.has_more && ui.small_button("Load older messages").clicked() {
                self.selection.before = chat.messages.first().map(|m| m.sent_at);
                self.request();
            }
            if self.selection.before.is_some() && ui.small_button("Jump to latest").clicked() {
                self.selection.before = None;
                self.request();
            }
        });
        let composer = 96.0;
        let height = (ui.available_height() - composer).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt(("messages", chat.conversation_id, self.selection.before))
            .max_height(height)
            .auto_shrink([false, false])
            .stick_to_bottom(self.selection.before.is_none())
            .show_viewport(ui, |ui, viewport| {
                let width = ui.available_width();
                let top = ui.cursor().min;
                let layouts = measure(ui, chat, width, private);
                let mut y = 0.0;
                for (row, layout) in chat.messages.iter().zip(layouts) {
                    let h = layout.height;
                    if y + h >= viewport.min.y && y <= viewport.max.y {
                        let rect =
                            Rect::from_min_size(top + egui::vec2(0.0, y), egui::vec2(width, h));
                        self.paint_row(ui, rect, chat, row, layout, private);
                    }
                    y += h;
                }
                ui.allocate_space(egui::vec2(width, y.max(1.0)));
                if chat.messages.is_empty() {
                    theme::empty_state(
                        ui,
                        "No messages cached yet",
                        "Recent history loads in the background when Discord provides it.",
                    );
                }
            });
        self.composer(ui, chat);
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
            painter.line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                Stroke::new(1.0, theme::BORDER),
            );
            let text =
                painter.layout_no_wrap(day.clone(), FontId::proportional(12.0), theme::MUTED);
            let pill = Rect::from_center_size(
                egui::pos2(rect.center().x, y),
                text.size() + egui::vec2(16.0, 6.0),
            );
            painter.rect_filled(pill, 8.0, theme::WORKSPACE);
            painter.galley(pill.min + egui::vec2(8.0, 3.0), text, theme::MUTED);
            body.min.y += DAY_SEPARATOR;
        }
        let response = ui.interact(body, ui.id().with(row.message_id), Sense::click());
        if response.hovered() || row.render.highlighted {
            let fill = if row.render.highlighted {
                theme::WARNING.gamma_multiply(0.08)
            } else {
                theme::HOVER.gamma_multiply(0.6)
            };
            ui.painter().rect_filled(body, 4.0, fill);
        }
        let name = if private {
            "Hidden user".to_owned()
        } else {
            row.render.author_display.clone()
        };
        let text_x = body.left() + GUTTER;
        let mut y = body.top() + 4.0;
        if !layout.continuation {
            let mut avatar = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                egui::pos2(body.left() + 6.0, body.top() + 6.0),
                egui::vec2(AVATAR, AVATAR),
            )));
            theme::avatar(&mut avatar, &name, AVATAR, false);
            // Name is elided so it can never run into the timestamp.
            let time = clock(row.sent_at) + if row.edited { " · edited" } else { "" };
            let time_galley =
                ui.painter()
                    .layout_no_wrap(time, FontId::proportional(11.0), theme::MUTED);
            let max_name = (body.right() - text_x - time_galley.size().x - 16.0).max(24.0);
            let mut job = egui::text::LayoutJob::simple_singleline(
                name,
                FontId::proportional(14.0),
                if row.is_mine {
                    theme::PRIMARY_TEXT
                } else {
                    theme::TEXT
                },
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(max_name);
            let name_galley = ui.painter().layout_job(job);
            let name_width = name_galley.size().x;
            ui.painter()
                .galley(egui::pos2(text_x, y), name_galley, theme::TEXT);
            ui.painter().galley(
                egui::pos2(text_x + name_width + 8.0, y + 2.0),
                time_galley,
                theme::MUTED,
            );
            y += HEADER_LINE;
        } else if response.hovered() {
            ui.painter().text(
                egui::pos2(body.left() + 8.0, y + 1.0),
                Align2::LEFT_TOP,
                short_clock(row.sent_at),
                FontId::proportional(10.0),
                theme::MUTED,
            );
        }
        let text_height = layout.galley.size().y;
        ui.painter()
            .galley(egui::pos2(text_x, y), layout.galley, theme::SECONDARY);
        y += text_height + 4.0;
        if !private {
            for extra in &row.extras {
                let card = Rect::from_min_size(
                    egui::pos2(text_x, y),
                    egui::vec2((body.right() - text_x - 8.0).min(420.0), ATTACHMENT - 6.0),
                );
                self.extra_card(ui, card, extra, &message_url(chat, row));
                y += ATTACHMENT;
            }
        }
        if row.bookmarked {
            ui.painter().text(
                egui::pos2(body.right() - 8.0, body.top() + 4.0),
                Align2::RIGHT_TOP,
                "Bookmarked",
                FontId::proportional(10.0),
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
        painter.rect(
            rect,
            6.0,
            if response.hovered() {
                theme::HOVER
            } else {
                theme::RAISED
            },
            Stroke::new(1.0, theme::BORDER),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.left_center() + egui::vec2(10.0, -7.0),
            Align2::LEFT_CENTER,
            title,
            FontId::proportional(13.0),
            theme::TEXT,
        );
        painter.text(
            rect.left_center() + egui::vec2(10.0, 8.0),
            Align2::LEFT_CENTER,
            detail,
            FontId::proportional(11.0),
            theme::MUTED,
        );
        painter.text(
            rect.right_center() - egui::vec2(10.0, 0.0),
            Align2::RIGHT_CENTER,
            "Open in Discord",
            FontId::proportional(11.0),
            theme::PRIMARY_TEXT,
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
        let draft = self.drafts.entry(id).or_default();
        let hint = match (private, can_send_here) {
            (true, _) => "Message (privacy mode)".to_owned(),
            (false, false) => "Sending is not available here".to_owned(),
            (false, true) => format!("Message {title}"),
        };
        let response = ui.add_enabled(
            !self.busy && can_send_here,
            egui::TextEdit::multiline(draft)
                .password(private)
                .id_salt(("composer", id))
                .hint_text(hint)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        // Enter sends, Shift+Enter inserts a newline (Ctrl/Cmd+Enter also sends).
        let submit = response.has_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        let text = draft.trim_end_matches('\n').to_owned();
        ui.horizontal(|ui| {
            let who = match identity {
                Some(DiscordIdentity::ApplicationBot) => "Sending as your bot",
                Some(DiscordIdentity::UserSocialSdk) => "Sending as you",
                None => "Read only here · use Open in Discord to reply",
            };
            ui.label(theme::meta(who));
            if can_send_here {
                ui.label(theme::meta("· Enter to send, Shift+Enter for a new line"));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let count = text.chars().count();
                let can_send =
                    !self.busy && can_send_here && !text.trim().is_empty() && count <= 2000;
                let send = ui.add_enabled(
                    can_send,
                    egui::Button::new(RichText::new("Send").color(theme::TEXT))
                        .fill(theme::PRIMARY),
                );
                if count > 1800 {
                    ui.label(RichText::new(format!("{count}/2000")).size(12.0).color(
                        if count > 2000 {
                            theme::PRIORITY
                        } else {
                            theme::MUTED
                        },
                    ));
                }
                if can_send && (send.clicked() || submit) {
                    if let Some(identity) = identity {
                        self.send(Command::SendAs(id, text.clone(), identity));
                    }
                }
            });
        });
        if submit {
            if let Some(d) = self.drafts.get_mut(&id) {
                *d = d.trim_end_matches('\n').to_owned();
            }
        }
    }
}

fn measure(ui: &Ui, chat: &ConversationViewModel, width: f32, private: bool) -> Vec<Layout> {
    let mut out = Vec::with_capacity(chat.messages.len());
    let mut prev: Option<&MessageRow> = None;
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
        let galley = ui.painter().layout(
            text,
            FontId::proportional(14.0),
            theme::SECONDARY,
            (width - GUTTER - 12.0).max(40.0),
        );
        let extras = if private { 0 } else { row.extras.len() };
        let mut height = galley.size().y + 10.0 + extras as f32 * ATTACHMENT;
        if !continuation {
            height += HEADER_LINE + 6.0;
            height = height.max(AVATAR + 14.0);
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

fn clock(t: Timestamp) -> String {
    let minutes = (t.as_millis() / 60_000).rem_euclid(1440);
    format!("{:02}:{:02} UTC", minutes / 60, minutes % 60)
}

fn short_clock(t: Timestamp) -> String {
    let minutes = (t.as_millis() / 60_000).rem_euclid(1440);
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// UTC calendar day, e.g. "Thu, 24 Sep 2026".
pub(crate) fn day_label(t: Timestamp) -> String {
    let days = t.as_millis().div_euclid(86_400_000);
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
        assert_eq!(day_label(Timestamp::from_millis(0)), "Thu, 1 Jan 1970");
        // 2026-09-24T15:00:00Z
        assert_eq!(
            day_label(Timestamp::from_millis(1_790_262_000_000)),
            "Thu, 24 Sep 2026"
        );
        assert_eq!(
            day_label(Timestamp::from_millis(951_782_400_000)),
            "Tue, 29 Feb 2000"
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
