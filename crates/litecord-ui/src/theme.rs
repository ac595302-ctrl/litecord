use eframe::egui::{self, Align2, Color32, FontId, Stroke, Ui};

// Darker navy/charcoal surfaces derived from the A01 reference. Contrast
// against TEXT/SECONDARY stays above 4.5:1 on every surface.
pub const SHELL: Color32 = Color32::from_rgb(14, 19, 28);
pub const SIDEBAR: Color32 = Color32::from_rgb(18, 24, 36);
pub const WORKSPACE: Color32 = Color32::from_rgb(21, 28, 41);
pub const RAISED: Color32 = Color32::from_rgb(30, 39, 56);
pub const HOVER: Color32 = Color32::from_rgb(35, 46, 66);
pub const BORDER: Color32 = Color32::from_rgb(41, 53, 74);
pub const TEXT: Color32 = Color32::from_rgb(231, 236, 245);
pub const SECONDARY: Color32 = Color32::from_rgb(176, 189, 210);
pub const MUTED: Color32 = Color32::from_rgb(142, 156, 180);
pub const PRIMARY: Color32 = Color32::from_rgb(77, 125, 255);
/// Primary hue lightened for text on dark fills (contrast ≥ 4.5:1).
pub const PRIMARY_TEXT: Color32 = Color32::from_rgb(143, 176, 255);
pub const SELECTED: Color32 = Color32::from_rgb(36, 54, 84);
pub const OMNI: Color32 = Color32::from_rgb(80, 210, 193);
pub const PRIORITY: Color32 = Color32::from_rgb(240, 139, 145);
pub const WARNING: Color32 = Color32::from_rgb(232, 184, 90);
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
    set_widget(&mut visuals.widgets.hovered, HOVER, HOVER, TEXT, PRIMARY, 6);
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
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size = egui::vec2(32.0, 28.0);
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

/// Presence shown on an avatar: shape and color both carry the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Online,
    Idle,
    Dnd,
    Offline,
    /// No presence indicator (e.g. yourself in compact lists, bots).
    None,
}

impl Presence {
    pub fn from_status(status: &str) -> Self {
        match status {
            "online" => Self::Online,
            "idle" => Self::Idle,
            "dnd" => Self::Dnd,
            "offline" | "invisible" => Self::Offline,
            _ => Self::None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Idle => "idle",
            Self::Dnd => "do not disturb",
            Self::Offline => "offline",
            Self::None => "",
        }
    }

    pub fn color(self) -> Color32 {
        match self {
            Self::Online => SUCCESS,
            Self::Idle => WARNING,
            Self::Dnd => PRIORITY,
            Self::Offline | Self::None => MUTED,
        }
    }
}

/// Muted hues for fallback avatars: distinct, but calm on navy surfaces and
/// dark enough for light initials (≥ 4.5:1 with TEXT).
const AVATAR_HUES: [Color32; 8] = [
    Color32::from_rgb(52, 86, 150),
    Color32::from_rgb(38, 110, 104),
    Color32::from_rgb(112, 70, 132),
    Color32::from_rgb(140, 78, 70),
    Color32::from_rgb(58, 108, 64),
    Color32::from_rgb(128, 96, 40),
    Color32::from_rgb(84, 76, 150),
    Color32::from_rgb(40, 96, 128),
];

/// Deterministic fallback color for a name.
pub fn avatar_color(label: &str) -> Color32 {
    AVATAR_HUES[(stable_hash(label.trim()) as usize) % AVATAR_HUES.len()]
}

/// Text avatar with a deterministic color; `online` shows a presence dot.
pub fn avatar(ui: &mut Ui, label: &str, size: f32, online: bool) -> egui::Response {
    avatar_presence(
        ui,
        label,
        size,
        if online {
            Presence::Online
        } else {
            Presence::None
        },
    )
}

/// Text avatar with a status-aware presence marker: filled dot (online),
/// crescent-like ring (idle), bar (dnd) or hollow ring (offline).
pub fn avatar_presence(ui: &mut Ui, label: &str, size: f32, presence: Presence) -> egui::Response {
    let display_name = if label.trim().is_empty() {
        "Unknown user"
    } else {
        label.trim()
    };
    let initials = initials(display_name);
    let size = size.max(1.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.circle_filled(rect.center(), size * 0.5, avatar_color(display_name));
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            initials,
            FontId::proportional((size * 0.38).clamp(8.0, size * 0.48)),
            TEXT,
        );
        if presence != Presence::None {
            let radius = (size * 0.14).max(3.0);
            let center = egui::pos2(rect.right() - radius * 0.9, rect.bottom() - radius * 0.9);
            let color = presence.color();
            painter.circle_filled(center, radius + 1.5, SHELL);
            match presence {
                Presence::Online => {
                    painter.circle_filled(center, radius, color);
                }
                Presence::Idle => {
                    painter.circle_filled(center, radius, color);
                    painter.circle_filled(
                        center + egui::vec2(-radius * 0.45, -radius * 0.45),
                        radius * 0.6,
                        SHELL,
                    );
                }
                Presence::Dnd => {
                    painter.circle_filled(center, radius, color);
                    painter.line_segment(
                        [
                            center - egui::vec2(radius * 0.55, 0.0),
                            center + egui::vec2(radius * 0.55, 0.0),
                        ],
                        Stroke::new((radius * 0.45).max(1.0), SHELL),
                    );
                }
                Presence::Offline | Presence::None => {
                    painter.circle_stroke(center, radius * 0.8, Stroke::new(1.5_f32, color));
                }
            }
        }
    }

    let presence_text = presence.label();
    let accessible_label = if presence_text.is_empty() {
        format!("Avatar for {display_name}")
    } else {
        format!("Avatar for {display_name}, {presence_text}")
    };
    let response = response.on_hover_text(if presence_text.is_empty() {
        display_name.to_owned()
    } else {
        format!("{display_name} · {presence_text}")
    });
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
            ui.label(egui::RichText::new(text).size(12.0).color(chip_text(color)));
        });
}

