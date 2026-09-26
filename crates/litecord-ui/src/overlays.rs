use crate::{bridge::Command, theme, workspace::Workspace};
use eframe::egui::{self, Ui};

impl Workspace {
    pub fn palette_window(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::K)) {
            self.palette_open = !self.palette_open;
        }
        if ctx.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::P,
            )
        }) {
            self.send(Command::Setting(
                "privacy.enabled".into(),
                serde_json::json!(!self.private()),
            ));
        }
        if !self.palette_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Search & commands")
            .open(&mut open)
            .collapsible(false)
            .default_width(520.0)
            .show(ctx, |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.palette_query)
                        .hint_text("Search commands…")
                        .desired_width(f32::INFINITY),
                );
                if response.changed() {
                    self.selection.palette_query = self.palette_query.clone();
                    self.request();
                }
                if let Some(s) = self.snapshot.clone() {
                    egui::ScrollArea::vertical()
                        .max_height(440.0)
                        .show(ui, |ui| {
                            for m in &s.palette.matches {
                                let response = ui.add_enabled(
                                    m.available && !self.busy,
                                    egui::Button::new(&m.name),
                                );
                                if let Some(reason) = &m.unavailable_reason {
                                    response.clone().on_disabled_hover_text(reason);
                                }
                                if response.clicked() {
                                    self.send(Command::Run(
                                        m.id.as_str().into(),
                                        self.selection.conversation,
                                    ));
                                    self.palette_open = false;
                                }
                                ui.label(
                                    egui::RichText::new(&m.description)
                                        .size(12.0)
                                        .color(theme::MUTED),
                                );
                            }
                        });
                }
            });
        self.palette_open &= open;
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.palette_open = false;
        }
    }
    pub fn server_flyout(&mut self, ui: &mut Ui, response: &egui::Response) {
        let popup = egui::Popup::from_response(response)
            .at_position(response.rect.right_top() + egui::vec2(4.0, 0.0))
            .width(260.0);
        let id = popup.get_id();
        if response.hovered() {
            egui::Popup::open_id(ui.ctx(), id);
        }
        if let Some(rect) = popup.get_popup_rect() {
            if ui.ctx().pointer_hover_pos().is_some_and(|p| {
                !rect.expand(8.0).contains(p) && !response.rect.expand(8.0).contains(p)
            }) {
                egui::Popup::close_id(ui.ctx(), id);
            }
        }
        popup.open_memory(None).show(|ui| {
            theme::section_label(ui, "Your servers");
            if let Some(s) = self.snapshot.clone() {
                if s.guilds.guilds.is_empty() {
                    ui.label("No cached servers yet.");
                }
                for g in &s.guilds.guilds {
                    if ui
                        .selectable_label(
                            self.selected_guild == Some(g.guild.id),
                            self.display(&g.guild.name),
                        )
                        .clicked()
                    {
                        self.selected_guild = Some(g.guild.id);
                        self.selected_channel = None;
                        self.navigate(litecord_layout::Destination::Servers);
                        ui.close();
                    }
                }
            }
        });
    }
}
