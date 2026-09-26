use crate::{
    bridge::{Bridge, Command, Selection, Snapshot},
    layout_editor, theme,
};
use eframe::egui::{self, Align2, FontId, Rect, Sense, Stroke, Ui};
use litecord_app::{view::UiEffect, LitecordApp};
use litecord_features::intent::NavTarget;
use litecord_layout::{
    allocate_split, registry, Axis, Destination, LayoutNode, LayoutProfile, Orientation,
};
use litecord_types::ids::*;
use std::{collections::BTreeMap, sync::Arc};

pub struct Workspace {
    pub bridge: Bridge,
    pub snapshot: Option<Arc<Snapshot>>,
    pub selection: Selection,
    pub drafts: BTreeMap<ConversationId, String>,
    pub filter: String,
    pub conversation_tab: usize,
    pub friends_tab: usize,
    pub notice: Option<String>,
    pub busy: bool,
    pub editing_message: Option<(MessageId, String)>,
    pub deleting_message: Option<MessageId>,
    pub note_draft: Option<(UserId, String)>,
    pub profile: LayoutProfile,
    pub edit_original: Option<LayoutProfile>,
    pub layout_token: String,
    pub profile_name: String,
    pub profile_manager: bool,
    pub drag_source: Option<(bool, String)>,
    pub pending_dock: Option<(bool, String, String, litecord_layout::Placement)>,
    pub resize: Option<crate::layout_editor::QueuedResize>,
    pub saving_layout: bool,
    pub layout_dirty: bool,
    pub selected_guild: Option<GuildId>,
    pub selected_channel: Option<ChannelId>,
    pub selected_lobby: Option<LobbyId>,
    pub voice_tab: usize,
    pub palette_open: bool,
    pub palette_focus_requested: bool,
    pub palette_query: String,
    pub close_when_idle: bool,
    pub settings_section: Option<String>,
    pub lobby_text: String,
    pub selected_memory: Option<MemoryId>,
    pub selected_task: Option<TaskId>,
    /// Omni slide-over panel (available on every destination).
    pub omni_open: bool,
    pub omni_draft: String,
    /// Tasks screen: new-task form and comment drafts.
    pub task_title_draft: String,
    pub task_priority_draft: litecord_types::tasks::TaskPriority,
    pub task_comment_draft: String,
    /// Memory screen filters.
    pub memory_kind_filter: Option<litecord_types::memory::MemoryKind>,
    pub memory_status_filter: Option<litecord_types::memory::MemoryStatus>,
    pub memory_search: String,
    /// Inbox filter (0 = all).
    pub inbox_filter: usize,
    pub relationship_confirmation:
        Option<(UserId, litecord_types::actions::RelationshipAction, String)>,
    #[cfg(feature = "screenshots")]
    pub screenshot_path: Option<std::path::PathBuf>,
    #[cfg(feature = "screenshots")]
    screenshot_requested: bool,
    #[cfg(feature = "screenshots")]
    started: std::time::Instant,
    /// A finished normal-mode splitter drag waits to be persisted.
    pub layout_save_pending: bool,
    /// Edit Layout menu-based docking selection.
    pub dock_menu: crate::layout_editor::DockMenu,
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("selection", &self.selection)
            .finish_non_exhaustive()
    }
}

