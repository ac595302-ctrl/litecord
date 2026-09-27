//! Omni sign-in, shared by Settings → Omni and the Omni panel before Omni is
//! ready: pick Codex or OpenCode, then sign in with that harness's own
//! account (ChatGPT in the browser, a device code, or a provider API key).
//! Keys go straight to the harness, which stores them; Litecord keeps no
//! copy and only learns whether the harness is signed in.

use eframe::egui::{self, Align2, Color32, Rect, Sense, Ui};
use litecord_app::harness::{HarnessKind, LoginInputs, LoginKind, LoginOption, LoginState};
use litecord_app::omni::{login_groups, HarnessInfo, LoginGroup};
use litecord_app::OmniViewModel;

use crate::bridge::{Command, OmniCommand};
use crate::{kit, ph, theme, workspace::Workspace};

/// Groups shown under "More providers" before the list asks for a search.
const MORE_LIMIT: usize = 30;

impl Workspace {
    /// Harness choice, account state, the open sign-in form and every
    /// sign-in method the harness offers.
    pub(crate) fn omni_setup(&mut self, ui: &mut Ui, omni: &OmniViewModel) {
        self.harness_cards(ui, omni);
        let Some(kind) = omni.status.selected else {
            return;
        };
        if kind == HarnessKind::Fake {
            return;
        }
        ui.add_space(14.0);
        self.load_login_options(omni, kind);
        self.login_state_card(ui, omni, kind);
        if let Some(id) = self.omni_key_option.clone() {
            if let Some(option) = omni.status.login_options.iter().find(|o| o.id == id) {
                ui.add_space(10.0);
                self.login_form(ui, kind, option);
            }
        }
        let ready = matches!(omni.status.login, LoginState::Ready { .. });
        let methods = match &omni.status.login {
            LoginState::SignedOut | LoginState::Error { .. } => true,
            // OpenCode signs in per provider; keep adding them possible.
            LoginState::Ready { .. } => kind == HarnessKind::OpenCode,
            _ => false,
        };
        if methods {
            ui.add_space(16.0);
            self.login_methods(ui, omni, kind, ready);
        }
    }

    /// Ask the harness for its sign-in methods once per harness, when they
    /// can be shown.
    fn load_login_options(&mut self, omni: &OmniViewModel, kind: HarnessKind) {
        // Not started yet: start it once so the card shows the real state
        // and the sign-in methods, rather than asking the user to check.
        if matches!(omni.status.login, LoginState::Stopped)
            && self.omni_checked_for != Some(kind)
            && !self.busy
        {
            self.omni_checked_for = Some(kind);
            self.send(Command::Omni(OmniCommand::Refresh));
            return;
        }
        let wanted = matches!(
            omni.status.login,
            LoginState::SignedOut | LoginState::Error { .. }
        ) || (kind == HarnessKind::OpenCode
            && matches!(omni.status.login, LoginState::Ready { .. }));
        if wanted
            && omni.status.login_options.is_empty()
            && self.omni_options_for != Some(kind)
            && !self.busy
        {
            self.omni_options_for = Some(kind);
            self.send(Command::Omni(OmniCommand::LoadLoginOptions));
        }
    }

