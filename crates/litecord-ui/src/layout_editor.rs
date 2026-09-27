//! Edit Layout interactions: projected splitter resizing, validated docking
//! previews and the menu-based docking alternative to dragging.
//!
//! The renderer draws *projected* trees (hidden/unavailable panels omitted and
//! single-child groups collapsed), so every mutation here is expressed in terms
//! of stable node IDs and applied to the saved tree through the pure
//! `litecord-layout` operations. Previews simulate the mutation on a clone and
//! only offer targets whose result validates.

use crate::{theme, workspace::Workspace};
use eframe::egui::{self, Align2, FontId, Rect, Stroke, Ui};
use litecord_layout::{
    registry, Destination, LayoutError, LayoutNode, LayoutProfile, LayoutResult, Placement,
    WeightedNode,
};

/// A splitter drag measured on the projected tree, applied after rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedResize {
    pub shell: bool,
    pub split_id: String,
    /// `(projected child ID, size)` for every child the renderer drew.
    pub sizes: Vec<(String, f32)>,
}

/// Selection state for the menu-based docking controls.
#[derive(Debug, Clone, PartialEq)]
pub struct DockMenu {
    /// `(shell tree, node ID)` of the panel to move.
    pub source: Option<(bool, String)>,
    pub target: Option<String>,
    pub placement: Placement,
}

impl Default for DockMenu {
    fn default() -> Self {
        Self {
            source: None,
            target: None,
            placement: Placement::Left,
        }
    }
}

pub const EDGES: [Placement; 4] = [
    Placement::Left,
    Placement::Right,
    Placement::Top,
    Placement::Bottom,
];

/// Pair projected children with the sizes the renderer measured for them.
pub fn projected_sizes(children: &[WeightedNode], sizes: &[f32]) -> Vec<(String, f32)> {
    children
        .iter()
        .zip(sizes)
        .map(|(c, s)| (c.node.id().to_owned(), *s))
        .collect()
}

/// Move the splitter after child `index` by `delta`, keeping both neighbours at
/// or above their minima. Out-of-range indices return the sizes unchanged.
pub fn drag_splitter(sizes: &[f32], minima: &[f32], index: usize, delta: f32) -> Vec<f32> {
    let mut out = sizes.to_vec();
    if index + 1 >= sizes.len() || index + 1 >= minima.len() || !delta.is_finite() {
        return out;
    }
    let low = (minima[index] - sizes[index]).min(0.0);
    let high = (sizes[index + 1] - minima[index + 1]).max(0.0);
    let delta = delta.clamp(low, high);
    out[index] += delta;
    out[index + 1] -= delta;
    out
}

fn tree_mut(
    profile: &mut LayoutProfile,
    destination: Destination,
    shell: bool,
) -> Option<&mut LayoutNode> {
    if shell {
        Some(&mut profile.shell)
    } else {
        profile.destinations.get_mut(&destination)
    }
}

fn tree(profile: &LayoutProfile, destination: Destination, shell: bool) -> Option<&LayoutNode> {
    if shell {
        Some(&profile.shell)
    } else {
        profile.destinations.get(&destination)
    }
}

/// Apply a projected resize to the saved tree. On error `profile` is unchanged.
pub fn apply_resize(
    profile: &mut LayoutProfile,
    destination: Destination,
    resize: &QueuedResize,
) -> LayoutResult<()> {
    tree_mut(profile, destination, resize.shell)
        .ok_or_else(|| LayoutError("layout tree not found".into()))?
        .resize_projected(&resize.split_id, &resize.sizes)
}

/// Simulate a dock on a clone of the profile and validate the whole result.
pub fn simulate_dock(
    profile: &LayoutProfile,
    destination: Destination,
    shell: bool,
    source: &str,
    target: &str,
    placement: Placement,
) -> LayoutResult<LayoutProfile> {
    let mut draft = profile.clone();
    tree_mut(&mut draft, destination, shell)
        .ok_or_else(|| LayoutError("layout tree not found".into()))?
        .dock(source, target, placement)?;
    draft.validate()?;
    Ok(draft)
}

pub fn panel_label(panel: &str) -> &str {
    registry::descriptor(panel).map_or(panel, |d| d.label)
}