impl Workspace {
    pub fn new(app: LitecordApp, runtime: tokio::runtime::Handle, ctx: egui::Context) -> Self {
        let bridge = Bridge::new(app, runtime, ctx);
        let selection = bridge.selection.borrow().clone();
        Self {
            bridge,
            snapshot: None,
            selection,
            drafts: BTreeMap::new(),
            filter: String::new(),
            conversation_tab: 0,
            friends_tab: 0,
            notice: None,
            busy: false,
            editing_message: None,
            deleting_message: None,
            note_draft: None,
            profile: LayoutProfile::new("profile_default".into(), "Default".into()),
            edit_original: None,
            layout_token: String::new(),
            profile_name: String::new(),
            profile_manager: false,
            drag_source: None,
            pending_dock: None,
            resize: None,
            saving_layout: false,
            layout_dirty: false,
            selected_guild: None,
            selected_channel: None,
            selected_lobby: None,
            voice_tab: 0,
            palette_open: false,
            palette_focus_requested: false,
            palette_query: String::new(),
            close_when_idle: false,
            settings_section: None,
            lobby_text: String::new(),
            selected_memory: None,
            selected_task: None,
            omni_open: false,
            omni_draft: String::new(),
            task_title_draft: String::new(),
            task_priority_draft: litecord_types::tasks::TaskPriority::default(),
            task_comment_draft: String::new(),
            memory_kind_filter: None,
            memory_status_filter: None,
            memory_search: String::new(),
            inbox_filter: 0,
            relationship_confirmation: None,
            #[cfg(feature = "screenshots")]
            screenshot_path: None,
            #[cfg(feature = "screenshots")]
            screenshot_requested: false,
            #[cfg(feature = "screenshots")]
            started: std::time::Instant::now(),
            layout_save_pending: false,
            dock_menu: crate::layout_editor::DockMenu::default(),
        }
    }
    pub fn request(&mut self) {
        self.selection.generation += 1;
        self.bridge.selection.send_replace(self.selection.clone());
    }
    pub fn navigate(&mut self, d: Destination) {
        self.selection.destination = d;
        self.filter.clear();
        self.request();
    }
    /// Discard the Edit Layout draft. Shared by the header Cancel, Escape, the
    /// shortcut toggle and the profile dialog, so queued work never leaks out.
    pub fn cancel_edit(&mut self) {
        if let Some(original) = self.edit_original.take() {
            self.profile = original;
        }
        self.layout_dirty = false;
        self.layout_save_pending = false;
        self.drag_source = None;
        self.pending_dock = None;
        self.resize = None;
        self.dock_menu = crate::layout_editor::DockMenu::default();
    }
    pub fn apply_edit(&mut self) {
        if self.busy {
            return;
        }
        self.send(Command::SaveProfile(
            self.profile.clone(),
            self.layout_token.clone(),
        ));
        self.saving_layout = self.busy;
    }
    pub fn open_conversation(&mut self, id: ConversationId) {
        self.selection.destination = Destination::Messages;
        self.selection.conversation = Some(id);
        self.selection.contact = None;
        self.selection.before = None;
        self.note_draft = None;
        self.request();
    }
    pub fn send(&mut self, command: Command) {
        if self.busy || self.close_when_idle {
            return;
        }
        match self.bridge.commands.try_send(command) {
            Ok(()) => self.busy = true,
            Err(_) => self.notice = Some("Workspace is busy. Please try again.".into()),
        }
    }
    fn effects(&mut self, ctx: &egui::Context, effects: Vec<UiEffect>) {
        for effect in effects {
            match effect {
                UiEffect::Notice { message } => self.notice = Some(message),
                UiEffect::CopyToClipboard { text } => {
                    if self.private() {
                        self.notice = Some("Copy is hidden while Privacy Mode is enabled.".into());
                    } else {
                        ctx.copy_text(text);
                    }
                }
                UiEffect::OpenUrl { url } => ctx.open_url(egui::OpenUrl::new_tab(url)),
                UiEffect::Navigate { target } => match target {
                    NavTarget::Conversation { conversation_id } => {
                        self.open_conversation(conversation_id)
                    }
                    NavTarget::Friends => self.navigate(Destination::Friends),
                    NavTarget::Guild { guild_id } => {
                        self.selected_guild = Some(guild_id);
                        self.selected_channel = None;
                        self.navigate(Destination::Servers);
                    }
                    NavTarget::AgentInbox => self.navigate(Destination::Inbox),
                    NavTarget::Memory => self.navigate(Destination::Memory),
                    NavTarget::Tasks => self.navigate(Destination::Tasks),
                    NavTarget::Settings { section } => {
                        self.settings_section = section;
                        self.navigate(Destination::Settings)
                    }
                    NavTarget::Diagnostics => {
                        self.settings_section = None;
                        self.navigate(Destination::Settings)
                    }
                    NavTarget::Voice => self.navigate(Destination::Voice),
                },
            }
        }
    }
    pub fn private(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| {
            s.settings
                .sections
                .iter()
                .flat_map(|(_, r)| r)
                .any(|r| r.descriptor.key == "privacy.enabled" && r.value.as_bool() == Some(true))
        })
    }
    pub fn display(&self, text: &str) -> String {
        if self.private() {
            "Hidden in privacy mode".into()
        } else {
            text.to_owned()
        }
    }
    fn receive(&mut self, ctx: &egui::Context) {
        while let Ok(c) = self.bridge.completions.try_recv() {
            self.busy = false;
            if let Some(error) = c.error {
                self.notice = Some(error);
                self.saving_layout = false;
            } else {
                if c.relationship_changed.is_some_and(|id| {
                    self.relationship_confirmation
                        .as_ref()
                        .is_some_and(|(user, _, _)| *user == id)
                }) {
                    self.relationship_confirmation = None;
                }
                if let Some(id) = c.message_changed {
                    if self
                        .editing_message
                        .as_ref()
                        .is_some_and(|(editing, _)| *editing == id)
                    {
                        self.editing_message = None;
                    }
                    if self.deleting_message == Some(id) {
                        self.deleting_message = None;
                    }
                }
                if let Some(id) = c.task_created {
                    self.selected_task = Some(id);
                    self.selection.task = Some(id);
                    self.request();
                }
                if let Some(id) = c.omni_session {
                    self.selection.omni_session = Some(id);
                    self.omni_open = true;
                    self.request();
                }
                if let Some((id, text)) = c.sent {
                    if self.drafts.get(&id) == Some(&text) {
                        self.drafts.remove(&id);
                    }
                }
                if self.saving_layout {
                    self.edit_original = None;
                    self.saving_layout = false;
                    self.layout_dirty = false;
                }
                self.effects(ctx, c.effects);
            }
        }
        if self.bridge.snapshots.has_changed().unwrap_or(false) {
            let result = self.bridge.snapshots.borrow_and_update().clone();
            match result {
                Ok(s) if s.selection.generation == self.selection.generation => {
                    if self.selection.conversation.is_none() {
                        self.selection.conversation = s.selection.conversation;
                    }
                    if self.edit_original.is_none() && !self.saving_layout && !self.layout_dirty {
                        if let Some(p) = s
                            .layouts
                            .profiles
                            .profiles
                            .iter()
                            .find(|p| p.id == s.layouts.profiles.active_profile_id)
                        {
                            self.profile = p.clone();
                        }
                        self.layout_token = s.layouts.storage_token.clone();
                    }
                    self.snapshot = Some(Arc::new(s));
                }
                Ok(_) => {}
                Err(e) => self.notice = Some(e),
            }
        }
    }
    fn header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(
                egui::RichText::new("Litecord")
                    .size(15.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(8.0);
            if self.edit_original.is_some() {
                if ui
                    .add_enabled(
                        !self.busy,
                        egui::Button::new(egui::RichText::new("Apply layout").color(theme::SHELL))
                            .fill(theme::PRIMARY_TEXT),
                    )
                    .clicked()
                {
                    self.apply_edit();
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Cancel"))
                    .clicked()
                {
                    self.cancel_edit();
                }
            }
            // Layout controls and status get their space first (right side);
            // the search field adapts to what is left.
            let right_reserve = 430.0;
            let search_width = (ui.available_width() - right_reserve).clamp(140.0, 380.0);
            let (rect, search) =
                ui.allocate_exact_size(egui::vec2(search_width, 28.0), Sense::click());
            if ui.is_rect_visible(rect) {
                let painter = ui.painter();
                painter.rect(
                    rect,
                    6.0,
                    if search.hovered() {
                        theme::RAISED
                    } else {
                        theme::WORKSPACE
                    },
                    Stroke::new(1.0, theme::BORDER),
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    rect.left_center() + egui::vec2(10.0, 0.0),
                    Align2::LEFT_CENTER,
                    if search_width > 220.0 {
                        "Search or run a command…"
                    } else {
                        "Search…"
                    },
                    FontId::proportional(13.0),
                    theme::MUTED,
                );
                painter.text(
                    rect.right_center() - egui::vec2(10.0, 0.0),
                    Align2::RIGHT_CENTER,
                    "Ctrl+K",
                    FontId::proportional(11.0),
                    theme::MUTED,
                );
            }
            let search = search.on_hover_text("Command palette (Ctrl/Cmd+K)");
            search.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Search or run a command")
            });
            if search.clicked() {
                self.palette_open = true;
                self.palette_focus_requested = true;
            }
            if let Some(s) = self.snapshot.clone() {
                connection_status(ui, &s.diagnostics.session, "Discord");
                if let Some(bot) = &s.diagnostics.bot {
                    connection_status(ui, &bot.session, "Bot");
                }
                if s.diagnostics.backend_mode == litecord_types::capability::BackendMode::Demo {
                    theme::chip(ui, "Demo data", theme::WARNING);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (badge, running) = self.snapshot.as_ref().map_or((0, false), |s| {
                    (
                        s.omni.requests.len() + s.omni.checkins.len(),
                        s.omni.sessions.iter().any(|x| x.running),
                    )
                });
                let label = match (badge, running) {
                    (0, false) => "Omni".to_owned(),
                    (0, true) => "Omni · working".to_owned(),
                    (n, _) => format!("Omni · {n}"),
                };
                let omni = ui
                    .add(
                        egui::Button::new(egui::RichText::new(label).color(if self.omni_open {
                            theme::SHELL
                        } else {
                            theme::OMNI
                        }))
                        .fill(if self.omni_open {
                            theme::OMNI
                        } else {
                            theme::OMNI.gamma_multiply(0.12)
                        })
                        .stroke(Stroke::new(1.0, theme::OMNI.gamma_multiply(0.5))),
                    )
                    .on_hover_text("Ask Omni (Ctrl+J)");
                if omni.clicked() {
                    self.omni_open = !self.omni_open;
                }
                if ui.button("Layouts").clicked() {
                    self.profile_manager = !self.profile_manager;
                }
                ui.label(
                    egui::RichText::new(&self.profile.name)
                        .size(12.0)
                        .color(theme::MUTED),
                );
            });
        });
    }
    fn render_tree(&mut self, ui: &mut Ui, tree: &LayoutNode, rect: Rect, shell: bool) {
        match tree {
            LayoutNode::Panel {
                id,
                panel,
                placement,
                orientation,
                ..
            } => {
                if panel == "workspace" {
                    let source = self
                        .profile
                        .destinations
                        .get(&self.selection.destination)
                        .cloned();
                    if let Some(mut t) = source {
                        if rect.width() < 900.0 && self.edit_original.is_none() {
                            hide(&mut t, "context_inspector");
                        }
                        if rect.width() < 560.0 && self.edit_original.is_none() {
                            hide(&mut t, "conversation_list");
                            hide(&mut t, "contextual_sidebar");
                        }
                        if let Some(t) = t.project(self.selection.destination) {
                            self.render_tree(ui, &t, rect, false);
                        }
                    }
                    self.docking_preview(ui, shell, id, panel, rect);
                    return;
                }
                let fill = if matches!(panel.as_str(), "primary_navigation" | "user_controls") {
                    theme::SHELL
                } else if matches!(
                    panel.as_str(),
                    "conversation_list" | "contextual_sidebar" | "channel_list"
                ) {
                    theme::SIDEBAR
                } else {
                    theme::WORKSPACE
                };
                theme::surface(ui, rect, fill);
                ui.painter().line_segment(
                    [rect.right_top(), rect.right_bottom()],
                    Stroke::new(1.0_f32, theme::BORDER),
                );
                let mut content = rect.shrink2(egui::vec2(
                    if panel == "primary_navigation" {
                        4.0
                    } else {
                        16.0
                    },
                    12.0,
                ));
                if self.edit_original.is_some() {
                    let handle = Rect::from_min_size(rect.min, egui::vec2(rect.width(), 28.0));
                    let response =
                        ui.interact(handle, ui.id().with((shell, id, "handle")), Sense::drag());
                    ui.painter().rect_filled(handle, 0.0, theme::SELECTED);
                    ui.painter().text(
                        handle.center(),
                        Align2::CENTER_CENTER,
                        registry::descriptor(panel)
                            .map(|p| p.label)
                            .unwrap_or(panel),
                        FontId::proportional(12.0),
                        theme::TEXT,
                    );
                    if response.drag_started() && !self.layout_busy() {
                        self.drag_source = Some((shell, id.clone()));
                    }
                    if self
                        .drag_source
                        .as_ref()
                        .is_some_and(|(s, source)| *s == shell && source == id)
                    {
                        ui.painter().rect_stroke(
                            rect.shrink(1.0),
                            0.0,
                            Stroke::new(1.5, theme::PRIMARY),
                            egui::StrokeKind::Inside,
                        );
                    }
                    content.min.y += 28.0;
                }
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt((shell, id))
                        .max_rect(content)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                child.set_clip_rect(rect.intersect(ui.clip_rect()));
                self.panel(
                    &mut child,
                    panel,
                    registry::orientation(panel, *placement, *orientation)
                        .unwrap_or(Orientation::Vertical),
                );
                // Drawn after the panel body so the preview sits on top of it.
                self.docking_preview(ui, shell, id, panel, rect);
            }
            LayoutNode::Split { id, axis, children } => {
                let horizontal = *axis == Axis::Horizontal;
                let total = if horizontal {
                    rect.width()
                } else {
                    rect.height()
                };
                let gap = 4.0;
                let weights: Vec<_> = children.iter().map(|c| c.weight).collect();
                let minima: Vec<_> = children
                    .iter()
                    .map(|c| {
                        let min = c.node.minimum_size();
                        if horizontal {
                            min.width
                        } else {
                            min.height
                        }
                    })
                    .collect();
                let Ok(a) = allocate_split(
                    (total - gap * (children.len() - 1) as f32).max(0.0),
                    &weights,
                    &minima,
                ) else {
                    return;
                };
                let mut offset = 0.0;
                for (i, c) in children.iter().enumerate() {
                    let r = if horizontal {
                        Rect::from_min_size(
                            rect.min + egui::vec2(offset, 0.0),
                            egui::vec2(a.sizes[i], rect.height()),
                        )
                    } else {
                        Rect::from_min_size(
                            rect.min + egui::vec2(0.0, offset),
                            egui::vec2(rect.width(), a.sizes[i]),
                        )
                    };
                    self.render_tree(ui, &c.node, r, shell);
                    offset += a.sizes[i];
                    if i + 1 < children.len() {
                        let splitter = if horizontal {
                            Rect::from_min_size(
                                rect.min + egui::vec2(offset, 0.0),
                                egui::vec2(gap, rect.height()),
                            )
                        } else {
                            Rect::from_min_size(
                                rect.min + egui::vec2(0.0, offset),
                                egui::vec2(rect.width(), gap),
                            )
                        };
                        let response =
                            ui.interact(splitter, ui.id().with((shell, id, i)), Sense::drag());
                        if response.hovered() || response.dragged() {
                            ui.ctx().set_cursor_icon(if horizontal {
                                egui::CursorIcon::ResizeHorizontal
                            } else {
                                egui::CursorIcon::ResizeVertical
                            });
                            ui.painter().rect_filled(splitter, 0.0, theme::PRIMARY);
                        }
                        // No new geometry mutation while a save is in flight.
                        if response.dragged() && !a.below_minimum && !self.layout_busy() {
                            let delta = ui.input(|inp| {
                                if horizontal {
                                    inp.pointer.delta().x
                                } else {
                                    inp.pointer.delta().y
                                }
                            });
                            let sizes = layout_editor::drag_splitter(&a.sizes, &minima, i, delta);
                            if delta != 0.0 && sizes.iter().all(|s| *s > 0.0) {
                                // Sizes are measured on the projected tree; they are
                                // keyed by projected child ID and mapped back onto the
                                // saved tree by `resize_projected`.
                                self.resize = Some(layout_editor::QueuedResize {
                                    shell,
                                    split_id: id.clone(),
                                    sizes: layout_editor::projected_sizes(children, &sizes),
                                });
                            }
                        }
                        if response.drag_stopped() && self.edit_original.is_none() {
                            // Persisted after the queued resize is applied in `draw`.
                            self.layout_save_pending = true;
                        }
                        offset += gap;
                    }
                }
            }
        }
    }
}