    /// Codex and OpenCode side by side; the selected one is outlined.
    fn harness_cards(&mut self, ui: &mut Ui, omni: &OmniViewModel) {
        let shown: Vec<&HarnessInfo> = omni
            .status
            .harnesses
            .iter()
            .filter(|h| h.kind != HarnessKind::Fake || h.installed)
            .collect();
        if shown.is_empty() {
            return;
        }
        let gap = 12.0;
        let width = ui.available_width();
        // Stack the cards when each would be narrower than ~210px.
        let columns =
            (((width + gap) / (210.0 + gap)).floor() as usize).clamp(1, shown.len().min(3));
        let card_w = (width - gap * (columns as f32 - 1.0)) / columns as f32;
        let height = if columns == 1 { 66.0 } else { 84.0 };
        let rows = shown.len().div_ceil(columns);
        let (area, _) = ui.allocate_exact_size(
            egui::vec2(width, rows as f32 * height + (rows as f32 - 1.0) * gap),
            Sense::hover(),
        );
        for (i, h) in shown.iter().enumerate() {
            let (col, row) = (i % columns, i / columns);
            let rect = Rect::from_min_size(
                area.min + egui::vec2(col as f32 * (card_w + gap), row as f32 * (height + gap)),
                egui::vec2(card_w, height),
            );
            let selected = omni.status.selected == Some(h.kind);
            let resp = ui.interact(
                rect,
                ui.id().with(("harness", h.kind.as_str())),
                Sense::click(),
            );
            let painter = ui.painter();
            let (fill, stroke) = if selected {
                (
                    theme::lerp(theme::CARD, theme::PRIMARY, 0.12),
                    theme::lerp(theme::CARD, theme::PRIMARY, 0.75),
                )
            } else if resp.hovered() && h.installed {
                (theme::lerp(theme::CARD, theme::HOVER, 0.7), theme::BORDER)
            } else {
                (theme::CARD, theme::BORDER)
            };
            painter.rect(
                rect,
                14.0,
                fill,
                egui::Stroke::new(1.0_f32, stroke),
                egui::StrokeKind::Inside,
            );
            let (glyph, tint) = harness_look(h.kind);
            let tile = Rect::from_center_size(
                egui::pos2(rect.left() + 38.0, rect.center().y),
                egui::vec2(44.0, 44.0),
            );
            kit::paint_tile(painter, tile, glyph, tint, 12.0);
            let x = tile.right() + 14.0;
            let text_w = rect.right() - x - 36.0;
            kit::text_at(
                painter,
                egui::pos2(x, rect.center().y - 11.0),
                Align2::LEFT_CENTER,
                h.label,
                theme::semibold(16.0),
                theme::TEXT,
                text_w,
            );
            let (status, color) = if h.installed {
                (
                    h.path
                        .as_deref()
                        .map_or_else(|| "Installed".to_owned(), |p| format!("Installed · {p}")),
                    theme::SECONDARY,
                )
            } else {
                (format!("Not installed · {}", h.install_hint), theme::MUTED)
            };
            if h.installed {
                kit::dot(
                    painter,
                    egui::pos2(x + 4.0, rect.center().y + 12.0),
                    3.5,
                    theme::SUCCESS,
                );
            }
            let sx = if h.installed { x + 14.0 } else { x };
            kit::text_at(
                painter,
                egui::pos2(sx, rect.center().y + 12.0),
                Align2::LEFT_CENTER,
                &status,
                theme::regular(13.0),
                color,
                rect.right() - sx - 14.0,
            );
            if selected {
                kit::icon(
                    painter,
                    egui::pos2(rect.right() - 20.0, rect.top() + 20.0),
                    ph::CHECK_CIRCLE,
                    18.0,
                    theme::PRIMARY_TEXT,
                );
            }
            let tip = if h.installed {
                if selected {
                    format!("Omni uses {}", h.label)
                } else {
                    format!("Use {} for Omni", h.label)
                }
            } else {
                format!("Install with: {}", h.install_hint)
            };
            let resp = resp.on_hover_text(tip);
            if resp.clicked() && h.installed && !selected && !self.busy {
                self.omni_key_option = None;
                self.omni_options_for = None;
                self.send(Command::Omni(OmniCommand::Select(h.kind)));
            }
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            kit::label(
                ui,
                "Installed one just now?",
                theme::regular(13.5),
                theme::MUTED,
            );
            if kit::link(ui, "Look again").clicked() && !self.busy {
                self.send(Command::Omni(OmniCommand::Redetect));
            }
        });
    }

    /// Where sign-in stands, with the action that moves it forward.
    fn login_state_card(&mut self, ui: &mut Ui, omni: &OmniViewModel, kind: HarnessKind) {
        let harness = kind.label();
        let (glyph, tint, title, detail) = match &omni.status.login {
            LoginState::Ready { account } => (
                ph::CHECK_CIRCLE,
                kit::GREEN,
                "Signed in".to_owned(),
                account
                    .clone()
                    .unwrap_or_else(|| format!("{harness} is ready")),
            ),
            LoginState::Stopped => (
                ph::CIRCLE_NOTCH,
                kit::GREY,
                "Not running".to_owned(),
                format!("{harness} starts when you use Omni."),
            ),
            LoginState::SignedOut => (
                ph::KEY,
                kit::BLUE,
                "Not signed in".to_owned(),
                format!("Sign in with your {harness} account below."),
            ),
            LoginState::SigningIn { .. } => (
                ph::GLOBE,
                kit::TEAL,
                "Finish signing in".to_owned(),
                format!("This updates as soon as {harness} confirms."),
            ),
            LoginState::NotInstalled => (
                ph::WARNING,
                kit::YELLOW,
                format!("{harness} is not installed"),
                "Install it, then choose Look again.".to_owned(),
            ),
            LoginState::Error { message } => (
                ph::WARNING_CIRCLE,
                kit::RED,
                "Sign-in problem".to_owned(),
                message.clone(),
            ),
        };
        // Buttons, right to left.
        let mut actions: Vec<(&str, Option<&str>, kit::Kind, OmniCommand)> = Vec::new();
        match &omni.status.login {
            LoginState::Ready { .. } => actions.push((
                if kind == HarnessKind::OpenCode {
                    "Sign out all"
                } else {
                    "Sign out"
                },
                Some(ph::SIGN_OUT),
                kit::Kind::Secondary,
                OmniCommand::SignOut,
            )),
            LoginState::Stopped => actions.push((
                "Check sign-in",
                Some(ph::ARROWS_CLOCKWISE),
                kit::Kind::Secondary,
                OmniCommand::Refresh,
            )),
            LoginState::SigningIn { .. } => {
                actions.push(("Cancel", None, kit::Kind::Ghost, OmniCommand::CancelSignIn));
                actions.push((
                    "Check",
                    Some(ph::ARROWS_CLOCKWISE),
                    kit::Kind::Secondary,
                    OmniCommand::Refresh,
                ));
            }
            LoginState::Error { .. } => actions.push((
                "Retry",
                Some(ph::ARROWS_CLOCKWISE),
                kit::Kind::Secondary,
                OmniCommand::Refresh,
            )),
            _ => {}
        }
        let signing_url = match &omni.status.login {
            LoginState::SigningIn { url: Some(url), .. } => Some(url.clone()),
            _ => None,
        };

        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 72.0), Sense::hover());
        kit::paint_card(ui.painter(), rect, false);
        kit::paint_bubble(
            ui.painter(),
            egui::pos2(rect.left() + 38.0, rect.center().y),
            glyph,
            tint,
            44.0,
        );
        let mut right = rect.right() - 14.0;
        let mut clicked = None;
        for (i, (label, icon, style, _)) in actions.iter().enumerate() {
            let w = kit::button_width(ui.painter(), *icon, label, 34.0);
            let r = Rect::from_min_size(
                egui::pos2(right - w, rect.center().y - 17.0),
                egui::vec2(w, 34.0),
            );
            if kit::button_at(
                ui,
                r,
                ui.id().with(("login_action", i)),
                *style,
                *icon,
                label,
                !self.busy,
            )
            .clicked()
            {
                clicked = Some(i);
            }
            right = r.left() - 8.0;
        }
        if let Some(url) = &signing_url {
            let label = "Open page";
            let w = kit::button_width(ui.painter(), Some(ph::ARROW_SQUARE_OUT), label, 34.0);
            let r = Rect::from_min_size(
                egui::pos2(right - w, rect.center().y - 17.0),
                egui::vec2(w, 34.0),
            );
            if kit::button_at(
                ui,
                r,
                ui.id().with("login_open_page"),
                kit::Kind::Primary,
                Some(ph::ARROW_SQUARE_OUT),
                label,
                true,
            )
            .on_hover_text("Open the sign-in page in your browser again")
            .clicked()
            {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url.clone()));
            }
            right = r.left() - 8.0;
        }
        let x = rect.left() + 74.0;
        let text_w = (right - x - 8.0).max(40.0);
        let painter = ui.painter();
        kit::text_at(
            painter,
            egui::pos2(x, rect.center().y - 11.0),
            Align2::LEFT_CENTER,
            &title,
            theme::semibold(15.5),
            theme::TEXT,
            text_w,
        );
        let detail = self.display(&detail);
        let detail_color = if matches!(omni.status.login, LoginState::Error { .. }) {
            kit::RED.fg
        } else {
            theme::SECONDARY
        };
        let shown = kit::text_at(
            painter,
            egui::pos2(x, rect.center().y + 12.0),
            Align2::LEFT_CENTER,
            &detail,
            theme::regular(13.5),
            detail_color,
            text_w,
        );
        if shown.width() >= text_w - 1.0 {
            ui.interact(
                Rect::from_min_max(
                    egui::pos2(x, rect.center().y),
                    egui::pos2(x + text_w, rect.bottom()),
                ),
                ui.id().with("login_detail"),
                Sense::hover(),
            )
            .on_hover_text(detail.clone());
        }
        if let Some(i) = clicked {
            let cmd = actions.swap_remove(i).3;
            if matches!(cmd, OmniCommand::SignOut) {
                self.omni_key_option = None;
            }
            self.send(Command::Omni(cmd));
        }

        if let Some(url) = &signing_url {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                kit::label(
                    ui,
                    "Browser didn't open?",
                    theme::regular(13.5),
                    theme::MUTED,
                );
                if kit::link(ui, "Copy the sign-in link").clicked() {
                    ui.ctx().copy_text(url.clone());
                    self.notice = Some("Sign-in link copied.".into());
                }
            });
        }

        // Device codes and pasted codes.
        if let LoginState::SigningIn {
            instructions,
            needs_code,
            ..
        } = &omni.status.login
        {
            if let Some(text) = instructions.as_deref().filter(|t| !t.trim().is_empty()) {
                ui.add_space(8.0);
                kit::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        kit::bubble(ui, ph::INFO, kit::TEAL, 32.0);
                        ui.vertical(|ui| {
                            ui.set_width((ui.available_width() - 90.0).max(80.0));
                            kit::para(ui, text, theme::medium(14.5), theme::TEXT);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if kit::button_ex(
                                ui,
                                kit::Kind::Secondary,
                                Some(ph::COPY),
                                "Copy",
                                32.0,
                                true,
                            )
                            .clicked()
                            {
                                ui.ctx().copy_text(text.to_owned());
                            }
                        });
                    });
                });
            }
            if *needs_code {
                ui.add_space(8.0);
                kit::field(
                    ui,
                    &mut self.omni_code_draft,
                    "Paste the code from your browser",
                    false,
                    "omni_code",
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let code = self.omni_code_draft.trim().to_owned();
                    if kit::button_ex(
                        ui,
                        kit::Kind::Primary,
                        Some(ph::CHECK),
                        "Submit code",
                        34.0,
                        !code.is_empty() && !self.busy,
                    )
                    .clicked()
                    {
                        self.send(Command::Omni(OmniCommand::SubmitCode(code)));
                        self.omni_code_draft.clear();
                    }
                });
            }
        }
    }

    /// Featured providers first (ChatGPT sign-in as the main button), the
    /// rest behind "More providers".
    fn login_methods(&mut self, ui: &mut Ui, omni: &OmniViewModel, kind: HarnessKind, ready: bool) {
        let options = &omni.status.login_options;
        if options.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                kit::label(
                    ui,
                    "Asking the harness how you can sign in…",
                    theme::regular(14.0),
                    theme::SECONDARY,
                );
            });
            if kit::link(ui, "Ask again").clicked() && !self.busy {
                self.send(Command::Omni(OmniCommand::LoadLoginOptions));
            }
            return;
        }
        let groups = login_groups(options);
        let (main, more): (Vec<&LoginGroup>, Vec<&LoginGroup>) = groups
            .iter()
            .partition(|g| g.featured || g.connected || groups.len() <= 3);
        kit::label(
            ui,
            if ready {
                "Add a provider"
            } else {
                "Sign in with"
            },
            theme::semibold(16.0),
            theme::TEXT,
        );
        ui.add_space(8.0);
        // The main button: a ChatGPT (or first) browser sign-in, unless
        // that provider is already connected.
        let hero = main
            .iter()
            .filter(|g| !g.connected)
            .flat_map(|g| g.options.iter())
            .find(|o| o.kind == LoginKind::Browser && o.label.contains("ChatGPT"))
            .or_else(|| {
                main.iter()
                    .filter(|g| !g.connected)
                    .flat_map(|g| g.options.iter())
                    .find(|o| o.kind == LoginKind::Browser)
            })
            .cloned();
        if let Some(o) = &hero {
            let glyph = if o.label.contains("ChatGPT") || o.provider.as_deref() == Some("openai") {
                ph::OPEN_AI_LOGO
            } else {
                ph::SIGN_IN
            };
            let text = if o.label.starts_with("Sign in") || o.method_label.is_empty() {
                o.label.clone()
            } else {
                format!(
                    "Sign in with {}",
                    o.method_label.trim_end_matches(" (browser)")
                )
            };
            if kit::button_wide(
                ui,
                kit::Kind::Primary,
                Some(glyph),
                &text,
                ui.available_width(),
                44.0,
            )
            .on_hover_text("Opens your browser; Litecord only learns that you signed in")
            .clicked()
                && !self.busy
            {
                self.choose_option(o);
            }
            ui.add_space(10.0);
        }
        for g in &main {
            self.provider_card(ui, kind, g, hero.as_ref().map(|o| o.id.as_str()));
            ui.add_space(8.0);
        }
        if more.is_empty() {
            return;
        }
        ui.add_space(2.0);
        let caret = if self.omni_more_providers {
            ph::CARET_DOWN
        } else {
            ph::CARET_RIGHT
        };
        let label = format!("More providers ({})", more.len());
        if kit::button_ex(ui, kit::Kind::Ghost, Some(caret), &label, 32.0, true).clicked() {
            self.omni_more_providers = !self.omni_more_providers;
        }
        if !self.omni_more_providers {
            return;
        }
        ui.add_space(6.0);
        kit::search(ui, &mut self.omni_provider_filter, "Search providers...");
        ui.add_space(8.0);
        let needle = self.omni_provider_filter.trim().to_lowercase();
        let matching: Vec<&&LoginGroup> = more
            .iter()
            .filter(|g| {
                needle.is_empty()
                    || g.label.to_lowercase().contains(&needle)
                    || g.provider.contains(&needle)
            })
            .collect();
        for g in matching.iter().take(MORE_LIMIT) {
            self.provider_card(ui, kind, g, None);
            ui.add_space(8.0);
        }
        if matching.len() > MORE_LIMIT {
            kit::label(
                ui,
                format!(
                    "{} more; search to narrow the list.",
                    matching.len() - MORE_LIMIT
                ),
                theme::regular(13.5),
                theme::MUTED,
            );
        } else if matching.is_empty() {
            kit::label(
                ui,
                "No provider matches.",
                theme::regular(13.5),
                theme::MUTED,
            );
        }
    }

    /// One provider: heading row, then a line per sign-in method.
    fn provider_card(
        &mut self,
        ui: &mut Ui,
        kind: HarnessKind,
        g: &LoginGroup,
        skip: Option<&str>,
    ) {
        let methods: Vec<&LoginOption> = g
            .options
            .iter()
            .filter(|o| Some(o.id.as_str()) != skip)
            .collect();
        if methods.is_empty() && !g.connected {
            return;
        }
        let line_h = 40.0;
        let head_h = 56.0;
        let height =
            head_h + methods.len() as f32 * line_h + if methods.is_empty() { 0.0 } else { 8.0 };
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
        kit::paint_card(ui.painter(), rect, false);
        let name = if g.label.is_empty() {
            kind.label().to_owned()
        } else {
            g.label.clone()
        };
        let head = Rect::from_min_size(rect.min, egui::vec2(rect.width(), head_h));
        let tile = Rect::from_center_size(
            egui::pos2(head.left() + 32.0, head.center().y),
            egui::vec2(36.0, 36.0),
        );
        match provider_glyph(&g.provider) {
            Some(glyph) => kit::paint_tile(ui.painter(), tile, glyph, kit::tint_for(&name), 10.0),
            None => kit::paint_initials_tile(ui.painter(), tile, &name, kit::tint_for(&name), 10.0),
        }
        let mut right = head.right() - 14.0;
        if g.connected && kind == HarnessKind::OpenCode && !g.provider.is_empty() {
            let label = "Sign out";
            let w = kit::button_width(ui.painter(), None, label, 30.0);
            let r = Rect::from_min_size(
                egui::pos2(right - w, head.center().y - 15.0),
                egui::vec2(w, 30.0),
            );
            if kit::button_at(
                ui,
                r,
                ui.id().with(("provider_out", &g.provider)),
                kit::Kind::Ghost,
                None,
                label,
                !self.busy,
            )
            .on_hover_text(format!(
                "Remove the credential {} stored for {name}",
                kind.label()
            ))
            .clicked()
            {
                self.send(Command::Omni(OmniCommand::SignOutProvider(
                    g.provider.clone(),
                )));
            }
            right = r.left() - 8.0;
        }
        if g.connected {
            let text = "Connected";
            let w = kit::text_width(ui.painter(), text, theme::medium(12.0)) + 34.0;
            let r = Rect::from_min_size(
                egui::pos2(right - w, head.center().y - 12.0),
                egui::vec2(w, 24.0),
            );
            kit::paint_status_pill(ui.painter(), r, Some(ph::CHECK), text, theme::SUCCESS);
            right = r.left() - 8.0;
        }
        let x = tile.right() + 12.0;
        kit::text_at(
            ui.painter(),
            egui::pos2(x, head.center().y),
            Align2::LEFT_CENTER,
            &name,
            theme::semibold(15.0),
            theme::TEXT,
            (right - x - 6.0).max(30.0),
        );

        for (i, o) in methods.iter().enumerate() {
            let line = Rect::from_min_size(
                egui::pos2(rect.left() + 8.0, head.bottom() + i as f32 * line_h),
                egui::vec2(rect.width() - 16.0, line_h),
            );
            let resp = ui.interact(line, ui.id().with(("login_option", &o.id)), Sense::click());
            let painter = ui.painter();
            if resp.hovered() {
                painter.rect_filled(line, 8.0, theme::lerp(theme::CARD, theme::HOVER, 0.8));
            }
            let (glyph, action, color) = method_look(o);
            kit::icon(
                painter,
                egui::pos2(line.left() + 22.0, line.center().y),
                glyph,
                17.0,
                theme::SECONDARY,
            );
            let aw = kit::text_width(painter, action, theme::medium(13.5));
            kit::text_at(
                painter,
                egui::pos2(line.right() - 30.0, line.center().y),
                Align2::RIGHT_CENTER,
                action,
                theme::medium(13.5),
                color,
                aw + 1.0,
            );
            kit::icon(
                painter,
                egui::pos2(line.right() - 14.0, line.center().y),
                ph::CARET_RIGHT,
                13.0,
                color,
            );
            let label = if o.method_label.is_empty() {
                &o.label
            } else {
                &o.method_label
            };
            kit::text_at(
                painter,
                egui::pos2(line.left() + 42.0, line.center().y),
                Align2::LEFT_CENTER,
                label,
                theme::regular(14.5),
                theme::BODY,
                (line.width() - 42.0 - aw - 44.0).max(30.0),
            );
            if resp.on_hover_text(&o.label).clicked() && !self.busy {
                self.choose_option(o);
            }
        }
    }

    /// Browser methods without questions start at once; API keys and
    /// methods with questions open the form first.
    fn choose_option(&mut self, o: &LoginOption) {
        if o.kind == LoginKind::Browser && o.prompts.is_empty() {
            self.omni_key_option = None;
            self.send(Command::Omni(OmniCommand::SignInWith(
                o.id.clone(),
                LoginInputs::new(),
            )));
            return;
        }
        self.omni_key_option = Some(o.id.clone());
        self.omni_key_draft.clear();
        self.omni_login_inputs = o
            .prompts
            .iter()
            .filter_map(|p| p.choices.first().map(|c| (p.key.clone(), c.value.clone())))
            .collect();
    }

    /// The open form: the method's questions, the key (for API keys), and
    /// the button that hands them to the harness.
    fn login_form(&mut self, ui: &mut Ui, kind: HarnessKind, o: &LoginOption) {
        let title = match (&o.provider_label, o.method_label.is_empty()) {
            (Some(p), false) if !o.method_label.contains(p.as_str()) => {
                format!("{p} · {}", o.method_label)
            }
            (_, false) => o.method_label.clone(),
            _ => o.label.clone(),
        };
        kit::card(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.horizontal(|ui| {
                kit::bubble(
                    ui,
                    if o.kind == LoginKind::ApiKey {
                        ph::KEY
                    } else {
                        ph::SIGN_IN
                    },
                    kit::BLUE,
                    36.0,
                );
                kit::label(ui, &title, theme::semibold(16.0), theme::TEXT);
            });
            for p in &o.prompts {
                if p.when
                    .as_ref()
                    .is_some_and(|w| !w.applies(&self.omni_login_inputs))
                {
                    continue;
                }
                kit::label(ui, &p.message, theme::medium(14.0), theme::BODY);
                if p.choices.is_empty() {
                    let value = self.omni_login_inputs.entry(p.key.clone()).or_default();
                    kit::field(
                        ui,
                        value,
                        p.placeholder.as_deref().unwrap_or(""),
                        false,
                        &p.key,
                    );
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for c in &p.choices {
                            let on = self.omni_login_inputs.get(&p.key) == Some(&c.value);
                            let r = kit::filter_chip(ui, &c.label, on);
                            let r = match &c.hint {
                                Some(h) => r.on_hover_text(h),
                                None => r,
                            };
                            if r.clicked() {
                                self.omni_login_inputs
                                    .insert(p.key.clone(), c.value.clone());
                            }
                        }
                    });
                }
            }
            if o.kind == LoginKind::ApiKey {
                kit::label(ui, "API key", theme::medium(14.0), theme::BODY);
                kit::field(
                    ui,
                    &mut self.omni_key_draft,
                    "Paste your key",
                    true,
                    "omni_api_key",
                );
                kit::para(
                    ui,
                    format!(
                        "The key goes straight to {}, which stores it. Litecord keeps no copy.",
                        kind.label()
                    ),
                    theme::regular(13.0),
                    theme::MUTED,
                );
            }
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let key_ok = o.kind != LoginKind::ApiKey || self.omni_key_draft.trim().len() >= 8;
                let label = if o.kind == LoginKind::ApiKey {
                    "Save key"
                } else {
                    "Continue in browser"
                };
                if kit::button_ex(
                    ui,
                    kit::Kind::Primary,
                    Some(ph::SIGN_IN),
                    label,
                    36.0,
                    key_ok && !self.busy,
                )
                .clicked()
                {
                    let inputs: LoginInputs = o
                        .prompts
                        .iter()
                        .filter(|p| {
                            p.when
                                .as_ref()
                                .is_none_or(|w| w.applies(&self.omni_login_inputs))
                        })
                        .filter_map(|p| {
                            let v = self.omni_login_inputs.get(&p.key)?.trim();
                            (!v.is_empty()).then(|| (p.key.clone(), v.to_owned()))
                        })
                        .collect();
                    let cmd = if o.kind == LoginKind::ApiKey {
                        let key = std::mem::take(&mut self.omni_key_draft);
                        OmniCommand::ApiKey(
                            o.id.clone(),
                            litecord_core::secrets::Secret::new(key.trim().to_owned()),
                            inputs,
                        )
                    } else {
                        OmniCommand::SignInWith(o.id.clone(), inputs)
                    };
                    self.send(Command::Omni(cmd));
                    self.omni_key_option = None;
                    self.omni_login_inputs.clear();
                }
                if kit::button_ex(ui, kit::Kind::Ghost, None, "Cancel", 36.0, true).clicked() {
                    self.omni_key_draft.clear();
                    self.omni_key_option = None;
                    self.omni_login_inputs.clear();
                }
            });
        });
    }
}

