//! Omni panel: chat with the user's own harness (Codex/OpenCode), its
//! approval requests, and setup when it is not ready. Streaming text comes
//! from `Bridge::omni_stream`; everything else from `Snapshot::omni`.

use eframe::egui::{self, RichText, Stroke, Ui};
use litecord_app::automations::{AutomationDraft, AutomationOutputKind, AutomationTrigger};
use litecord_app::harness::{Decision, HarnessKind, LoginKind, LoginState, OmniMode, RequestKind};
use litecord_app::omni::{OmniItem, OmniRequestRow, OmniSessionRow};
use litecord_app::OmniViewModel;

use crate::bridge::{Command, OmniCommand};
use crate::theme;
use crate::workspace::Workspace;

impl Workspace {
    pub fn omni_panel(&mut self, ui: &mut Ui) {
        let Some(snapshot) = self.snapshot.clone() else {
            ui.label(theme::meta("Loading…"));
            return;
        };
        let omni = &snapshot.omni;
        self.omni_header(ui, omni);
        ui.separator();
        if !self.omni_ready(ui, omni) {
            return;
        }
        if let Some(selected) = omni.status.selected {
            if omni.status.models.is_empty()
                && self.omni_models_requested_for != Some(selected)
                && !self.busy
            {
                self.omni_models_requested_for = Some(selected);
                self.send(Command::Omni(OmniCommand::LoadModels));
            }
        }
        self.model_row(ui, omni);
        if let Some(error) = &omni.status.last_error {
            ui.label(RichText::new(error).size(12.0).color(theme::WARNING));
        }
        ui.add_space(6.0);
        for req in &omni.requests {
            self.omni_request(ui, req);
        }
        let running = omni.active.as_ref().is_some_and(|a| a.running);
        let composer_height = 92.0;
        let transcript_height = (ui.available_height() - composer_height).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt("omni_transcript")
            .max_height(transcript_height)
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if omni.items.is_empty() {
                    omni_intro(ui);
                }
                for item in &omni.items {
                    self.omni_item(ui, omni.active.as_ref(), item);
                }
                let stream = self.bridge.omni_stream.borrow().clone();
                if let (Some((sid, text)), Some(active)) = (stream, omni.active.as_ref()) {
                    if sid == active.id && running {
                        omni_bubble(ui, &text, true);
                    }
                }
                if running {
                    ui.label(
                        RichText::new("Omni is working…")
                            .size(12.0)
                            .color(theme::OMNI),
                    );
                }
            });
        self.omni_composer(ui, omni, running);
    }

    fn omni_header(&mut self, ui: &mut Ui, omni: &OmniViewModel) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Omni").size(18.0).color(theme::OMNI).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::kit::icon_button_ex(
                    ui,
                    crate::ph::X,
                    "Close Omni (Ctrl+J)",
                    30.0,
                    theme::SECONDARY,
                    true,
                )
                .clicked()
                {
                    self.omni_open = false;
                }
            });
        });
        ui.horizontal(|ui| {
            let harness = omni
                .status
                .selected
                .map_or("No harness", HarnessKind::label);
            let account = match &omni.status.login {
                LoginState::Ready { account: Some(a) } => format!("{harness} · {a}"),
                LoginState::Ready { account: None } => harness.to_owned(),
                LoginState::SigningIn { .. } => format!("{harness} · signing in"),
                LoginState::SignedOut => format!("{harness} · signed out"),
                LoginState::Stopped => format!("{harness} · idle"),
                LoginState::NotInstalled => "No harness installed".to_owned(),
                LoginState::Error { .. } => format!("{harness} · error"),
            };
            ui.add(egui::Label::new(theme::meta(account)).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("History", |ui| {
                    ui.set_min_width(260.0);
                    let chats: Vec<&OmniSessionRow> =
                        omni.sessions.iter().filter(|s| s.kind == "chat").collect();
                    if chats.is_empty() {
                        ui.label(theme::meta("No chats yet"));
                    }
                    for s in chats.into_iter().take(20) {
                        let label = format!(
                            "{}{}",
                            if s.running { "• " } else { "" },
                            self.display(&s.title)
                        );
                        if ui
                            .selectable_label(omni.active.as_ref().map(|a| a.id) == Some(s.id), label)
                            .clicked()
                        {
                            self.selection.omni_session = Some(s.id);
                            self.request();
                            ui.close();
                        }
                    }
                });
                ui.menu_button("New", |ui| {
                    if ui
                        .button("Assistant chat")
                        .on_hover_text("Litecord tools only")
                        .clicked()
                    {
                        self.send(Command::Omni(OmniCommand::New(OmniMode::Assistant)));
                        ui.close();
                    }
                    if ui
                        .button("Workspace chat")
                        .on_hover_text("Adds sandboxed shell and file tools; each command asks you first")
                        .clicked()
                    {
                        self.send(Command::Omni(OmniCommand::New(OmniMode::Workspace)));
                        ui.close();
                    }
                    if ui
                        .button("Computer-use chat")
                        .on_hover_text("Uses the computer-use tools configured in your harness; you can stop it at any time")
                        .clicked()
                    {
                        self.send(Command::Omni(OmniCommand::New(OmniMode::ComputerUse)));
                        ui.close();
                    }
                });
            });
        });
        if let Some(active) = &omni.active {
            ui.horizontal(|ui| {
                let (color, mode) = match active.mode {
                    OmniMode::Assistant => (theme::OMNI, "Assistant · Litecord tools only"),
                    OmniMode::Workspace => {
                        (theme::WARNING, "Workspace · shell and files ask first")
                    }
                    OmniMode::ComputerUse => {
                        (theme::PRIORITY, "Computer use · watch and stop anytime")
                    }
                };
                theme::chip(ui, mode, color);
                if active.tokens_in + active.tokens_out > 0 {
                    ui.label(theme::meta(format!(
                        "{} turn{} · {}k tokens",
                        active.turns,
                        if active.turns == 1 { "" } else { "s" },
                        (active.tokens_in + active.tokens_out).div_ceil(1000)
                    )));
                }
            });
        }
    }

    /// Setup states. Returns true when chatting is possible.
    fn omni_ready(&mut self, ui: &mut Ui, omni: &OmniViewModel) -> bool {
        let status = &omni.status;
        if let Some(why) = &status.unavailable {
            theme::empty_state(ui, "Omni is unavailable", why);
            return false;
        }
        match &status.login {
            LoginState::Ready { .. } | LoginState::Stopped => true,
            LoginState::NotInstalled => {
                theme::empty_state(
                    ui,
                    "Connect an agent harness",
                    "Omni runs on your own Codex or OpenCode, signed in with its own account. \
                     Install one of them, then reopen this panel. Litecord never sees your \
                     provider credentials.",
                );
                false
            }
            LoginState::SignedOut => {
                theme::empty_state(
                    ui,
                    "Sign in to use Omni",
                    "You sign in with the harness's own login (for Codex: your ChatGPT account). \
                     A browser window opens; Litecord only learns that you are signed in.",
                );
                ui.horizontal(|ui| self.sign_in_menu(ui, omni));
                ui.label(theme::meta(
                    "API keys and other methods are also in Settings, Omni section.",
                ));
                false
            }
            LoginState::SigningIn {
                url,
                instructions,
                needs_code,
            } => {
                theme::empty_state(
                    ui,
                    "Finish signing in in your browser",
                    instructions
                        .as_deref()
                        .unwrap_or("This panel updates when the harness reports success."),
                );
                if *needs_code {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.omni_code_draft)
                                .hint_text("Paste the code")
                                .desired_width(200.0),
                        );
                        let code = self.omni_code_draft.trim().to_owned();
                        if ui
                            .add_enabled(
                                !code.is_empty() && !self.busy,
                                egui::Button::new("Submit"),
                            )
                            .clicked()
                        {
                            self.send(Command::Omni(OmniCommand::SubmitCode(code)));
                            self.omni_code_draft.clear();
                        }
                    });
                }
                if let Some(url) = url {
                    if ui.button("Open sign-in page again").clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                    }
                }
                if ui.button("Check again").clicked() {
                    self.send(Command::Omni(OmniCommand::Refresh));
                }
                false
            }
            LoginState::Error { message } => {
                theme::empty_state(ui, "The harness reported an error", message);
                if ui.button("Retry").clicked() {
                    self.send(Command::Omni(OmniCommand::Refresh));
                }
                false
            }
        }
    }

    fn omni_request(&mut self, ui: &mut Ui, req: &OmniRequestRow) {
        let (title, detail) = match &req.request {
            RequestKind::Command { command, cwd } => (
                "Run a command?",
                match cwd {
                    Some(cwd) => format!("{command}\nin {cwd}"),
                    None => command.clone(),
                },
            ),
            RequestKind::FileChange { summary } => ("Change files?", summary.clone()),
            RequestKind::Permission { title } => ("Allow?", title.clone()),
        };
        egui::Frame::new()
            .fill(theme::WARNING.gamma_multiply(0.08))
            .stroke(Stroke::new(1.0_f32, theme::WARNING.gamma_multiply(0.5)))
            .corner_radius(8)
            .inner_margin(10)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(title).color(theme::WARNING).strong());
                ui.label(RichText::new(detail).monospace().color(theme::TEXT));
                if let Some(reason) = &req.reason {
                    ui.label(theme::meta(reason.as_str()));
                }
                ui.label(theme::meta(
                    "Local action on this computer, not a Discord action.",
                ));
                ui.horizontal(|ui| {
                    let answer = |d| Command::Omni(OmniCommand::Answer(req.id.clone(), d));
                    if ui.button("Allow once").clicked() {
                        self.send(answer(Decision::Accept));
                    }
                    if ui
                        .button("Allow for this chat")
                        .on_hover_text("Similar requests in this chat run without asking")
                        .clicked()
                    {
                        self.send(answer(Decision::AcceptForSession));
                    }
                    if ui.button("Decline").clicked() {
                        self.send(answer(Decision::Decline));
                    }
                });
            });
        ui.add_space(6.0);
    }

    fn omni_item(&mut self, ui: &mut Ui, session: Option<&OmniSessionRow>, item: &OmniItem) {
        let text = self.display(&item.text);
        match (item.role.as_str(), item.kind.as_str()) {
            ("user", _) => {
                ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
                    egui::Frame::new()
                        .fill(theme::SELECTED)
                        .corner_radius(10)
                        .inner_margin(egui::Margin::symmetric(10, 7))
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width() * 0.85);
                            ui.label(RichText::new(text).color(theme::TEXT));
                        });
                });
                ui.add_space(4.0);
            }
            ("omni", "message") => {
                omni_bubble(ui, &text, false);
                ui.horizontal(|ui| {
                    if ui
                        .small_button("Remember")
                        .on_hover_text("Save to Memory as an Omni-derived fact for you to review")
                        .clicked()
                    {
                        if let Some(s) = session {
                            self.send(Command::Omni(OmniCommand::Remember(s.id, item.seq)));
                        }
                    }
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(item.text.clone());
                    }
                });
                ui.add_space(4.0);
            }
            ("system", "message") if item.text.starts_with("Error:") => {
                if item
                    .text
                    .contains("No endpoints found that support tool use")
                {
                    ui.label(
                        RichText::new("This model cannot use Omni's Litecord tools through its current provider route.")
                            .size(12.0)
                            .color(theme::PRIORITY),
                    );
                    ui.label(theme::meta(
                        "Choose another model above, then start a new Assistant chat. The model setting applies to new chats.",
                    ));
                    ui.collapsing("Provider details", |ui| {
                        ui.label(theme::meta(text));
                    });
                } else {
                    ui.label(RichText::new(text).size(12.0).color(theme::PRIORITY));
                }
            }
            ("system", "message") => {
                // Check-in prompts and other system turns: collapsed.
                ui.collapsing(theme::meta("Scheduled check-in"), |ui| {
                    ui.label(theme::meta(text));
                });
            }
            (_, kind) => {
                let icon = match kind {
                    "tool_call" => "Used",
                    "command" => "Command",
                    "file_change" => "Files",
                    "subagent" => "Subagent",
                    "plan" => "Plan",
                    "compaction" => "Context",
                    _ => "",
                };
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(icon).size(12.0).color(theme::OMNI));
                    ui.label(theme::meta(text));
                });
            }
        }
    }

    fn omni_composer(&mut self, ui: &mut Ui, omni: &OmniViewModel, running: bool) {
        ui.separator();
        let edit = egui::TextEdit::multiline(&mut self.omni_draft)
            .desired_rows(2)
            .desired_width(f32::INFINITY)
            .hint_text("Ask Omni about your people, messages and tasks…");
        let response = ui.add(edit);
        let enter = response.has_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        let session = omni.active.as_ref().map(|a| a.id);
        ui.horizontal(|ui| {
            ui.label(theme::meta("Enter to send · Shift+Enter for a new line"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if running {
                    if let Some(id) = session {
                        if ui.button("Stop").clicked() {
                            self.send(Command::Omni(OmniCommand::Interrupt(id)));
                        }
                    }
                } else {
                    let text = self.omni_draft.trim().to_owned();
                    let can = !text.is_empty() && !self.busy;
                    let send = ui.add_enabled(
                        can,
                        egui::Button::new(RichText::new("Send").color(theme::SHELL))
                            .fill(theme::OMNI),
                    );
                    if can && (send.clicked() || enter) {
                        self.send(Command::Omni(OmniCommand::Send(session, text)));
                        self.omni_draft.clear();
                    }
                }
                if let Some(id) = session {
                    ui.menu_button("More", |ui| {
                        if ui
                            .button("Compact context")
                            .on_hover_text("Ask the harness to summarize earlier turns")
                            .clicked()
                        {
                            self.send(Command::Omni(OmniCommand::Compact(id)));
                            ui.close();
                        }
                        if ui.button("Archive chat").clicked() {
                            self.send(Command::Omni(OmniCommand::Archive(id)));
                            self.selection.omni_session = None;
                            self.request();
                            ui.close();
                        }
                    });
                }
            });
        });
        if enter {
            // Keep the newline Enter inserted out of the next message.
            self.omni_draft = self.omni_draft.trim_end_matches('\n').to_owned();
        }
    }
}