/// Readable text color for a chip of `color`: the hue itself for light
/// hues, a lightened variant for the primary blue.
pub fn chip_text(color: Color32) -> Color32 {
    if color == PRIMARY {
        PRIMARY_TEXT
    } else if color == MUTED || color == BORDER {
        SECONDARY
    } else {
        color
    }
}

/// Subtle vertical gradient behind a panel (top slightly lighter).
pub fn surface(ui: &Ui, rect: egui::Rect, base: Color32) {
    let top = lighten(base, 6);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), base);
    mesh.colored_vertex(rect.left_bottom(), base);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}

fn lighten(c: Color32, by: u8) -> Color32 {
    Color32::from_rgb(
        c.r().saturating_add(by),
        c.g().saturating_add(by),
        c.b().saturating_add(by),
    )
}

/// Compact secondary text.
pub fn meta(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).size(12.0).color(MUTED)
}

/// Title for a destination's center panel plus an optional subtitle.
pub fn page_header(ui: &mut Ui, title: &str, subtitle: Option<&str>) {
    ui.label(egui::RichText::new(title).size(20.0).color(TEXT));
    if let Some(sub) = subtitle {
        ui.label(egui::RichText::new(sub).size(13.0).color(SECONDARY));
    }
    ui.add_space(4.0);
}

/// Honest empty state: what's missing and what to do about it.
pub fn empty_state(ui: &mut Ui, title: &str, body: &str) {
    ui.add_space(12.0);
    ui.label(egui::RichText::new(title).size(14.0).color(SECONDARY));
    ui.label(egui::RichText::new(body).size(12.0).color(MUTED));
    ui.add_space(12.0);
}

/// A selectable sidebar/filter row with an optional trailing count.
pub fn nav_row(ui: &mut Ui, label: &str, count: Option<usize>, selected: bool) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 30.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            SELECTED
        } else if response.hovered() {
            HOVER
        } else {
            Color32::TRANSPARENT
        };
        let painter = ui.painter();
        painter.rect_filled(rect, 6.0, fill);
        if selected {
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 6.0), egui::vec2(3.0, 18.0)),
                2.0,
                PRIMARY,
            );
        }
        painter.text(
            rect.left_center() + egui::vec2(12.0, 0.0),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(14.0),
            if selected { TEXT } else { SECONDARY },
        );
        if let Some(n) = count {
            painter.text(
                rect.right_center() - egui::vec2(10.0, 0.0),
                Align2::RIGHT_CENTER,
                n.to_string(),
                FontId::proportional(12.0),
                MUTED,
            );
        }
    }
    let label = label.to_owned();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label)
    });
    response
}

/// A pill tab (A01 conversation filters) with an optional count badge.
pub fn pill(ui: &mut Ui, label: &str, badge: Option<usize>, selected: bool) -> egui::Response {
    let font = FontId::proportional(13.0);
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), TEXT)
        .size()
        .x;
    let badge_w = if badge.is_some_and(|n| n > 0) {
        22.0
    } else {
        0.0
    };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(text_w + 20.0 + badge_w, 26.0),
        egui::Sense::click(),
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = if selected {
            SELECTED
        } else if response.hovered() {
            HOVER
        } else {
            Color32::TRANSPARENT
        };
        painter.rect_filled(rect, 7.0, fill);
        painter.text(
            rect.left_center() + egui::vec2(10.0, 0.0),
            Align2::LEFT_CENTER,
            label,
            font,
            if selected { TEXT } else { SECONDARY },
        );
        if let Some(n) = badge.filter(|n| *n > 0) {
            let c = egui::pos2(rect.right() - 16.0, rect.center().y);
            painter.circle_filled(c, 8.0, PRIMARY);
            painter.text(
                c,
                Align2::CENTER_CENTER,
                if n > 9 {
                    "9+".to_owned()
                } else {
                    n.to_string()
                },
                FontId::proportional(10.0),
                TEXT,
            );
        }
    }
    let label = label.to_owned();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label)
    });
    response
}

/// A search field with a magnifier glyph (A01 sidebars).
pub fn search_field(ui: &mut Ui, text: &mut String, hint: &str) -> egui::Response {
    egui::Frame::new()
        .fill(WORKSPACE)
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(8)
        .inner_margin(egui::Margin {
            left: 28,
            right: 8,
            top: 5,
            bottom: 5,
        })
        .show(ui, |ui| {
            let r = ui.add(
                egui::TextEdit::singleline(text)
                    .frame(egui::Frame::NONE)
                    .hint_text(hint)
                    .desired_width(f32::INFINITY),
            );
            crate::icons::glyph(
                ui.painter(),
                r.rect.left_center() - egui::vec2(15.0, 0.0),
                13.0,
                crate::icons::Glyph::Search,
                MUTED,
            );
            r
        })
        .inner
}
