use crate::{bridge::Command, theme, workspace::Workspace};
use eframe::egui::{self, Ui};
use litecord_app::view::{InboxItem, PendingActionRow};
use litecord_layout::Destination;
use litecord_types::{
    actions::{ActionStatus, CapabilityClass},
    provenance::Origin,
    social::PresenceStatus,
};

impl Workspace {
    pub fn home_screen(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.spinner();
            ui.label("Loading workspace data…");
            return;
        };
        let identity = self.display(&snapshot.account.display_name);
        let conversation_count = snapshot.conversations.conversations.len();
        let online_count = snapshot.friends.online.len();
        let open_task_count = snapshot.tasks.open.len();
        let review_count = snapshot.inbox.pending_actions.len();

        egui::ScrollArea::vertical()
            .id_salt("home_screen")
            .show(ui, |ui| {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(format!("Welcome back, {identity}"))
                                .size(22.0)
                                .strong()
                                .color(theme::TEXT),
                        );
                        ui.label(
                            egui::RichText::new("Your workspace at a glance.")
                                .size(13.0)
                                .color(theme::SECONDARY),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Open inbox").clicked() {
                            self.navigate(Destination::Inbox);
                        }
                    });
                });

                ui.add_space(16.0);
                omni_availability(ui);
                ui.add_space(16.0);

                ui.horizontal_wrapped(|ui| {
                    metric_card(ui, "Conversations", conversation_count);
                    metric_card(ui, "Friends online", online_count);
                    metric_card(ui, "Open tasks", open_task_count);
                    metric_card(ui, "Awaiting review", review_count);
                });

                ui.add_space(22.0);
                theme::section_label(ui, "Recent conversations");
                ui.add_space(8.0);
                if snapshot.conversations.conversations.is_empty() {
                    ui.label(
                        egui::RichText::new("Recent conversations will appear here.")
                            .color(theme::MUTED),
                    );
                } else {
                    for conversation in snapshot.conversations.conversations.iter().take(3) {
                        let title = self.display(&conversation.title);
                        let preview = self.display(
                            conversation
                                .last_message_preview
                                .as_deref()
                                .unwrap_or("No recent message."),
                        );
                        ui.horizontal(|ui| {
                            theme::avatar(
                                ui,
                                &title,
                                36.0,
                                conversation.recipient_status == Some(PresenceStatus::Online),
                            );
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(&title).strong().color(theme::TEXT));
                                ui.label(
                                    egui::RichText::new(&preview)
                                        .size(12.0)
                                        .color(theme::SECONDARY),
                                );
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if conversation.awaiting_reply {
                                        theme::chip(ui, "Reply needed", theme::PRIMARY);
                                    }
                                    if ui.button("Open").clicked() {
                                        self.open_conversation(conversation.conversation_id);
                                    }
                                },
                            );
                        });
                        ui.add_space(6.0);
                        ui.separator();
                    }
                }
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
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new("Inbox")
                        .size(22.0)
                        .strong()
                        .color(theme::TEXT),
                );
                ui.label(
                    egui::RichText::new(
                        "Items that need your attention and actions awaiting review.",
                    )
                    .size(13.0)
                    .color(theme::SECONDARY),
                );
                ui.add_space(18.0);

                ui.horizontal(|ui| {
                    theme::section_label(ui, "Today");
                    ui.label(
                        egui::RichText::new(format!("Owner: {owner}"))
                            .size(12.0)
                            .color(theme::MUTED),
                    );
                });
                ui.add_space(8.0);

                if snapshot.inbox.needs_attention.is_empty()
                    && snapshot.inbox.pending_actions.is_empty()
                {
                    ui.label(
                        egui::RichText::new("Nothing needs your attention right now.")
                            .color(theme::MUTED),
                    );
                }

                if !snapshot.inbox.needs_attention.is_empty() {
                    theme::section_label(ui, "Needs attention");
                    ui.add_space(8.0);
                    for item in &snapshot.inbox.needs_attention {
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
                                    &format!("From {from}"),
                                    &preview,
                                    "Open conversation",
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
                                    "Review this task suggestion in Tasks.",
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

                if !snapshot.inbox.pending_actions.is_empty() {
                    ui.add_space(10.0);
                    theme::section_label(ui, "Pending actions");
                    ui.add_space(8.0);
                    for action in &snapshot.inbox.pending_actions {
                        self.pending_action_card(ui, action, privacy);
                        ui.add_space(8.0);
                    }
                }
            });
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

fn omni_availability(ui: &mut Ui) {
    egui::Frame::new()
        .fill(theme::OMNI.gamma_multiply(0.09))
        .stroke(egui::Stroke::new(
            1.0_f32,
            theme::OMNI.gamma_multiply(0.28),
        ))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                theme::chip(ui, "OMNI", theme::OMNI);
                ui.label(
                    egui::RichText::new(
                        "Generative replies are unavailable in this build. Inbox suggestions retain their source and proposals require your review.",
                    )
                    .size(12.0)
                    .color(theme::SECONDARY),
                );
            });
        });
}

fn metric_card(ui: &mut Ui, label: &str, count: usize) {
    egui::Frame::new()
        .fill(theme::SIDEBAR)
        .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(142.0);
            ui.set_min_height(68.0);
            ui.label(
                egui::RichText::new(count.to_string())
                    .size(22.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.label(egui::RichText::new(label).size(12.0).color(theme::MUTED));
        });
}

fn attention_row(ui: &mut Ui, category: &str, title: &str, body: &str, action: &str) -> bool {
    let mut clicked = false;
    egui::Frame::new()
        .fill(theme::SIDEBAR)
        .stroke(egui::Stroke::new(1.0_f32, theme::BORDER))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(category).size(12.0).color(theme::MUTED));
                    });
                    ui.label(egui::RichText::new(title).strong().color(theme::TEXT));
                    ui.label(egui::RichText::new(body).size(12.0).color(theme::SECONDARY));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    clicked = ui.button(action).clicked();
                });
            });
        });
    clicked
}

fn origin_label(origin: Origin) -> &'static str {
    match origin {
        Origin::DiscordSocialSdk | Origin::DiscordBotGateway => "Observed from Discord",
        Origin::UserProvided => "Added by you",
        Origin::LocalApplication => "Local heuristic",
        Origin::AgentDerived => "Agent suggestion",
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