impl Workspace {
    /// Settings → Omni: harness, every sign-in method, model, check-ins.
    pub(crate) fn omni_settings(&mut self, ui: &mut Ui, omni: &OmniViewModel) {
        theme::section_label(ui, "Omni");
        egui::Frame::new()
            .fill(theme::SIDEBAR)
            .inner_margin(12)
            .corner_radius(8)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(theme::meta(
                    "Omni runs on your own agent harness, signed in with that harness's account. \
                     Litecord starts it only while you use Omni and never stores its credentials.",
                ));
                ui.add_space(6.0);
                self.harness_picker(ui, omni);
                ui.add_space(4.0);
                self.account_row(ui, omni);
                if omni.status.selected.is_some() {
                    ui.add_space(4.0);
                    self.model_row(ui, omni);
                }
                if let Some(why) = &omni.status.unavailable {
                    ui.label(RichText::new(why).color(theme::WARNING));
                }
                ui.add_space(6.0);
                let mut on = omni.heartbeat_enabled;
                if ui
                    .checkbox(&mut on, "Scheduled check-ins")
                    .on_hover_text("Omni looks at what changed and adds anything important to your Inbox. It never sends messages on its own.")
                    .changed()
                {
                    self.send(Command::Omni(OmniCommand::Heartbeats(on)));
                }
                ui.label(theme::meta(
                    "Check-ins run only when something changed, at most twice an hour, and not during quiet hours.",
                ));
            });
        ui.add_space(8.0);
        self.automations_settings(ui, omni);
    }

    fn harness_picker(&mut self, ui: &mut Ui, omni: &OmniViewModel) {
        ui.horizontal(|ui| {
            ui.label("Harness");
            let selected = omni.status.selected;
            for h in &omni.status.harnesses {
                let label = if h.installed {
                    h.label.to_owned()
                } else {
                    format!("{} (not installed)", h.label)
                };
                let r = ui.add_enabled(
                    h.installed && !self.busy,
                    egui::Button::selectable(selected == Some(h.kind), label),
                );
                if r.clicked() && selected != Some(h.kind) {
                    self.omni_models_requested_for = None;
                    self.send(Command::Omni(OmniCommand::Select(h.kind)));
                }
            }
        });
        if omni
            .status
            .harnesses
            .iter()
            .all(|h| !h.installed || h.kind == HarnessKind::Fake)
        {
            ui.label(theme::meta(
                "Install Codex (npm i -g @openai/codex) or OpenCode (opencode.ai), then restart Litecord.",
            ));
        }
    }

    fn account_row(&mut self, ui: &mut Ui, omni: &OmniViewModel) {
        let login = &omni.status.login;
        ui.horizontal(|ui| {
            ui.label("Account");
            let (text, color) = match login {
                LoginState::Ready { account } => (
                    format!(
                        "Signed in{}",
                        account
                            .as_deref()
                            .map(|a| format!(" · {a}"))
                            .unwrap_or_default()
                    ),
                    theme::SUCCESS,
                ),
                LoginState::Stopped => (
                    "Not running (starts when you use Omni)".to_owned(),
                    theme::MUTED,
                ),
                LoginState::SignedOut => ("Signed out".to_owned(), theme::WARNING),
                LoginState::SigningIn { .. } => ("Signing in…".to_owned(), theme::WARNING),
                LoginState::NotInstalled => ("No harness".to_owned(), theme::MUTED),
                LoginState::Error { message } => (format!("Error: {message}"), theme::PRIORITY),
            };
            ui.label(RichText::new(text).color(color));
            if omni.status.selected.is_none() {
                return;
            }
            match login {
                LoginState::Ready { .. } => {
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Sign out"))
                        .clicked()
                    {
                        self.send(Command::Omni(OmniCommand::SignOut));
                    }
                }
                LoginState::Stopped => {
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("Check"))
                        .clicked()
                    {
                        self.send(Command::Omni(OmniCommand::Refresh));
                    }
                }
                _ => self.sign_in_menu(ui, omni),
            }
        });
        if let LoginState::SigningIn {
            needs_code: true,
            instructions,
            ..
        } = login
        {
            ui.label(theme::meta(
                instructions
                    .as_deref()
                    .unwrap_or("Paste the code shown in your browser."),
            ));
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.omni_code_draft)
                        .hint_text("Sign-in code")
                        .desired_width(220.0),
                );
                let code = self.omni_code_draft.trim().to_owned();
                if ui
                    .add_enabled(
                        !code.is_empty() && !self.busy,
                        egui::Button::new("Submit code"),
                    )
                    .clicked()
                {
                    self.send(Command::Omni(OmniCommand::SubmitCode(code)));
                    self.omni_code_draft.clear();
                }
            });
        }
        if let Some(option) = self.omni_key_option.clone() {
            let label = omni
                .status
                .login_options
                .iter()
                .find(|o| o.id == option)
                .map_or("API key", |o| o.label.as_str())
                .to_owned();
            ui.label(theme::meta(format!(
                "{label}: the key goes straight to the harness, which stores it. Litecord keeps no copy."
            )));
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.omni_key_draft)
                        .password(true)
                        .hint_text("Paste API key")
                        .desired_width(280.0),
                );
                let ok = self.omni_key_draft.trim().len() >= 8 && !self.busy;
                if ui.add_enabled(ok, egui::Button::new("Sign in")).clicked() {
                    let key = std::mem::take(&mut self.omni_key_draft);
                    self.send(Command::Omni(OmniCommand::ApiKey(
                        option.clone(),
                        litecord_core::secrets::Secret::new(key),
                    )));
                    self.omni_key_option = None;
                }
                if ui.button("Cancel").clicked() {
                    self.omni_key_draft.clear();
                    self.omni_key_option = None;
                }
            });
        }
    }

    fn sign_in_menu(&mut self, ui: &mut Ui, omni: &OmniViewModel) {
        let options = omni.status.login_options.clone();
        let r = ui.menu_button("Sign in…", |ui| {
            ui.set_min_width(260.0);
            if options.is_empty() {
                ui.label(theme::meta("Loading sign-in methods…"));
            }
            for o in &options {
                let text = match o.kind {
                    LoginKind::Browser => format!("{} (browser)", o.label),
                    LoginKind::ApiKey => format!("{} (API key)", o.label),
                };
                if ui.button(text).clicked() {
                    match o.kind {
                        LoginKind::Browser => {
                            self.send(Command::Omni(OmniCommand::SignInWith(o.id.clone())))
                        }
                        LoginKind::ApiKey => {
                            self.omni_key_option = Some(o.id.clone());
                            self.omni_key_draft.clear();
                        }
                    }
                    ui.close();
                }
            }
        });
        if r.response.clicked() && options.is_empty() && !self.busy {
            self.send(Command::Omni(OmniCommand::LoadLoginOptions));
        }
    }

    fn model_row(&mut self, ui: &mut Ui, omni: &OmniViewModel) {
        ui.horizontal(|ui| {
            ui.label("Model");
            let current = omni.status.model.clone();
            let shown = current.clone().unwrap_or_else(|| "Harness default".into());
            let mut choice = current.clone();
            ui.menu_button(egui::RichText::new(&shown).size(13.0), |ui| {
                ui.set_min_width(320.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.omni_model_query)
                        .hint_text("Search models")
                        .desired_width(f32::INFINITY),
                );
                if ui
                    .selectable_label(current.is_none(), "Harness default")
                    .clicked()
                {
                    choice = None;
                    ui.close();
                }
                ui.separator();
                let query = self.omni_model_query.trim().to_lowercase();
                let matches: Vec<_> = omni
                    .status
                    .models
                    .iter()
                    .filter(|model| model.to_lowercase().contains(&query))
                    .collect();
                ui.label(theme::meta(format!("{} matching models", matches.len())));
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        for model in matches.into_iter().take(80) {
                            if ui
                                .selectable_label(current.as_ref() == Some(model), model)
                                .clicked()
                            {
                                choice = Some(model.clone());
                                ui.close();
                            }
                        }
                    });
            });
            if choice != current {
                self.send(Command::Omni(OmniCommand::SetModel(choice)));
            }
        });
        ui.label(theme::meta("Applies to new chats"));
        if omni.status.models.is_empty() {
            ui.horizontal(|ui| {
                ui.label(theme::meta("No models loaded from this harness."));
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Refresh models"))
                    .clicked()
                {
                    self.send(Command::Omni(OmniCommand::LoadModels));
                }
            });
        }
    }
}

