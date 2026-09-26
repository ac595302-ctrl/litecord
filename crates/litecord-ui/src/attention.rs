use crate::{bridge::Command, theme, workspace::Workspace};
use eframe::egui::{self, Ui};
use litecord_app::view::{InboxItem, PendingActionRow};
use litecord_layout::Destination;
use litecord_types::{
    actions::{ActionStatus, CapabilityClass},
    provenance::{DiscordIdentity, Origin},
};

impl Workspace {
    pub fn home_screen(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.spinner();
            return;
        };
        let identity = self.display(&snapshot.account.display_name);
        egui::ScrollArea::vertical()
            .id_salt("home_screen")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!("Welcome back, {identity}"))
                        .size(20.0)
                        .color(theme::TEXT),
                );
                ui.add_space(8.0);
                self.home_omni_card(ui, &snapshot);
                ui.add_space(12.0);
                let replies: Vec<_> = snapshot
                    .inbox
                    .needs_attention
                    .iter()
                    .filter_map(|i| match i {
                        InboxItem::PendingReply {
                            conversation_id,
                            from,
                            preview,
                            ..
                        } => Some((*conversation_id, from.clone(), preview.clone())),
                        _ => None,
                    })
                    .collect();
                ui.horizontal_wrapped(|ui| {
                    let tiles = [
                        ("Waiting on you", replies.len(), Destination::Inbox),
                        (
                            "Friends online",
                            snapshot.friends.online.len(),
                            Destination::Friends,
                        ),
                        ("Open tasks", snapshot.tasks.open.len(), Destination::Tasks),
                        (
                            "To review",
                            snapshot.inbox.pending_actions.len()
                                + snapshot.tasks.candidates.len()
                                + snapshot
                                    .memory
                                    .counts_by_status
                                    .iter()
                                    .find(|(s, _)| {
                                        *s == litecord_types::memory::MemoryStatus::Candidate
                                    })
                                    .map_or(0, |(_, n)| *n as usize),
                            Destination::Memory,
                        ),
                    ];
                    for (label, count, dest) in tiles {
                        if metric_card(ui, label, count).clicked() {
                            self.navigate(dest);
                        }
                    }
                });
                ui.add_space(14.0);
                let half = ((ui.available_width() - 16.0) / 2.0).max(240.0);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(half);
                        theme::section_label(ui, "Waiting on you");
                        if replies.is_empty() {
                            ui.label(theme::meta("You're all caught up."));
                        }
                        for (conv, from, preview) in replies.iter().take(5) {
                            if attention_row(
                                ui,
                                "Reply",
                                &self.display(from),
                                &self.display(preview),
                                "Open",
                            ) {
                                self.navigate(Destination::Messages);
                                self.open_conversation(*conv);
                            }
                        }
                    });
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        ui.set_width(half);
                        theme::section_label(ui, "Recent conversations");
                        for c in snapshot.conversations.conversations.iter().take(5) {
                            let title = self.display(&c.title);
                            let r = ui
                                .horizontal(|ui| {
                                    theme::avatar_presence(
                                        ui,
                                        &title,
                                        30.0,
                                        c.recipient_status.map_or(theme::Presence::None, |p| {
                                            theme::Presence::from_status(p.as_str())
                                        }),
                                    );
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new(&title).color(theme::TEXT));
                                        ui.add(
                                            egui::Label::new(theme::meta(
                                                self.display(
                                                    c.last_message_preview
                                                        .as_deref()
                                                        .unwrap_or("No messages cached yet"),
                                                ),
                                            ))
                                            .truncate(),
                                        );
                                    });
                                })
                                .response
                                .interact(egui::Sense::click());
                            if r.clicked() {
                                self.navigate(Destination::Messages);
                                self.open_conversation(c.conversation_id);
                            }
                            ui.add_space(4.0);
                        }
                    });
                });
            });
    }

    fn home_omni_card(&mut self, ui: &mut Ui, snapshot: &crate::bridge::Snapshot) {
        use litecord_app::harness::LoginState;
        let status = &snapshot.omni.status;
        egui::Frame::new()
            .fill(theme::OMNI.gamma_multiply(0.07))
            .stroke(egui::Stroke::new(1.0, theme::OMNI.gamma_multiply(0.35)))
            .corner_radius(8)
            .inner_margin(12)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Omni").color(theme::OMNI).strong());
                    let ready =
                        matches!(status.login, LoginState::Ready { .. } | LoginState::Stopped)
                            && status.unavailable.is_none();
                    if ready {
                        let w = (ui.available_width() - 80.0).max(120.0);
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.omni_draft)
                                .desired_width(w)
                                .hint_text("Ask about your people, messages and tasks…"),
                        );
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let text = self.omni_draft.trim().to_owned();
                        if (ui
                            .add_enabled(!text.is_empty() && !self.busy, egui::Button::new("Ask"))
                            .clicked()
                            || enter && !text.is_empty())
                            && !self.busy
                        {
                            self.send(Command::Omni(crate::bridge::OmniCommand::Send(None, text)));
                            self.omni_draft.clear();
                            self.omni_open = true;
                        }
                    } else {
                        let msg = if status.selected.is_none() {
                            "Connect Codex or OpenCode to ask questions about your conversations."
                        } else {
                            "Sign in to your agent harness to start asking."
                        };
                        ui.label(egui::RichText::new(msg).color(theme::SECONDARY));
                        if ui.button("Set up Omni").clicked() {
                            self.omni_open = true;
                        }
                    }
                });
            });
    }
    pub fn inbox_screen(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.spinner();
            ui.label("Loading inbox…");
            return;
        };
        let privacy = self.private();
        let owner = self.display(&snapshot.account.display_name);

        egui::ScrollArea::vertical()
            .id_salt("inbox_screen")
            .show(ui, |ui| {
                theme::page_header(
                    ui,
                    "Inbox",
                    Some("Replies you owe, suggestions to review, and actions waiting for your approval."),
                );
                let _ = &owner;
                self.inbox_today(ui, &snapshot);
                let filter = self.inbox_filter;
                let show = |f: usize| filter == 0 || filter == f;
                let attention: Vec<&InboxItem> =
                    crate::context_ui::visible_attention(&snapshot.inbox.needs_attention)
                        .into_iter()
                    .filter(|i| match i {
                        InboxItem::PendingReply { .. } | InboxItem::ReminderDue { .. } => show(1),
                        InboxItem::TaskCandidate { .. } | InboxItem::Commitment { .. } => show(4),
                    })
                    .collect();
                let approvals = show(2);
                let checkins = show(3);
                let empty = attention.is_empty()
                    && (!approvals
                        || snapshot.inbox.pending_actions.is_empty() && snapshot.omni.requests.is_empty())
                    && (!checkins || snapshot.omni.checkins.is_empty());
                if empty {
                    theme::empty_state(ui, "All clear", "Nothing here needs your attention right now.");
                }
                if approvals && !snapshot.omni.requests.is_empty() {
                    theme::section_label(ui, "Omni wants to run on this computer");
                    ui.add_space(4.0);
                    for req in snapshot.omni.requests.clone() {
                        self.inbox_omni_request(ui, &req);
                    }
                    ui.add_space(8.0);
                }
                if checkins && !snapshot.omni.checkins.is_empty() {
                    ui.horizontal(|ui| {
                        theme::section_label(ui, "From Omni");
                        if ui.small_button("Dismiss all").clicked() {
                            self.send(Command::Omni(crate::bridge::OmniCommand::DismissCheckins));
                        }
                    });
                    for c in &snapshot.omni.checkins {
                        egui::Frame::new()
                            .fill(theme::OMNI.gamma_multiply(0.06))
                            .stroke(egui::Stroke::new(1.0, theme::OMNI.gamma_multiply(0.3)))
                            .corner_radius(8)
                            .inner_margin(10)
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(egui::RichText::new(&c.source).size(11.0).color(theme::OMNI));
                                ui.label(egui::RichText::new(self.display(&c.text)).color(theme::TEXT));
                                ui.horizontal(|ui| {
                                    if ui.small_button("Continue in Omni").clicked() {
                                        self.selection.omni_session = Some(c.session_id);
                                        self.omni_open = true;
                                        self.request();
                                    }
                                    if ui.small_button("Remember").clicked() {
                                        self.send(Command::Omni(crate::bridge::OmniCommand::Remember(
                                            c.session_id,
                                            c.seq,
                                        )));
                                    }
                                });
                            });
                        ui.add_space(6.0);
                    }
                    ui.add_space(8.0);
                }
                if !attention.is_empty() {
                    theme::section_label(ui, "Needs attention");
                    ui.add_space(8.0);
                    for item in attention {
                        match item {
                            InboxItem::PendingReply {
                                conversation_id,
                                from,
                                preview,
                                ..
                            } => {
                                let from = self.display(from);
                                let preview = self.display(preview);
                                if attention_row(
                                    ui,
                                    "Reply needed",
                                    &from,
                                    &preview,
                                    "Reply",
                                ) {
                                    self.open_conversation(*conversation_id);
                                }
                            }
                            InboxItem::ReminderDue { title, .. } => {
                                let title = self.display(title);
                                if attention_row(
                                    ui,
                                    "Reminder",
                                    &title,
                                    "A saved reminder needs attention.",
                                    "Open tasks",
                                ) {
                                    self.navigate(Destination::Tasks);
                                }
                            }
                            InboxItem::TaskCandidate { title, origin, .. } => {
                                let title = self.display(title);
                                let source = origin_label(*origin);
                                if attention_row(
                                    ui,
                                    source,
                                    &title,
                                    "",
                                    "Open tasks",
                                ) {
                                    self.navigate(Destination::Tasks);
                                }
                            }
                            InboxItem::Commitment { text, .. } => {
                                let text = self.display(text);
                                if attention_row(
                                    ui,
                                    "Commitment",
                                    &text,
                                    "Saved in Memory.",
                                    "Open memory",
                                ) {
                                    self.navigate(Destination::Memory);
                                }
                            }
                        }
                        ui.add_space(8.0);
                    }
                }

                if approvals && !snapshot.inbox.pending_actions.is_empty() {
                    ui.add_space(10.0);
                    theme::section_label(ui, "Waiting for your approval");
                    ui.add_space(8.0);
                    for action in &snapshot.inbox.pending_actions {
                        self.pending_action_card(ui, action, privacy);
                        ui.add_space(8.0);
                    }
                }
            });
    }

    /// A09: "Today" strip and teal Omni assistance cards.
    fn inbox_today(&mut self, ui: &mut Ui, snapshot: &crate::bridge::Snapshot) {
        let now = litecord_types::Timestamp::now().as_millis();
        let end_of_day = (now / 86_400_000 + 1) * 86_400_000;
        let replies = snapshot
            .inbox
            .needs_attention
            .iter()
            .filter(|i| matches!(i, InboxItem::PendingReply { .. }))
            .count();
        let due_today = snapshot
            .tasks
            .open
            .iter()
            .filter(|t| t.due_at.is_some_and(|d| d.as_millis() < end_of_day))
            .count();
        let reminders = snapshot
            .tasks
            .reminders
            .iter()
            .filter(|r| r.trigger.due_at().as_millis() < end_of_day)
            .count();
        egui::Frame::new()
            .fill(theme::RAISED)
            .stroke(egui::Stroke::new(1.0, theme::BORDER))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Today").strong().color(theme::TEXT));
                    ui.add_space(12.0);
                    for (n, label, color) in [
                        (replies, "waiting on a reply", theme::PRIMARY_TEXT),
                        (due_today, "tasks due", theme::PRIORITY),
                        (reminders, "reminders", theme::WARNING),
                    ] {
                        ui.label(egui::RichText::new(n.to_string()).strong().color(color));
                        ui.label(
                            egui::RichText::new(label)
                                .size(12.0)
                                .color(theme::SECONDARY),
                        );
                        ui.add_space(10.0);
                    }
                });
            });
        ui.add_space(8.0);
        let memory_review = snapshot
            .memory
            .counts_by_status
            .iter()
            .find(|(s, _)| *s == litecord_types::memory::MemoryStatus::Candidate)
            .map_or(0, |(_, n)| *n as usize);
        let first_reply = snapshot.inbox.needs_attention.iter().find_map(|i| match i {
            InboxItem::PendingReply {
                conversation_id,
                from,
                ..
            } => Some((*conversation_id, from.clone())),
            _ => None,
        });
        let approvals = snapshot.inbox.pending_actions.len() + snapshot.omni.requests.len();
        ui.label(
            egui::RichText::new("Omni inbox assistance")
                .size(14.0)
                .strong()
                .color(theme::TEXT),
        );
        ui.add_space(4.0);
        let mut action: Option<u8> = None;
        ui.horizontal_wrapped(|ui| {
            let cards: [(String, String, &str, u8); 4] = [
                (
                    format!("{approvals} waiting for approval"),
                    "Messages and actions proposed by Omni or you".into(),
                    "Review",
                    0,
                ),
                (
                    first_reply.as_ref().map_or_else(
                        || "No replies owed".into(),
                        |(_, f)| format!("Reply to {}", self.display(f)),
                    ),
                    "Oldest conversation waiting on you".into(),
                    "Open",
                    1,
                ),
                (
                    format!("{memory_review} memories to review"),
                    "New facts picked up from your messages".into(),
                    "Review",
                    2,
                ),
                (
                    "Catch-up summary".into(),
                    "Ask Omni what changed today".into(),
                    "Ask Omni",
                    3,
                ),
            ];
            for (title, body, button, id) in cards {
                egui::Frame::new()
                    .fill(theme::OMNI.gamma_multiply(0.07))
                    .stroke(egui::Stroke::new(1.0, theme::OMNI.gamma_multiply(0.35)))
                    .corner_radius(8)
                    .inner_margin(10)
                    .show(ui, |ui| {
                        ui.set_width(190.0);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&title).color(theme::TEXT));
                            ui.label(theme::meta(body));
                            let enabled = match id {
                                0 => approvals > 0,
                                1 => first_reply.is_some(),
                                2 => memory_review > 0,
                                _ => true,
                            };
                            if ui
                                .add_enabled(
                                    enabled,
                                    egui::Button::new(
                                        egui::RichText::new(button).color(theme::OMNI),
                                    )
                                    .small(),
                                )
                                .clicked()
                            {
                                action = Some(id);
                            }
                        });
                    });
            }
        });
        match action {
            Some(0) => self.inbox_filter = 2,
            Some(1) => {
                if let Some((conv, _)) = first_reply {
                    self.navigate(Destination::Messages);
                    self.open_conversation(conv);
                }
            }
            Some(2) => {
                self.memory_status_filter = Some(litecord_types::memory::MemoryStatus::Candidate);
                self.navigate(Destination::Memory);
            }
            Some(_) => {
                self.omni_open = true;
                self.omni_draft = "What changed today, and what needs my attention first?".into();
            }
            None => {}
        }
        ui.add_space(8.0);
    }

    fn inbox_omni_request(&mut self, ui: &mut Ui, req: &litecord_app::omni::OmniRequestRow) {
        use litecord_app::harness::{Decision, RequestKind};
        let detail = match &req.request {
            RequestKind::Command { command, .. } => format!("Run: {command}"),
            RequestKind::FileChange { summary } => format!("Change files: {summary}"),
            RequestKind::Permission { title } => title.clone(),
        };
        egui::Frame::new()
            .fill(theme::SIDEBAR)
            .stroke(egui::Stroke::new(1.0, theme::WARNING.gamma_multiply(0.5)))
            .corner_radius(8)
            .inner_margin(12)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    theme::chip(ui, "Local · this computer", theme::WARNING);
                    ui.label(egui::RichText::new(detail).monospace().color(theme::TEXT));
                });
                if let Some(reason) = &req.reason {
                    ui.label(theme::meta(reason.as_str()));
                }
                ui.horizontal(|ui| {
                    let answer =
                        |d| Command::Omni(crate::bridge::OmniCommand::Answer(req.id.clone(), d));
                    if ui.button("Allow once").clicked() {
                        self.send(answer(Decision::Accept));
                    }
                    if ui.button("Decline").clicked() {
                        self.send(answer(Decision::Decline));
                    }
                    if ui.small_button("View chat").clicked() {
                        self.selection.omni_session = Some(req.session_id);
                        self.omni_open = true;
                        self.request();
                    }
                });
            });
        ui.add_space(6.0);
    }

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
        let class = class_label(action.class);
        let is_pending = action.status == ActionStatus::PendingApproval;
        // The current review snapshot carries a complete target and body only for
        // message sends. Incomplete action payloads remain rejectable, never approvable.
        let payload_reviewable = action.kind == "send_message"
            && target.as_deref().is_some_and(|value| !value.is_empty())
            && content.as_deref().is_some_and(|value| !value.is_empty());
        let can_approve = !self.busy && is_pending && !privacy && payload_reviewable;
        let can_reject = !self.busy && is_pending;
        let mut decision = None;
        let action_id = action.action_id;

        egui::Frame::new()
            .fill(theme::SIDEBAR)
            .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(&summary)
                                .strong()
                                .color(theme::TEXT),
                        );
                        ui.horizontal(|ui| {
                            match action.identity {
                                DiscordIdentity::ApplicationBot => {
                                    theme::chip(ui, "Acts as your bot", theme::WARNING)
                                }
                                DiscordIdentity::UserSocialSdk => {
                                    theme::chip(ui, "Acts as you", theme::PRIMARY)
                                }
                            }
                            theme::chip(ui, &class, action_class_color(action.class));
                            theme::chip(ui, &status, theme::MUTED);
                        });
                    });
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if ui
                                .add_enabled(can_reject, egui::Button::new("Reject"))
                                .clicked()
                            {
                                decision = Some(false);
                            }
                            if ui
                                .add_enabled(can_approve, egui::Button::new("Approve"))
                                .clicked()
                            {
                                decision = Some(true);
                            }
                        },
                    );
                });
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!("Requested by {actor} · {kind}"))
                        .size(12.0)
                        .color(theme::SECONDARY),
                );
                if let Some(target) = &target {
                    ui.label(
                        egui::RichText::new(format!("Target: {target}"))
                            .size(12.0)
                            .color(theme::SECONDARY),
                    );
                }
                if let Some(rationale) = &rationale {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("Reason: {rationale}"))
                            .size(12.0)
                            .color(theme::MUTED),
                    );
                }
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("Full action content")
                        .size(12.0)
                        .strong()
                        .color(theme::SECONDARY),
                );
                if let Some(content) = &content {
                    ui.label(
                        egui::RichText::new(content)
                            .color(theme::TEXT),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("The exact action content is not available in this review snapshot.")
                            .color(theme::PRIORITY),
                    );
                }
                if privacy {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "Privacy Mode hides the exact payload, so approval is disabled.",
                        )
                        .size(12.0)
                        .color(theme::PRIORITY),
                    );
                } else if !payload_reviewable && is_pending {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "This snapshot omits part of the exact target or payload, so approval is disabled.",
                        )
                        .size(12.0)
                        .color(theme::MUTED),
                    );
                }
            });

        if let Some(approve) = decision {
            self.send(if approve {
                Command::Approve(action_id)
            } else {
                Command::Reject(action_id)
            });
        }
    }
}

