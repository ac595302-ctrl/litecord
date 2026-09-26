use eframe::egui::{self, Align2, Color32, FontId, Stroke, Ui};

pub const SHELL: Color32 = Color32::from_rgb(17, 23, 34);
pub const SIDEBAR: Color32 = Color32::from_rgb(21, 29, 43);
pub const WORKSPACE: Color32 = Color32::from_rgb(23, 31, 44);
pub const RAISED: Color32 = Color32::from_rgb(32, 42, 60);
pub const BORDER: Color32 = Color32::from_rgb(45, 58, 80);
pub const TEXT: Color32 = Color32::from_rgb(231, 236, 245);
pub const SECONDARY: Color32 = Color32::from_rgb(176, 189, 210);
pub const MUTED: Color32 = Color32::from_rgb(142, 156, 180);
pub const PRIMARY: Color32 = Color32::from_rgb(77, 125, 255);
pub const SELECTED: Color32 = Color32::from_rgb(38, 57, 87);
pub const OMNI: Color32 = Color32::from_rgb(80, 210, 193);
pub const PRIORITY: Color32 = Color32::from_rgb(240, 139, 145);
pub const SUCCESS: Color32 = Color32::from_rgb(85, 215, 160);

/// Apply the shared dark palette, typography, spacing, and corner radii.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.dark_mode = true;
    visuals.override_text_color = None;
    visuals.weak_text_color = Some(SECONDARY);
    visuals.weak_text_alpha = 1.0;
    visuals.window_fill = SHELL;
    visuals.window_stroke = Stroke::new(1.0_f32, BORDER);
    visuals.panel_fill = SHELL;
    visuals.faint_bg_color = SIDEBAR;
    visuals.extreme_bg_color = WORKSPACE;
    visuals.text_edit_bg_color = Some(RAISED);
    visuals.code_bg_color = RAISED;
    visuals.hyperlink_color = PRIMARY;
    visuals.warn_fg_color = PRIORITY;
    visuals.error_fg_color = PRIORITY;
    visuals.window_corner_radius = egui::CornerRadius::same(8);
    visuals.menu_corner_radius = egui::CornerRadius::same(8);
    visuals.selection.bg_fill = SELECTED;
    visuals.selection.stroke = Stroke::new(1.0_f32, PRIMARY);
    visuals.text_cursor.stroke = Stroke::new(2.0_f32, PRIMARY);

    set_widget(
        &mut visuals.widgets.noninteractive,
        WORKSPACE,
        WORKSPACE,
        TEXT,
        BORDER,
        6,
    );
    set_widget(
        &mut visuals.widgets.inactive,
        RAISED,
        RAISED,
        TEXT,
        BORDER,
        6,
    );
    set_widget(
        &mut visuals.widgets.hovered,
        SELECTED,
        SELECTED,
        TEXT,
        PRIMARY,
        6,
    );
    set_widget(
        &mut visuals.widgets.active,
        SELECTED,
        SELECTED,
        TEXT,
        PRIMARY,
        6,
    );
    set_widget(&mut visuals.widgets.open, RAISED, RAISED, TEXT, BORDER, 6);

    let mut style = (*ctx.global_style()).clone();
    style.visuals = visuals;
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(12.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, FontId::proportional(18.0));
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, FontId::monospace(13.0));
    style.text_styles.insert(
        egui::TextStyle::Name("destination-title".into()),
        FontId::proportional(20.0),
    );
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.window_margin = egui::Margin::same(16);
    style.spacing.menu_margin = egui::Margin::same(8);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style.spacing.interact_size = egui::vec2(40.0, 32.0);
    ctx.set_style_of(egui::Theme::Dark, style);
}

fn set_widget(
    widget: &mut egui::style::WidgetVisuals,
    bg_fill: Color32,
    weak_bg_fill: Color32,
    text: Color32,
    outline: Color32,
    radius: u8,
) {
    widget.bg_fill = bg_fill;
    widget.weak_bg_fill = weak_bg_fill;
    widget.bg_stroke = Stroke::new(1.0_f32, outline);
    widget.fg_stroke = Stroke::new(1.0_f32, text);
    widget.corner_radius = egui::CornerRadius::same(radius);
    widget.expansion = 0.0;
}

/// Draw a text-based avatar fallback with a deterministic surface tint.
pub fn avatar(ui: &mut Ui, label: &str, size: f32, online: bool) -> egui::Response {
    let display_name = if label.trim().is_empty() {
        "Unknown user"
    } else {
        label.trim()
    };
    let initials = initials(display_name);
    let size = size.max(1.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());

    if ui.is_rect_visible(rect) {
        let backgrounds = [RAISED, SELECTED, SIDEBAR];
        let fill = backgrounds[(stable_hash(display_name) as usize) % backgrounds.len()];
        let painter = ui.painter();
        painter.circle_filled(rect.center(), size * 0.5, fill);
        painter.circle_stroke(rect.center(), size * 0.5, Stroke::new(1.0_f32, BORDER));
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            initials,
            FontId::proportional((size * 0.36).max(8.0).min(size * 0.48)),
            TEXT,
        );

        if online {
            let radius = (size * 0.16).max(2.0);
            let center = egui::pos2(rect.right() - radius, rect.bottom() - radius);
            painter.circle_filled(center, radius, SUCCESS);
            painter.circle_stroke(center, radius, Stroke::new(1.5_f32, WORKSPACE));
        }
    }

    let presence = if online { "online" } else { "offline" };
    let accessible_label = format!("Avatar for {display_name}, {presence}");
    let response = response.on_hover_text(format!("{display_name} · {presence}"));
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Image, true, accessible_label.clone())
    });
    response
}

fn initials(label: &str) -> String {
    let initials: String = label
        .split_whitespace()
        .take(2)
        .filter_map(|word| word.chars().next())
        .flat_map(char::to_uppercase)
        .collect();
    if initials.is_empty() {
        "?".to_owned()
    } else {
        initials
    }
}

fn stable_hash(label: &str) -> u32 {
    label.bytes().fold(2_166_136_261, |hash, byte| {
        hash.wrapping_mul(16_777_619) ^ u32::from(byte)
    })
}

/// Add a compact uppercase section heading.
pub fn section_label(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(12.0)
            .color(SECONDARY)
            .strong(),
    );
}

/// Draw a quiet, noninteractive tinted label chip.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) {
    let fill = color.gamma_multiply(0.14);
    let outline = color.gamma_multiply(0.38);
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, outline))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(12.0).color(SECONDARY));
        });
}