/// New-automation form state.
#[derive(Debug, Clone)]
pub struct AutomationForm {
    pub open: bool,
    pub name: String,
    pub prompt: String,
    /// 0 daily, 1 every N hours, 2 DM from, 3 keyword.
    pub trigger: usize,
    pub time: String,
    pub weekdays_only: bool,
    pub hours: u16,
    pub user: Option<litecord_types::UserId>,
    pub keyword: String,
    pub output: AutomationOutputKind,
}

impl Default for AutomationForm {
    fn default() -> Self {
        Self {
            open: false,
            name: String::new(),
            prompt: String::new(),
            trigger: 0,
            time: "08:00".into(),
            weekdays_only: false,
            hours: 4,
            user: None,
            keyword: String::new(),
            output: AutomationOutputKind::Inbox,
        }
    }
}

impl AutomationForm {
    /// The draft, or why it isn't valid yet.
    pub fn draft(&self) -> Result<AutomationDraft, &'static str> {
        if self.name.trim().is_empty() {
            return Err("Name it");
        }
        if self.prompt.trim().is_empty() {
            return Err("Tell Omni what to do");
        }
        let trigger = match self.trigger {
            0 => {
                let (h, m) = self.time.trim().split_once(':').ok_or("Time as HH:MM")?;
                let (h, m): (u16, u16) = (
                    h.parse().map_err(|_| "Time as HH:MM")?,
                    m.parse().map_err(|_| "Time as HH:MM")?,
                );
                if h > 23 || m > 59 {
                    return Err("Time as HH:MM");
                }
                AutomationTrigger::Daily {
                    minute_of_day: h * 60 + m,
                    weekdays: if self.weekdays_only { 0x1f } else { 0 },
                }
            }
            1 => AutomationTrigger::Every {
                hours: self.hours.clamp(1, 168),
            },
            2 => AutomationTrigger::DirectMessageFrom {
                user_id: self.user.ok_or("Pick a person")?,
            },
            _ => {
                if self.keyword.trim().chars().count() < 2 {
                    return Err("Keyword of 2+ characters");
                }
                AutomationTrigger::Keyword {
                    keyword: self.keyword.trim().to_owned(),
                }
            }
        };
        Ok(AutomationDraft {
            name: self.name.trim().to_owned(),
            prompt: self.prompt.trim().to_owned(),
            trigger,
            output: self.output,
        })
    }
}