impl eframe::App for Workspace {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

impl Workspace {
    pub(crate) fn draw(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        self.receive(&ctx);
        self.command_shortcuts(&ctx);
        if ctx.input_mut(|i| {
            i.consume_key(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::L,
            )
        }) && !self.busy
        {
            if self.edit_original.is_some() {
                self.cancel_edit();
            } else {
                self.edit_original = Some(self.profile.clone());
                self.profile_manager = false;
            }
        }
        if self.edit_original.is_some()
            && !self.busy
            && !self.palette_open
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.cancel_edit();
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.busy {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_when_idle = true;
            self.notice = Some("Finishing the current action before closing…".into());
        }
        if self.close_when_idle && !self.busy {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        egui::Panel::top("global_header")
            .exact_size(44.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::SHELL)
                    .inner_margin(egui::Margin::symmetric(16, 6)),
            )
            .show_inside(root, |ui| self.header(ui));
        if let Some(notice) = self.notice.clone() {
            egui::Panel::bottom("notice").show_inside(root, |ui| {
                ui.horizontal(|ui| {
                    ui.label(notice);
                    if ui.small_button("Dismiss").clicked() {
                        self.notice = None;
                    }
                });
            });
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::J)) {
            self.omni_open = !self.omni_open;
        }
        if self.omni_open {
            egui::Panel::right("omni_panel")
                .exact_size(400.0)
                .frame(
                    egui::Frame::new()
                        .fill(theme::SIDEBAR)
                        .stroke(Stroke::new(1.0, theme::BORDER))
                        .inner_margin(egui::Margin::symmetric(14, 10)),
                )
                .show_inside(root, |ui| self.omni_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::SHELL))
            .show_inside(root, |ui| {
                let tree = self.profile.shell.project(self.selection.destination);
                if let Some(t) = tree {
                    self.render_tree(ui, &t, ui.max_rect(), true);
                }
            });
        self.edit_toolbar(&ctx);
        self.apply_queued_layout_changes(&ctx);
        self.drag_ghost(&ctx);
        if ctx.input(|i| i.pointer.any_released()) {
            self.drag_source = None;
        }
        self.profiles_window(&ctx);
        self.message_dialogs(&ctx);
        self.relationship_dialog(&ctx);
        self.palette_window(&ctx);
        #[cfg(feature = "screenshots")]
        if let Some(path) = &self.screenshot_path {
            if !self.screenshot_requested
                && self.snapshot.is_some()
                && self.started.elapsed() > std::time::Duration::from_secs(3)
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                self.screenshot_requested = true;
            }
            let screenshot = ctx.input(|i| {
                i.events.iter().find_map(|e| {
                    if let egui::Event::Screenshot { image, .. } = e {
                        Some(image.clone())
                    } else {
                        None
                    }
                })
            });
            if let Some(image) = screenshot {
                let pixels: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                match image::save_buffer(
                    path,
                    &pixels,
                    image.size[0] as u32,
                    image.size[1] as u32,
                    image::ColorType::Rgba8,
                ) {
                    Ok(()) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                    Err(e) => {
                        self.notice = Some(format!("Screenshot failed: {e}"));
                        self.screenshot_path = None;
                    }
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// Colored dot + text for a connection state; hover explains errors.
fn connection_status(ui: &mut Ui, state: &litecord_types::social::SessionState, who: &str) {
    use litecord_types::social::SessionState as S;
    let (color, text) = match state {
        S::Ready => (theme::SUCCESS, "Connected"),
        S::Hydrating => (theme::WARNING, "Syncing…"),
        S::Connecting | S::Authorizing => (theme::WARNING, "Connecting…"),
        S::Reconnecting => (theme::WARNING, "Reconnecting…"),
        S::Offline => (theme::PRIORITY, "Offline"),
        S::LoggedOut => (theme::MUTED, "Signed out"),
        S::Error { .. } => (theme::PRIORITY, "Error"),
    };
    let r = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, color);
            ui.label(
                egui::RichText::new(format!("{who} · {text}"))
                    .size(12.0)
                    .color(theme::SECONDARY),
            );
        })
        .response;
    if let S::Error { message } = state {
        r.on_hover_text(message.as_str());
    }
}

fn hide(t: &mut LayoutNode, p: &str) {
    match t {
        LayoutNode::Panel { panel, visible, .. } if panel == p => *visible = false,
        LayoutNode::Split { children, .. } => {
            for c in children {
                hide(&mut c.node, p)
            }
        }
        _ => {}
    }
}