fn harness_look(kind: HarnessKind) -> (&'static str, kit::Tint) {
    match kind {
        HarnessKind::Codex => (ph::OPEN_AI_LOGO, kit::BLUE),
        HarnessKind::OpenCode => (ph::TERMINAL_WINDOW, kit::PURPLE),
        _ => (ph::SPARKLE, kit::TEAL),
    }
}

fn provider_glyph(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some(ph::OPEN_AI_LOGO),
        "github-copilot" | "github" => Some(ph::GITHUB_LOGO),
        "google" | "google-vertex" => Some(ph::GLOBE),
        "opencode" => Some(ph::TERMINAL_WINDOW),
        _ => None,
    }
}

/// Leading glyph, trailing action text and its colour for a method line.
fn method_look(o: &LoginOption) -> (&'static str, &'static str, Color32) {
    let label = o.label.to_lowercase();
    match o.kind {
        LoginKind::ApiKey => (ph::KEY, "Add key", theme::PRIMARY_TEXT),
        LoginKind::Browser if label.contains("device") || label.contains("headless") => {
            (ph::DEVICE_MOBILE, "Get code", theme::PRIMARY_TEXT)
        }
        LoginKind::Browser => (ph::GLOBE, "Sign in", theme::PRIMARY_TEXT),
    }
}