impl Workspace {
    fn automations_settings(&mut self, ui: &mut Ui, omni: &OmniViewModel) {
        theme::section_label(ui, "Automations");
        egui::Frame::new()
            .fill(theme::SIDEBAR)
            .inner_margin(12)
            .corner_radius(8)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(theme::meta(
                    "Omni runs these on its own, in Assistant mode (Litecord tools only). Results \
                     go to your Inbox, suggested tasks or drafts; nothing is ever sent without you.",
                ));
                ui.add_space(4.0);
                if omni.automations.is_empty() {
                    ui.label(theme::meta("No automations yet. Start from a preset or build your own."));
                }
                for a in &omni.automations {
                    ui.horizontal(|ui| {
                        let mut on = a.enabled;
                        if ui.checkbox(&mut on, "").on_hover_text("Enabled").changed() {
                            self.send(Command::Omni(OmniCommand::SetAutomationEnabled(a.id, on)));
                        }
                        ui.vertical(|ui| {
                            ui.label(RichText::new(self.display(&a.name)).color(theme::TEXT));
                            let mut meta = format!("{} · {}", a.trigger_label, a.output.label());
                            if a.runs > 0 {
                                meta.push_str(&format!(" · ran {} time{}", a.runs, if a.runs == 1 { "" } else { "s" }));
                            }
                            ui.label(theme::meta(meta));
                            if let Some(e) = &a.last_error {
                                ui.label(RichText::new(format!("Last run failed: {e}")).size(12.0).color(theme::PRIORITY));
                            }
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Delete").clicked() {
                                self.send(Command::Omni(OmniCommand::DeleteAutomation(a.id)));
                            }
                            if let Some(sid) = a.session_id {
                                if ui.small_button("Open").on_hover_text("See this automation's Omni chat").clicked() {
                                    self.selection.omni_session = Some(sid);
                                    self.omni_open = true;
                                    self.request();
                                }
                            }
                            if ui.add_enabled(!self.busy, egui::Button::new("Run now").small()).clicked() {
                                self.send(Command::Omni(OmniCommand::RunAutomation(a.id)));
                            }
                        });
                    });
                    ui.separator();
                }
                ui.horizontal_wrapped(|ui| {
                    ui.label(theme::meta("Presets:"));
                    for p in litecord_app::automations::presets() {
                        let exists = omni.automations.iter().any(|a| a.name == p.name);
                        if ui
                            .add_enabled(!exists && !self.busy, egui::Button::new(&p.name).small())
                            .on_hover_text(format!("{} · {}\n{}", p.trigger.describe(), p.output.label(), p.prompt))
                            .clicked()
                        {
                            self.send(Command::Omni(OmniCommand::CreateAutomation(p)));
                        }
                    }
                    if ui.small_button(if self.automation_form.open { "Close form" } else { "Custom…" }).clicked() {
                        self.automation_form.open = !self.automation_form.open;
                    }
                });
                if self.automation_form.open {
                    self.automation_form_ui(ui);
                }
            });
    }

    fn automation_form_ui(&mut self, ui: &mut Ui) {
        let friends: Vec<(litecord_types::UserId, String)> = self
            .snapshot
            .as_ref()
            .map(|s| {
                s.friends
                    .online
                    .iter()
                    .chain(&s.friends.offline)
                    .map(|f| (f.user_id, f.display_name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let private = self.private();
        let f = &mut self.automation_form;
        ui.add_space(6.0);
        egui::Grid::new("automation_form").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut f.name).hint_text("e.g. Evening wrap-up").desired_width(260.0));
            ui.end_row();
            ui.label("When");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("automation_trigger")
                    .selected_text(["Every day at", "Every N hours", "New DM from", "Message mentions"][f.trigger.min(3)])
                    .show_ui(ui, |ui| {
                        for (i, l) in ["Every day at", "Every N hours", "New DM from", "Message mentions"].into_iter().enumerate() {
                            ui.selectable_value(&mut f.trigger, i, l);
                        }
                    });
                match f.trigger {
                    0 => {
                        ui.add(egui::TextEdit::singleline(&mut f.time).desired_width(56.0));
                        ui.label(theme::meta("UTC"));
                        ui.checkbox(&mut f.weekdays_only, "weekdays only");
                    }
                    1 => {
                        ui.add(egui::DragValue::new(&mut f.hours).range(1..=168).suffix(" h"));
                    }
                    2 => {
                        let shown = f
                            .user
                            .and_then(|u| friends.iter().find(|(id, _)| *id == u))
                            .map_or_else(|| "Choose a person".to_owned(), |(_, n)| {
                                if private { "Hidden user".to_owned() } else { n.clone() }
                            });
                        egui::ComboBox::from_id_salt("automation_user").selected_text(shown).show_ui(ui, |ui| {
                            for (id, name) in &friends {
                                let label = if private { "Hidden user" } else { name.as_str() };
                                ui.selectable_value(&mut f.user, Some(*id), label);
                            }
                        });
                    }
                    _ => {
                        ui.add(egui::TextEdit::singleline(&mut f.keyword).hint_text("keyword").desired_width(160.0));
                    }
                }
            });
            ui.end_row();
            ui.label("Result");
            egui::ComboBox::from_id_salt("automation_output")
                .selected_text(f.output.label())
                .show_ui(ui, |ui| {
                    for o in [AutomationOutputKind::Inbox, AutomationOutputKind::Tasks, AutomationOutputKind::Drafts] {
                        ui.selectable_value(&mut f.output, o, o.label());
                    }
                });
            ui.end_row();
            ui.label("Ask Omni to");
            ui.add(
                egui::TextEdit::multiline(&mut f.prompt)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("e.g. Tell me which conversations need a reply and suggest one line for each."),
            );
            ui.end_row();
        });
        let draft = self.automation_form.draft();
        ui.horizontal(|ui| {
            let ok = draft.is_ok() && !self.busy;
            if ui
                .add_enabled(ok, egui::Button::new("Create automation"))
                .clicked()
            {
                if let Ok(d) = draft.clone() {
                    self.send(Command::Omni(OmniCommand::CreateAutomation(d)));
                    self.automation_form = AutomationForm::default();
                }
            }
            if let Err(why) = &draft {
                ui.label(theme::meta(*why));
            }
        });
    }
}