/// Panel ID of a node in the saved tree, for labels.
pub fn node_panel(
    profile: &LayoutProfile,
    destination: Destination,
    shell: bool,
    id: &str,
) -> Option<String> {
    match tree(profile, destination, shell)?.find(id)? {
        LayoutNode::Panel { panel, .. } => Some(panel.clone()),
        LayoutNode::Split { .. } => None,
    }
}

pub fn edge_name(placement: Placement) -> &'static str {
    match placement {
        Placement::Left => "Left",
        Placement::Right => "Right",
        Placement::Top => "Above",
        Placement::Bottom => "Below",
        Placement::Center => "Center",
    }
}

/// "Left of Chat", "Below Contact inspector", ...
pub fn dock_label(placement: Placement, target_panel: &str) -> String {
    let target = panel_label(target_panel);
    match placement {
        Placement::Left | Placement::Right => format!("{} of {target}", edge_name(placement)),
        Placement::Top | Placement::Bottom => format!("{} {target}", edge_name(placement)),
        Placement::Center => format!("Into {target}"),
    }
}

/// Panels the user can currently see, as `(shell tree, node ID, panel ID)`.
pub fn dock_candidates(
    profile: &LayoutProfile,
    destination: Destination,
) -> Vec<(bool, String, String)> {
    fn leaves(node: &LayoutNode, shell: bool, out: &mut Vec<(bool, String, String)>) {
        match node {
            LayoutNode::Panel { id, panel, .. } => out.push((shell, id.clone(), panel.clone())),
            LayoutNode::Split { children, .. } => {
                for c in children {
                    leaves(&c.node, shell, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    if let Some(t) = profile.shell.project(destination) {
        leaves(&t, true, &mut out);
    }
    if let Some(t) = profile
        .destinations
        .get(&destination)
        .and_then(|t| t.project(destination))
    {
        leaves(&t, false, &mut out);
    }
    out
}

pub fn edge(rect: Rect, p: egui::Pos2) -> Placement {
    let distances = [
        (p.x - rect.left(), Placement::Left),
        (rect.right() - p.x, Placement::Right),
        (p.y - rect.top(), Placement::Top),
        (rect.bottom() - p.y, Placement::Bottom),
    ];
    distances
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map_or(Placement::Left, |x| x.1)
}

pub fn drop_rect(r: Rect, p: Placement) -> Rect {
    match p {
        Placement::Left => Rect::from_min_max(r.min, egui::pos2(r.center().x, r.bottom())),
        Placement::Right => Rect::from_min_max(egui::pos2(r.center().x, r.top()), r.max),
        Placement::Top => Rect::from_min_max(r.min, egui::pos2(r.right(), r.center().y)),
        _ => Rect::from_min_max(egui::pos2(r.left(), r.center().y), r.max),
    }
}

impl Workspace {
    /// A save (or any other command) is in flight; layout mutations must wait.
    pub(crate) fn layout_busy(&self) -> bool {
        self.busy || self.saving_layout
    }

    /// Highlight a validated drop zone on `target` while a panel is dragged, and
    /// queue the dock on release.
    pub(crate) fn docking_preview(
        &mut self,
        ui: &Ui,
        shell: bool,
        target: &str,
        target_panel: &str,
        rect: Rect,
    ) {
        if self.edit_original.is_none() || self.layout_busy() {
            return;
        }
        let Some((source_shell, source)) = self.drag_source.clone() else {
            return;
        };
        if source_shell != shell || source == target {
            return;
        }
        let Some(pointer) = ui.ctx().pointer_hover_pos().filter(|p| rect.contains(*p)) else {
            return;
        };
        let placement = edge(rect, pointer);
        let destination = self.selection.destination;
        if simulate_dock(
            &self.profile,
            destination,
            shell,
            &source,
            target,
            placement,
        )
        .is_err()
        {
            return;
        }
        let zone = drop_rect(rect, placement);
        let painter = ui.painter();
        painter.rect_filled(zone, 4.0, theme::PRIMARY.gamma_multiply(0.25));
        painter.rect_stroke(
            zone.shrink(1.0),
            4.0,
            Stroke::new(1.5_f32, theme::PRIMARY),
            egui::StrokeKind::Inside,
        );
        painter.text(
            zone.center(),
            Align2::CENTER_CENTER,
            dock_label(placement, target_panel),
            FontId::proportional(13.0),
            theme::TEXT,
        );
        if ui.input(|i| i.pointer.any_released()) {
            self.pending_dock = Some((shell, source, target.to_owned(), placement));
        }
    }

    /// Floating label of the dragged panel that follows the pointer.
    pub(crate) fn drag_ghost(&self, ctx: &egui::Context) {
        if self.edit_original.is_none() {
            return;
        }
        let (Some((shell, source)), Some(pointer)) =
            (&self.drag_source, ctx.pointer_interact_pos())
        else {
            return;
        };
        let panel = node_panel(&self.profile, self.selection.destination, *shell, source)
            .unwrap_or_else(|| source.clone());
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("layout_drag_ghost"),
        ));
        let galley = painter.layout_no_wrap(
            panel_label(&panel).to_owned(),
            FontId::proportional(13.0),
            theme::TEXT,
        );
        let rect = Rect::from_min_size(pointer + egui::vec2(14.0, 10.0), galley.size())
            .expand2(egui::vec2(10.0, 6.0));
        painter.rect_filled(rect, 6.0, theme::RAISED.gamma_multiply(0.92));
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0_f32, theme::PRIMARY),
            egui::StrokeKind::Inside,
        );
        painter.galley(rect.min + egui::vec2(10.0, 6.0), galley, theme::TEXT);
        ctx.request_repaint();
    }

    /// Floating Edit Layout toolbar. It overlays the bottom of the window rather
    /// than taking layout space, so entering Edit Layout never shifts panels.
    pub(crate) fn edit_toolbar(&mut self, ctx: &egui::Context) {
        if self.edit_original.is_none() {
            return;
        }
        egui::Area::new(egui::Id::new("layout_edit_toolbar"))
            .order(egui::Order::Foreground)
            .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -16.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::RAISED)
                    .stroke(Stroke::new(1.0_f32, theme::BORDER))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Editing layout")
                                    .strong()
                                    .color(theme::TEXT),
                            )
                            .on_hover_text(
                                "Drag a panel header onto an edge of another panel, \
                                 or choose a move here. Apply or Cancel in the header.",
                            );
                            ui.separator();
                            self.docking_controls(ui);
                        });
                    });
            });
    }

    /// Menu-based docking: choose panel, edge and target, then move.
    pub(crate) fn docking_controls(&mut self, ui: &mut Ui) {
        let destination = self.selection.destination;
        let candidates = dock_candidates(&self.profile, destination);
        let menu = &mut self.dock_menu;
        if menu
            .source
            .as_ref()
            .is_some_and(|(s, id)| !candidates.iter().any(|(cs, cid, _)| cs == s && cid == id))
        {
            menu.source = None;
        }
        let source_shell = menu.source.as_ref().map(|(s, _)| *s);
        if menu.target.as_ref().is_some_and(|t| {
            !candidates.iter().any(|(cs, cid, _)| {
                Some(*cs) == source_shell
                    && cid == t
                    && menu.source.as_ref().map(|(_, s)| s) != Some(cid)
            })
        }) {
            menu.target = None;
        }
        let label_of = |shell: bool, id: &str| {
            candidates
                .iter()
                .find(|(s, cid, _)| *s == shell && cid == id)
                .map_or(id.to_owned(), |(_, _, p)| panel_label(p).to_owned())
        };
        ui.label("Move");
        egui::ComboBox::from_id_salt("dock_menu_source")
            .selected_text(
                menu.source
                    .as_ref()
                    .map_or("Panel…".to_owned(), |(s, id)| label_of(*s, id)),
            )
            .show_ui(ui, |ui| {
                for (shell, id, panel) in &candidates {
                    ui.selectable_value(
                        &mut menu.source,
                        Some((*shell, id.clone())),
                        panel_label(panel),
                    );
                }
            });
        egui::ComboBox::from_id_salt("dock_menu_edge")
            .selected_text(edge_name(menu.placement))
            .width(72.0)
            .show_ui(ui, |ui| {
                for edge in EDGES {
                    ui.selectable_value(&mut menu.placement, edge, edge_name(edge));
                }
            });
        let source = menu.source.clone();
        egui::ComboBox::from_id_salt("dock_menu_target")
            .selected_text(match (&menu.target, &source) {
                (Some(t), Some((s, _))) => label_of(*s, t),
                _ => "Target…".to_owned(),
            })
            .show_ui(ui, |ui| {
                for (shell, id, panel) in &candidates {
                    if source
                        .as_ref()
                        .is_some_and(|(s, sid)| s == shell && sid != id)
                    {
                        ui.selectable_value(&mut menu.target, Some(id.clone()), panel_label(panel));
                    }
                }
            });
        let choice = match (&menu.source, &menu.target) {
            (Some((shell, source)), Some(target)) => {
                Some((*shell, source.clone(), target.clone(), menu.placement))
            }
            _ => None,
        };
        let valid = choice.as_ref().is_some_and(|(shell, s, t, p)| {
            simulate_dock(&self.profile, destination, *shell, s, t, *p).is_ok()
        });
        let busy = self.layout_busy();
        if ui
            .add_enabled(
                valid && !busy,
                egui::Button::new("Move").fill(theme::PRIMARY),
            )
            .clicked()
        {
            self.pending_dock = choice.clone();
            self.dock_menu.target = None;
        }
        if choice.is_some() && !valid {
            ui.label(
                egui::RichText::new("Not allowed there")
                    .size(12.0)
                    .color(theme::PRIORITY),
            );
        }
    }

    /// Apply queued resize/dock mutations after rendering, and persist a finished
    /// normal-mode splitter drag. Edit-mode changes stay in the draft until Apply.
    pub(crate) fn apply_queued_layout_changes(&mut self, ctx: &egui::Context) {
        let destination = self.selection.destination;
        if let Some(resize) = self.resize.take() {
            if !self.layout_busy() {
                match apply_resize(&mut self.profile, destination, &resize) {
                    Ok(()) => self.layout_dirty = true,
                    Err(e) => self.notice = Some(e.to_string()),
                }
            }
        }
        if let Some((shell, source, target, placement)) = self.pending_dock.take() {
            if self.edit_original.is_some() && !self.layout_busy() {
                match simulate_dock(
                    &self.profile,
                    destination,
                    shell,
                    &source,
                    &target,
                    placement,
                ) {
                    Ok(next) => {
                        self.profile = next;
                        self.layout_dirty = true;
                    }
                    Err(e) => self.notice = Some(e.to_string()),
                }
            }
        }
        if self.layout_save_pending {
            if self.edit_original.is_some() || !self.layout_dirty {
                self.layout_save_pending = false;
            } else if !self.layout_busy() && !self.close_when_idle {
                self.layout_save_pending = false;
                self.apply_edit();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use litecord_layout::Axis;

    fn weights(node: &LayoutNode) -> Vec<f32> {
        match node {
            LayoutNode::Split { children, .. } => children.iter().map(|c| c.weight).collect(),
            LayoutNode::Panel { .. } => vec![],
        }
    }

    #[test]
    fn projected_sizes_pair_ids_in_order() {
        let tree = LayoutNode::split(
            "root",
            Axis::Horizontal,
            vec![
                (1.0, LayoutNode::panel("a", "chat", Placement::Center)),
                (
                    1.0,
                    LayoutNode::panel("b", "context_inspector", Placement::Right),
                ),
            ],
        );
        let LayoutNode::Split { children, .. } = &tree else {
            unreachable!()
        };
        assert_eq!(
            projected_sizes(children, &[300.0, 100.0]),
            vec![("a".to_owned(), 300.0), ("b".to_owned(), 100.0)]
        );
    }

    #[test]
    fn splitter_drag_respects_minima() {
        assert_eq!(
            drag_splitter(&[300.0, 300.0], &[200.0, 200.0], 0, 50.0),
            vec![350.0, 250.0]
        );
        assert_eq!(
            drag_splitter(&[300.0, 300.0], &[200.0, 200.0], 0, 500.0),
            vec![400.0, 200.0]
        );
        assert_eq!(
            drag_splitter(&[300.0, 300.0], &[200.0, 200.0], 0, -500.0),
            vec![200.0, 400.0]
        );
        // Out-of-range index or non-finite delta is a no-op, never a panic.
        assert_eq!(drag_splitter(&[1.0], &[0.0], 0, 5.0), vec![1.0]);
        assert_eq!(
            drag_splitter(&[300.0, 300.0], &[200.0, 200.0], 0, f32::NAN),
            vec![300.0, 300.0]
        );
    }

    #[test]
    fn projected_resize_keeps_hidden_inspector_weight() {
        let mut profile = LayoutProfile::new("p".into(), "P".into());
        let d = Destination::Messages;
        profile
            .destinations
            .get_mut(&d)
            .unwrap()
            .set_visible("inspector", false)
            .unwrap();
        let projected = profile.destinations[&d].project(d).unwrap();
        let LayoutNode::Split { children, .. } = &projected else {
            unreachable!()
        };
        assert_eq!(children.len(), 2);
        let resize = QueuedResize {
            shell: false,
            split_id: "destination_root".into(),
            sizes: projected_sizes(children, &[400.0, 800.0]),
        };
        // The old saved-tree API rejects projected sizes outright.
        assert!(profile
            .destinations
            .get_mut(&d)
            .unwrap()
            .resize("destination_root", &[400.0, 800.0])
            .is_err());
        apply_resize(&mut profile, d, &resize).unwrap();
        let w = weights(&profile.destinations[&d]);
        assert_eq!(w[2], 350.0);
        assert!((w[0] / w[1] - 0.5).abs() < 1e-4);
        assert!((w[0] + w[1] - (296.0 + 840.0)).abs() < 1e-2);
    }

    #[test]
    fn projected_resize_maps_collapsed_group_to_saved_child() {
        let mut profile = LayoutProfile::new("p".into(), "P".into());
        let d = Destination::Home;
        let tree = profile.destinations.get_mut(&d).unwrap();
        tree.dock("inspector", "context", Placement::Bottom)
            .unwrap();
        tree.set_visible("inspector", false).unwrap();
        let projected = tree.project(d).unwrap();
        let LayoutNode::Split { id, children, .. } = &projected else {
            unreachable!()
        };
        // The collapsed group projects as the bare "context" leaf.
        assert_eq!(children[0].node.id(), "context");
        let resize = QueuedResize {
            shell: false,
            split_id: id.clone(),
            sizes: projected_sizes(children, &[300.0, 900.0]),
        };
        apply_resize(&mut profile, d, &resize).unwrap();
        profile.validate().unwrap();
        let bad = QueuedResize {
            sizes: vec![("nope".into(), 1.0)],
            ..resize
        };
        let before = profile.clone();
        assert!(apply_resize(&mut profile, d, &bad).is_err());
        assert_eq!(profile, before);
    }

    #[test]
    fn docking_preview_only_accepts_valid_moves() {
        let profile = LayoutProfile::new("p".into(), "P".into());
        let d = Destination::Messages;
        let moved = simulate_dock(
            &profile,
            d,
            false,
            "inspector",
            "context",
            Placement::Bottom,
        )
        .unwrap();
        assert!(moved.destinations[&d].find("inspector").is_some());
        // The simulation never touches the original.
        assert_eq!(profile, LayoutProfile::new("p".into(), "P".into()));
        assert!(simulate_dock(&profile, d, false, "main", "main", Placement::Left).is_err());
        assert!(simulate_dock(&profile, d, false, "main", "context", Placement::Center).is_err());
        assert!(simulate_dock(&profile, d, false, "missing", "main", Placement::Left).is_err());
        // Shell moves validate the whole profile too.
        assert!(simulate_dock(&profile, d, true, "account", "outlet", Placement::Bottom).is_ok());
    }

    #[test]
    fn dock_labels_use_registry_names() {
        assert_eq!(dock_label(Placement::Left, "chat"), "Left of Chat");
        assert_eq!(
            dock_label(Placement::Bottom, "context_inspector"),
            "Below Context inspector"
        );
        assert_eq!(dock_label(Placement::Top, "unknown_x"), "Above unknown_x");
    }

    #[test]
    fn candidates_list_visible_panels_only() {
        let mut profile = LayoutProfile::new("p".into(), "P".into());
        let d = Destination::Messages;
        profile
            .destinations
            .get_mut(&d)
            .unwrap()
            .set_visible("inspector", false)
            .unwrap();
        let c = dock_candidates(&profile, d);
        assert!(c.iter().any(|(s, id, _)| *s && id == "outlet"));
        assert!(c.iter().any(|(s, id, _)| !*s && id == "main"));
        assert!(!c.iter().any(|(_, id, _)| id == "inspector"));
    }

    fn frame(ws: &mut Workspace, ctx: &egui::Context, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1586.0, 992.0),
            )),
            events,
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| ws.draw(ui));
    }

    fn release(pos: egui::Pos2) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dragged_dock_edits_draft_only_and_shared_cancel_clears_queue() {
        let app =
            litecord_app::LitecordApp::builder(litecord_core::config::LitecordConfig::default())
                .backend(std::sync::Arc::new(discord_adapter::MockBackend::new(
                    discord_adapter::fixtures::generate(7, litecord_types::Timestamp::now()),
                )))
                .in_memory()
                .start()
                .await
                .unwrap();
        let saved = app.layout_profiles_view().unwrap().profiles;
        let ctx = egui::Context::default();
        let mut ws = Workspace::new(app.clone(), tokio::runtime::Handle::current(), ctx.clone());
        ws.profile = saved.active().unwrap().clone();
        ws.selection.destination = Destination::Messages;
        ws.snapshot = Some(std::sync::Arc::new(
            crate::bridge::snapshot(&app, ws.selection.clone(), None).unwrap(),
        ));
        let original = ws.profile.clone();
        // Right edge of the chat panel.
        let pos = egui::pos2(1240.0, 500.0);

        // A save in flight blocks the drop.
        ws.edit_original = Some(original.clone());
        ws.busy = true;
        ws.drag_source = Some((false, "context".into()));
        frame(&mut ws, &ctx, vec![egui::Event::PointerMoved(pos)]);
        frame(&mut ws, &ctx, vec![release(pos)]);
        assert_eq!(ws.profile, original);
        ws.busy = false;

        ws.drag_source = Some((false, "context".into()));
        frame(&mut ws, &ctx, vec![egui::Event::PointerMoved(pos)]);
        assert!(ws.drag_source.is_some());
        frame(&mut ws, &ctx, vec![release(pos)]);
        assert_ne!(ws.profile, original, "drop should edit the draft");
        ws.profile.validate().unwrap();
        assert!(ws.drag_source.is_none());
        assert_eq!(
            app.layout_profiles_view().unwrap().profiles,
            saved,
            "edit-mode docking stays in the draft until Apply"
        );

        ws.pending_dock = Some((false, "main".into(), "context".into(), Placement::Top));
        ws.resize = Some(QueuedResize {
            shell: false,
            split_id: "destination_root".into(),
            sizes: vec![("main".into(), 1.0)],
        });
        ws.drag_source = Some((false, "main".into()));
        ws.cancel_edit();
        assert_eq!(ws.profile, original);
        assert!(ws.edit_original.is_none());
        assert!(ws.pending_dock.is_none() && ws.resize.is_none() && ws.drag_source.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn normal_mode_projected_resize_is_persisted() {
        let app =
            litecord_app::LitecordApp::builder(litecord_core::config::LitecordConfig::default())
                .backend(std::sync::Arc::new(discord_adapter::MockBackend::new(
                    discord_adapter::fixtures::generate(7, litecord_types::Timestamp::now()),
                )))
                .in_memory()
                .start()
                .await
                .unwrap();
        let view = app.layout_profiles_view().unwrap();
        let ctx = egui::Context::default();
        let mut ws = Workspace::new(app.clone(), tokio::runtime::Handle::current(), ctx.clone());
        ws.profile = view.profiles.active().unwrap().clone();
        ws.layout_token = view.storage_token.clone();
        // Narrow window: the inspector is projected away, so only two sizes arrive.
        ws.resize = Some(QueuedResize {
            shell: false,
            split_id: "destination_root".into(),
            sizes: vec![("context".into(), 200.0), ("main".into(), 300.0)],
        });
        ws.layout_save_pending = true;
        ws.apply_queued_layout_changes(&ctx);
        assert!(ws.notice.is_none(), "{:?}", ws.notice);
        assert!(ws.saving_layout && ws.busy && !ws.layout_save_pending);
        for _ in 0..300 {
            if !ws.busy {
                break;
            }
            frame(&mut ws, &ctx, vec![]);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!ws.busy && !ws.layout_dirty, "{:?}", ws.notice);
        let stored = app.layout_profiles_view().unwrap().profiles;
        let tree = &stored.active().unwrap().destinations[&ws.selection.destination];
        let w = weights(tree);
        assert_eq!(w[2], 350.0, "hidden inspector keeps its weight");
        assert!((w[0] / w[1] - 200.0 / 300.0).abs() < 1e-3);
    }
}
