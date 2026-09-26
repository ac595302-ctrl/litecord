use crate::{
    bridge::{Bridge, Command, Selection, Snapshot},
    theme,
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
    pub resize: Option<(bool, String, Vec<f32>)>,
    pub saving_layout: bool,
    pub layout_dirty: bool,
    pub selected_guild: Option<GuildId>,
    pub selected_channel: Option<ChannelId>,
    pub selected_lobby: Option<LobbyId>,
    pub voice_tab: usize,
    pub palette_open: bool,
    pub palette_query: String,
    pub close_when_idle: bool,
    pub settings_section: Option<String>,
    pub lobby_text: String,
    pub selected_memory: Option<MemoryId>,
    pub selected_task: Option<TaskId>,
    #[cfg(feature = "screenshots")]
    pub screenshot_path: Option<std::path::PathBuf>,
    #[cfg(feature = "screenshots")]
    screenshot_requested: bool,
    #[cfg(feature = "screenshots")]
    started: std::time::Instant,
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
            palette_query: String::new(),
            close_when_idle: false,
            settings_section: None,
            lobby_text: String::new(),
            selected_memory: None,
            selected_task: None,
            #[cfg(feature = "screenshots")]
            screenshot_path: None,
            #[cfg(feature = "screenshots")]
            screenshot_requested: false,
            #[cfg(feature = "screenshots")]
            started: std::time::Instant::now(),
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
    pub fn cancel_edit(&mut self) {
        if let Some(original) = self.edit_original.take() {
            self.profile = original;
        }
        self.layout_dirty = false;
        self.drag_source = None;
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
                    NavTarget::Guild { .. } => self.navigate(Destination::Servers),
                    NavTarget::AgentInbox => self.navigate(Destination::Inbox),
                    NavTarget::Memory => self.navigate(Destination::Memory),
                    NavTarget::Tasks => self.navigate(Destination::Tasks),
                    NavTarget::Settings { .. } | NavTarget::Diagnostics => {
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
            ui.label(egui::RichText::new("LITECORD").strong().color(theme::TEXT));
            ui.add_space(16.0);
            if self.edit_original.is_some() {
                if ui
                    .add_enabled(
                        !self.busy,
                        egui::Button::new("Apply layout").fill(theme::PRIMARY),
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
            if ui.button("Search & commands   Ctrl+K").clicked() {
                self.palette_open = true;
            }
            if let Some(s) = &self.snapshot {
                theme::chip(
                    ui,
                    match s.diagnostics.backend_mode {
                        litecord_types::capability::BackendMode::Demo => "Demo · synthetic",
                        _ => "Discord",
                    },
                    theme::MUTED,
                );
                ui.label(
                    egui::RichText::new(format!("{:?}", s.diagnostics.session))
                        .size(12.0)
                        .color(theme::MUTED),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                    if self.edit_original.is_some() {
                        if let Some((source_shell, source)) = &self.drag_source {
                            if *source_shell && source != id {
                                if let Some(pointer) =
                                    ui.ctx().pointer_hover_pos().filter(|p| rect.contains(*p))
                                {
                                    let placement = edge(rect, pointer);
                                    ui.painter().rect_filled(
                                        drop_rect(rect, placement),
                                        4.0,
                                        theme::PRIMARY.gamma_multiply(0.25),
                                    );
                                    if ui.input(|i| i.pointer.any_released()) {
                                        self.pending_dock =
                                            Some((true, source.clone(), id.clone(), placement));
                                    }
                                }
                            }
                        }
                    }
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
                ui.painter().rect_filled(rect, 0.0, fill);
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
                    if response.drag_started() {
                        self.drag_source = Some((shell, id.clone()));
                    }
                    content.min.y += 28.0;
                    if let Some((source_shell, source)) = &self.drag_source {
                        if *source_shell == shell && source != id {
                            if let Some(pointer) =
                                ui.ctx().pointer_hover_pos().filter(|p| rect.contains(*p))
                            {
                                let placement = edge(rect, pointer);
                                let target = drop_rect(rect, placement);
                                ui.painter().rect_filled(
                                    target,
                                    4.0,
                                    theme::PRIMARY.gamma_multiply(0.25),
                                );
                                if ui.input(|i| i.pointer.any_released()) {
                                    self.pending_dock =
                                        Some((shell, source.clone(), id.clone(), placement));
                                }
                            }
                        }
                    }
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
                        if response.dragged() && !a.below_minimum {
                            let delta = ui.input(|inp| {
                                if horizontal {
                                    inp.pointer.delta().x
                                } else {
                                    inp.pointer.delta().y
                                }
                            });
                            let mut sizes = a.sizes.clone();
                            let delta =
                                delta.clamp(minima[i] - sizes[i], sizes[i + 1] - minima[i + 1]);
                            sizes[i] += delta;
                            sizes[i + 1] -= delta;
                            self.resize = Some((shell, id.clone(), sizes));
                            self.layout_dirty = true;
                        }
                        if response.drag_stopped() && self.edit_original.is_none() {
                            self.apply_edit();
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
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::SHELL))
            .show_inside(root, |ui| {
                let tree = self.profile.shell.project(self.selection.destination);
                if let Some(t) = tree {
                    self.render_tree(ui, &t, ui.max_rect(), true);
                }
            });
        if let Some((shell, id, weights)) = self.resize.take() {
            let tree = if shell {
                Some(&mut self.profile.shell)
            } else {
                self.profile
                    .destinations
                    .get_mut(&self.selection.destination)
            };
            if let Some(t) = tree {
                if let Err(e) = t.resize(&id, &weights) {
                    self.notice = Some(e.to_string());
                }
            }
        }
        if let Some((shell, source, target, placement)) = self.pending_dock.take() {
            let tree = if shell {
                Some(&mut self.profile.shell)
            } else {
                self.profile
                    .destinations
                    .get_mut(&self.selection.destination)
            };
            if let Some(t) = tree {
                if let Err(e) = t.dock(&source, &target, placement) {
                    self.notice = Some(e.to_string());
                }
            }
        }
        if ctx.input(|i| i.pointer.any_released()) {
            self.drag_source = None;
        }
        self.profiles_window(&ctx);
        self.message_dialogs(&ctx);
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
fn edge(rect: Rect, p: egui::Pos2) -> litecord_layout::Placement {
    let distances = [
        (p.x - rect.left(), litecord_layout::Placement::Left),
        (rect.right() - p.x, litecord_layout::Placement::Right),
        (p.y - rect.top(), litecord_layout::Placement::Top),
        (rect.bottom() - p.y, litecord_layout::Placement::Bottom),
    ];
    distances
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|x| x.1)
        .unwrap_or(litecord_layout::Placement::Left)
}
fn drop_rect(r: Rect, p: litecord_layout::Placement) -> Rect {
    use litecord_layout::Placement::*;
    match p {
        Left => Rect::from_min_max(r.min, egui::pos2(r.center().x, r.bottom())),
        Right => Rect::from_min_max(egui::pos2(r.center().x, r.top()), r.max),
        Top => Rect::from_min_max(r.min, egui::pos2(r.right(), r.center().y)),
        _ => Rect::from_min_max(egui::pos2(r.left(), r.center().y), r.max),
    }
}