fn omni_intro(ui: &mut Ui) {
    ui.add_space(8.0);
    ui.label(RichText::new("What can Omni do?").color(theme::SECONDARY));
    for line in [
        "Catch you up: “What did I miss today?”",
        "Find commitments: “What did I promise Ada?”",
        "Draft replies you approve before anything is sent",
        "Turn conversations into tasks and reminders",
    ] {
        ui.label(theme::meta(format!("• {line}")));
    }
    ui.add_space(4.0);
    ui.label(theme::meta(
        "Omni reads Litecord memory through its tools and never sends to Discord without your approval.",
    ));
    ui.add_space(8.0);
}

fn omni_bubble(ui: &mut Ui, text: &str, streaming: bool) {
    egui::Frame::new()
        .fill(theme::OMNI.gamma_multiply(0.06))
        .stroke(Stroke::new(
            1.0_f32,
            theme::OMNI.gamma_multiply(if streaming { 0.5 } else { 0.2 }),
        ))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(theme::TEXT));
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_form_builds_valid_drafts_only() {
        let mut f = AutomationForm {
            name: "Wrap-up".into(),
            prompt: "Summarize my day".into(),
            time: "18:30".into(),
            weekdays_only: true,
            ..AutomationForm::default()
        };
        let d = f.draft().unwrap();
        assert_eq!(
            d.trigger,
            AutomationTrigger::Daily {
                minute_of_day: 18 * 60 + 30,
                weekdays: 0x1f
            }
        );
        f.time = "25:00".into();
        assert!(f.draft().is_err());
        f.trigger = 2;
        assert_eq!(f.draft().unwrap_err(), "Pick a person");
        f.trigger = 3;
        f.keyword = "x".into();
        assert!(f.draft().is_err());
        f.keyword = "launch".into();
        assert!(matches!(
            f.draft().unwrap().trigger,
            AutomationTrigger::Keyword { .. }
        ));
        f.name = " ".into();
        assert_eq!(f.draft().unwrap_err(), "Name it");
    }
}
