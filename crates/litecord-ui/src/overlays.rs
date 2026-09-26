use crate::{bridge::Command, theme, workspace::Workspace};
use eframe::egui::{self, Ui};

impl Workspace {
    pub fn command_shortcuts(&mut self, ctx: &egui::Context) {
        if self.busy || self.close_when_idle {
            return;
        }
        let Some(snapshot) = self.snapshot.clone() else {
            return;
        };
        for command in &snapshot.shortcuts {
            let Some(shortcut) = command.shortcut.as_deref().and_then(keyboard_shortcut) else {
                continue;
            };
            if ctx.input_mut(|input| input.consume_shortcut(&shortcut)) {
                if command.available {
                    self.send(Command::Run(
                        command.id.as_str().into(),
                        self.selection.conversation,
                    ));
                } else {
                    self.notice = command.unavailable_reason.clone();
                }
                break;
            }
        }
    }
    pub fn palette_window(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::K)) {
            self.palette_open = !self.palette_open;
            self.palette_focus_requested = self.palette_open;
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
                if self.palette_focus_requested {
                    response.request_focus();
                    self.palette_focus_requested = false;
                }
                if response.changed() {
                    self.selection.palette_query = self.palette_query.clone();
                    self.request();
                }
                if let Some(s) = self.snapshot.clone() {
                    let current_results = s.palette.query == self.palette_query;
                    if current_results
                        && !self.busy
                        && response.has_focus()
                        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                    {
                        if let Some(command) =
                            s.palette.matches.iter().find(|command| command.available)
                        {
                            self.send(Command::Run(
                                command.id.as_str().into(),
                                self.selection.conversation,
                            ));
                            self.palette_open = false;
                        }
                    }
                    egui::ScrollArea::vertical()
                        .max_height(440.0)
                        .show(ui, |ui| {
                            for m in &s.palette.matches {
                                let response = ui.add_enabled(
                                    current_results && m.available && !self.busy,
                                    egui::Button::new(if let Some(shortcut) = &m.shortcut {
                                        format!("{}    {shortcut}", m.name)
                                    } else {
                                        m.name.clone()
                                    }),
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

fn keyboard_shortcut(text: &str) -> Option<egui::KeyboardShortcut> {
    let binding = text.parse::<litecord_features::command::Keybind>().ok()?;
    let key = egui::Key::from_name(&binding.key)?;
    let mut modifiers = egui::Modifiers::NONE;
    if binding.ctrl_or_cmd {
        modifiers |= egui::Modifiers::COMMAND;
    }
    if binding.shift {
        modifiers |= egui::Modifiers::SHIFT;
    }
    if binding.alt {
        modifiers |= egui::Modifiers::ALT;
    }
    Some(egui::KeyboardShortcut::new(modifiers, key))
}