fn metric_card(ui: &mut Ui, label: &str, count: usize) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(170.0, 60.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect(
            rect,
            8.0,
            if response.hovered() {
                theme::HOVER
            } else {
                theme::RAISED
            },
            egui::Stroke::new(1.0, theme::BORDER),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.left_top() + egui::vec2(12.0, 8.0),
            egui::Align2::LEFT_TOP,
            count.to_string(),
            egui::FontId::proportional(22.0),
            theme::TEXT,
        );
        painter.text(
            rect.left_bottom() + egui::vec2(12.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            label,
            egui::FontId::proportional(12.0),
            theme::SECONDARY,
        );
    }
    let text = format!("{label}: {count}");
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &text));
    response
}

/// One compact attention row: category chip, title, one-line body, action.
fn attention_row(ui: &mut Ui, category: &str, title: &str, body: &str, action: &str) -> bool {
    let mut clicked = false;
    egui::Frame::new()
        .fill(theme::WORKSPACE)
        .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add_sized(
                    [96.0, 18.0],
                    egui::Label::new(egui::RichText::new(category).size(11.0).color(theme::MUTED))
                        .truncate(),
                );
                let w = (ui.available_width() - 150.0).max(80.0);
                ui.vertical(|ui| {
                    ui.set_width(w);
                    ui.add(
                        egui::Label::new(egui::RichText::new(title).color(theme::TEXT)).truncate(),
                    );
                    if !body.is_empty() {
                        ui.add(egui::Label::new(theme::meta(body)).truncate());
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    clicked = ui.button(action).clicked();
                });
            });
        });
    ui.add_space(2.0);
    clicked
}

fn origin_label(origin: Origin) -> &'static str {
    match origin {
        Origin::DiscordSocialSdk | Origin::DiscordBotGateway => "Observed from Discord",
        Origin::UserProvided => "Added by you",
        Origin::LocalApplication => "Suggested task",
        Origin::AgentDerived => "From Omni",
        Origin::Imported => "Imported suggestion",
        Origin::Synthetic => "Synthetic demo item",
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
