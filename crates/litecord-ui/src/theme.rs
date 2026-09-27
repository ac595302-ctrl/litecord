//! Design tokens sampled from the accepted mock suite
//! (`docs/design/litecord_ui_mock_audit.pdf`, A01–A12), fonts, egui style,
//! and avatars. Components built on these live in [`crate::kit`].

use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, FontFamily, FontId, Stroke, Ui};

// ---- Surfaces (navy, darkest to lightest) --------------------------------
/// Title bar and window frame.
pub const TITLEBAR: Color32 = Color32::from_rgb(20, 26, 39);
/// Primary navigation rail.
pub const RAIL: Color32 = Color32::from_rgb(17, 23, 34);
/// Legacy name for the rail/shell surface.
pub const SHELL: Color32 = RAIL;
/// Contextual sidebars (conversation list, filters).
pub const SIDEBAR: Color32 = Color32::from_rgb(22, 28, 40);
/// Central workspace.
pub const WORKSPACE: Color32 = Color32::from_rgb(20, 26, 38);
/// Right inspector.
pub const INSPECTOR: Color32 = Color32::from_rgb(20, 26, 37);
/// Cards on any surface.
pub const CARD: Color32 = Color32::from_rgb(24, 33, 48);
/// Inputs: search fields, composer.
pub const FIELD: Color32 = Color32::from_rgb(28, 34, 49);
/// Raised controls: secondary buttons, round actions.
pub const RAISED: Color32 = Color32::from_rgb(33, 41, 58);
pub const HOVER: Color32 = Color32::from_rgb(34, 44, 63);
/// Selected list row (A01 conversation list).
pub const SELECTED: Color32 = Color32::from_rgb(38, 51, 79);
/// Selected rail item and pill tab.
pub const SELECTED_SOFT: Color32 = Color32::from_rgb(32, 42, 64);
/// Card and control outlines.
pub const BORDER: Color32 = Color32::from_rgb(37, 46, 64);
/// Hairline dividers between panels and rows.
pub const DIVIDER: Color32 = Color32::from_rgb(31, 38, 53);

// ---- Text ------------------------------------------------------------------
pub const TEXT: Color32 = Color32::from_rgb(244, 246, 251);
/// Message bodies.
pub const BODY: Color32 = Color32::from_rgb(214, 222, 236);
pub const SECONDARY: Color32 = Color32::from_rgb(167, 177, 196);
pub const MUTED: Color32 = Color32::from_rgb(135, 146, 167);
pub const FAINT: Color32 = Color32::from_rgb(107, 118, 139);

// ---- Semantic hues -----------------------------------------------------------
/// Primary action blue (no glow unless focused).
pub const PRIMARY: Color32 = Color32::from_rgb(58, 116, 253);
pub const PRIMARY_HOVER: Color32 = Color32::from_rgb(84, 136, 255);
/// Primary hue lightened for text and links on dark fills.
pub const PRIMARY_TEXT: Color32 = Color32::from_rgb(128, 166, 255);
/// Omni / AI: teal with a super-subtle glow only.
pub const OMNI: Color32 = Color32::from_rgb(63, 220, 240);
pub const OMNI_TEXT: Color32 = Color32::from_rgb(88, 205, 222);
/// High priority: muted red with the same restrained glow.
pub const PRIORITY: Color32 = Color32::from_rgb(244, 91, 102);
pub const PRIORITY_TEXT: Color32 = Color32::from_rgb(247, 118, 126);
pub const WARNING: Color32 = Color32::from_rgb(246, 176, 48);
pub const SUCCESS: Color32 = Color32::from_rgb(31, 212, 107);

// ---- Fonts -------------------------------------------------------------------

const INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular-subset.ttf");
const INTER_MEDIUM: &[u8] = include_bytes!("../assets/fonts/Inter-Medium-subset.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold-subset.ttf");
const PHOSPHOR_FILL: &[u8] = include_bytes!("../assets/fonts/Phosphor-Fill-subset.ttf");
const PHOSPHOR_REGULAR: &[u8] = include_bytes!("../assets/fonts/Phosphor-Regular-subset.ttf");
const PHOSPHOR_BOLD: &[u8] = include_bytes!("../assets/fonts/Phosphor-Bold-subset.ttf");
/// Noto Emoji (OFL): current Unicode emoji, with the invisible variation
/// selectors and skin-tone modifiers Discord text carries made zero-width.
const NOTO_EMOJI: &[u8] = include_bytes!("../assets/fonts/NotoEmoji-Regular.ttf");

/// Inter (OFL) for text, with egui's defaults as fallback for other scripts
/// and emoji; Phosphor (MIT) for icons.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let add = |fonts: &mut egui::FontDefinitions, name: &str, bytes: &'static [u8], y: f32| {
        let mut data = egui::FontData::from_static(bytes);
        data.tweak.y_offset_factor = y;
        fonts.font_data.insert(name.to_owned(), Arc::new(data));
    };
    add(&mut fonts, "inter", INTER_REGULAR, 0.0);
    add(&mut fonts, "inter-medium", INTER_MEDIUM, 0.0);
    add(&mut fonts, "inter-semibold", INTER_SEMIBOLD, 0.0);
    add(&mut fonts, "ph-fill", PHOSPHOR_FILL, 0.0);
    add(&mut fonts, "ph-line", PHOSPHOR_REGULAR, 0.0);
    add(&mut fonts, "ph-bold", PHOSPHOR_BOLD, 0.0);
    add(&mut fonts, "noto-emoji", NOTO_EMOJI, 0.0);
    // Emoji right after the text face, ahead of egui's older emoji font.
    let mut fallbacks = vec!["noto-emoji".to_owned()];
    fallbacks.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    let family = |first: &str| {
        let mut v = vec![first.to_owned()];
        v.extend(fallbacks.iter().cloned());
        v
    };
    fonts
        .families
        .insert(FontFamily::Proportional, family("inter"));
    fonts
        .families
        .insert(FontFamily::Name(MEDIUM.into()), family("inter-medium"));
    fonts
        .families
        .insert(FontFamily::Name(SEMIBOLD.into()), family("inter-semibold"));
    // Icon subsets lack replacement characters; retain text fallbacks so
    // epaint can render unsupported glyphs without warnings or empty output.
    fonts
        .families
        .insert(FontFamily::Name(ICON_FILL.into()), family("ph-fill"));
    fonts
        .families
        .insert(FontFamily::Name(ICON_LINE.into()), family("ph-line"));
    fonts
        .families
        .insert(FontFamily::Name(ICON_BOLD.into()), family("ph-bold"));
    if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
        mono.insert(1.min(mono.len()), "noto-emoji".to_owned());
    }
    ctx.set_fonts(fonts);
}

const MEDIUM: &str = "inter-medium";
const SEMIBOLD: &str = "inter-semibold";
const ICON_FILL: &str = "ph-fill";
const ICON_LINE: &str = "ph-line";
const ICON_BOLD: &str = "ph-bold";

#[cfg(test)]
mod font_tests {
    use super::*;

    #[test]
    fn icon_families_support_icons_and_replacement_characters() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.ctx().fonts_mut(|fonts| {
                for font in [icon_fill(16.0), icon_line(16.0), icon_bold(16.0)] {
                    assert!(fonts.has_glyph(&font, '\u{E2C2}'), "missing house icon");
                    assert!(fonts.has_glyph(&font, '?'), "missing replacement glyph");
                    assert!(fonts.glyph_width(&font, '\u{10FFFF}') > 0.0);
                }
            });
        });
    }
}

/// Inter Regular.
pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}
/// Inter Medium (names, labels).
pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MEDIUM.into()))
}
/// Inter SemiBold (titles, headings).
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}
/// Phosphor Fill icons (rail, bubbles, header actions).
pub fn icon_fill(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICON_FILL.into()))
}
/// Phosphor Bold (heavy outline) icons.
pub fn icon_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICON_BOLD.into()))
}
/// Phosphor Regular (outline) icons.
pub fn icon_line(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ICON_LINE.into()))
}

/// Apply fonts, the shared dark palette, typography, spacing and radii.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_theme(egui::Theme::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.dark_mode = true;
    visuals.override_text_color = None;
    visuals.weak_text_color = Some(SECONDARY);
    visuals.weak_text_alpha = 1.0;
    visuals.window_fill = CARD;
    visuals.window_stroke = Stroke::new(1.0_f32, BORDER);
    visuals.window_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(100),
    };
    visuals.panel_fill = WORKSPACE;
    visuals.faint_bg_color = SIDEBAR;
    visuals.extreme_bg_color = FIELD;
    visuals.text_edit_bg_color = Some(FIELD);
    visuals.code_bg_color = RAISED;
    visuals.hyperlink_color = PRIMARY_TEXT;
    visuals.warn_fg_color = WARNING;
    visuals.error_fg_color = PRIORITY;
    visuals.window_corner_radius = egui::CornerRadius::same(12);
    visuals.menu_corner_radius = egui::CornerRadius::same(10);
    visuals.selection.bg_fill = PRIMARY.gamma_multiply(0.45);
    visuals.selection.stroke = Stroke::new(1.0_f32, PRIMARY_TEXT);
    visuals.text_cursor.stroke = Stroke::new(2.0_f32, PRIMARY_TEXT);
    visuals.striped = false;
    visuals.slider_trailing_fill = true;

    set_widget(&mut visuals.widgets.noninteractive, CARD, TEXT, DIVIDER, 8);
    set_widget(&mut visuals.widgets.inactive, RAISED, TEXT, BORDER, 8);
    set_widget(
        &mut visuals.widgets.hovered,
        HOVER,
        TEXT,
        PRIMARY_TEXT.gamma_multiply(0.5),
        8,
    );
    set_widget(&mut visuals.widgets.active, SELECTED, TEXT, PRIMARY, 8);
    set_widget(&mut visuals.widgets.open, RAISED, TEXT, BORDER, 8);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, SECONDARY);

    let mut style = (*ctx.global_style()).clone();
    style.visuals = visuals;
    let t = &mut style.text_styles;
    t.insert(egui::TextStyle::Small, regular(12.0));
    t.insert(egui::TextStyle::Body, regular(14.0));
    t.insert(egui::TextStyle::Button, medium(14.0));
    t.insert(egui::TextStyle::Heading, semibold(20.0));
    t.insert(egui::TextStyle::Monospace, FontId::monospace(13.0));
    t.insert(
        egui::TextStyle::Name("destination-title".into()),
        semibold(26.0),
    );
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.window_margin = egui::Margin::same(18);
    style.spacing.menu_margin = egui::Margin::same(8);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size = egui::vec2(32.0, 30.0);
    style.spacing.combo_height = 280.0;
    style.spacing.scroll = egui::style::ScrollStyle {
        bar_width: 6.0,
        floating: true,
        // Hidden until the pointer is near or the area scrolls (the mocks
        // show no scroll bars).
        dormant_handle_opacity: 0.0,
        dormant_background_opacity: 0.0,
        ..egui::style::ScrollStyle::floating()
    };
    ctx.set_style_of(egui::Theme::Dark, style);
}

fn set_widget(
    widget: &mut egui::style::WidgetVisuals,
    fill: Color32,
    text: Color32,
    outline: Color32,
    radius: u8,
) {
    widget.bg_fill = fill;
    widget.weak_bg_fill = fill;
    widget.bg_stroke = Stroke::new(1.0_f32, outline);
    widget.fg_stroke = Stroke::new(1.0_f32, text);
    widget.corner_radius = egui::CornerRadius::same(radius);
    widget.expansion = 0.0;
}

// ---- Presence and avatars ------------------------------------------------------

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

    /// Capitalized label for rows ("Online", "Idle").
    pub fn title(self) -> &'static str {
        match self {
            Self::Online => "Online",
            Self::Idle => "Idle",
            Self::Dnd => "Do not disturb",
            Self::Offline => "Offline",
            Self::None => "",
        }
    }

    pub fn color(self) -> Color32 {
        match self {
            Self::Online => SUCCESS,
            Self::Idle => WARNING,
            Self::Dnd => PRIORITY,
            Self::Offline | Self::None => FAINT,
        }
    }
}

/// Avatar hues: saturated enough to read as "photos" in the mock's density,
/// dark enough for white initials.
const AVATAR_HUES: [(Color32, Color32); 8] = [
    (
        Color32::from_rgb(76, 122, 236),
        Color32::from_rgb(45, 76, 170),
    ),
    (
        Color32::from_rgb(38, 170, 160),
        Color32::from_rgb(22, 104, 110),
    ),
    (
        Color32::from_rgb(160, 104, 232),
        Color32::from_rgb(98, 56, 164),
    ),
    (
        Color32::from_rgb(232, 118, 96),
        Color32::from_rgb(158, 64, 60),
    ),
    (
        Color32::from_rgb(76, 176, 110),
        Color32::from_rgb(36, 110, 70),
    ),
    (
        Color32::from_rgb(222, 164, 64),
        Color32::from_rgb(150, 96, 30),
    ),
    (
        Color32::from_rgb(112, 118, 240),
        Color32::from_rgb(64, 64, 168),
    ),
    (
        Color32::from_rgb(226, 98, 150),
        Color32::from_rgb(150, 50, 96),
    ),
];

fn avatar_hues(label: &str) -> (Color32, Color32) {
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

/// Text avatar with a status-aware presence marker.
pub fn avatar_presence(ui: &mut Ui, label: &str, size: f32, presence: Presence) -> egui::Response {
    let display_name = if label.trim().is_empty() {
        "Unknown user"
    } else {
        label.trim()
    };
    let size = size.max(1.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        let ring = ui.visuals().panel_fill;
        paint_avatar(
            ui.painter(),
            rect.center(),
            size,
            display_name,
            presence,
            ring,
        );
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

/// Paint a gradient initials avatar centered at `center`. `ring` is the
/// surface behind the presence marker (it cuts the marker out of the disc).
pub fn paint_avatar(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    name: &str,
    presence: Presence,
    ring: Color32,
) {
    let r = size * 0.5;
    let (top, bottom) = avatar_hues(name);
    gradient_disc(painter, center, r, top, bottom);
    painter.text(
        center,
        Align2::CENTER_CENTER,
        initials(name),
        semibold((size * 0.38).clamp(8.0, 40.0)),
        Color32::WHITE,
    );
    paint_presence(painter, center, size, presence, ring);
}

/// Like [`paint_avatar`], but draws the user's Discord avatar image when
/// `url` is known and loaded; the initials disc shows while it loads, when
/// it fails, or when there is no avatar.
pub fn paint_avatar_url(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    name: &str,
    url: Option<&str>,
    presence: Presence,
    ring: Color32,
) {
    if !url.is_some_and(|url| paint_avatar_image(painter, center, size, url)) {
        paint_avatar(painter, center, size, name, Presence::None, ring);
    }
    paint_presence(painter, center, size, presence, ring);
}

/// Draws the image at `url` clipped to a circle; false until it is loaded.
fn paint_avatar_image(painter: &egui::Painter, center: egui::Pos2, size: f32, url: &str) -> bool {
    let px = size * painter.ctx().pixels_per_point();
    // Discord's CDN serves the size asked for; small avatars stay small in
    // memory. Other URLs are loaded as they are.
    let edge: u32 = if px <= 64.0 { 64 } else { 128 };
    let uri = if url.starts_with("https://cdn.discordapp.com/") && !url.contains('?') {
        format!("{url}?size={edge}")
    } else {
        url.to_owned()
    };
    let hint = egui::SizeHint::Size {
        width: edge,
        height: edge,
        maintain_aspect_ratio: true,
    };
    match painter
        .ctx()
        .try_load_texture(&uri, egui::TextureOptions::LINEAR, hint)
    {
        Ok(egui::load::TexturePoll::Ready { texture }) => {
            let rect = egui::Rect::from_center_size(center, egui::vec2(size, size));
            let radius = egui::CornerRadius::same((size * 0.5).min(255.0) as u8);
            painter.add(
                egui::epaint::RectShape::filled(rect, radius, Color32::WHITE).with_texture(
                    texture.id,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                ),
            );
            true
        }
        _ => false,
    }
}

/// Presence marker at the bottom-right of an avatar of `size` at `center`.
pub fn paint_presence(
    painter: &egui::Painter,
    center: egui::Pos2,
    size: f32,
    presence: Presence,
    ring: Color32,
) {
    if presence == Presence::None {
        return;
    }
    let radius = (size * 0.13).clamp(3.5, 9.0);
    let at = center + egui::vec2(size * 0.5 - radius * 0.95, size * 0.5 - radius * 0.95);
    let color = presence.color();
    painter.circle_filled(at, radius + (size * 0.045).clamp(1.5, 3.0), ring);
    match presence {
        Presence::Online => {
            painter.circle_filled(at, radius, color);
        }
        Presence::Idle => {
            painter.circle_filled(at, radius, color);
            painter.circle_filled(
                at + egui::vec2(-radius * 0.45, -radius * 0.45),
                radius * 0.6,
                ring,
            );
        }
        Presence::Dnd => {
            painter.circle_filled(at, radius, color);
            painter.line_segment(
                [
                    at - egui::vec2(radius * 0.5, 0.0),
                    at + egui::vec2(radius * 0.5, 0.0),
                ],
                Stroke::new((radius * 0.42).max(1.0), ring),
            );
        }
        Presence::Offline | Presence::None => {
            painter.circle_stroke(at, radius * 0.72, Stroke::new(1.8_f32, color));
        }
    }
}

/// A disc shaded from `top` to `bottom` (a soft photographic feel).
pub fn gradient_disc(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    top: Color32,
    bottom: Color32,
) {
    let segments = 48;
    let points: Vec<egui::Pos2> = (0..segments)
        .map(|i| {
            let a = i as f32 / segments as f32 * std::f32::consts::TAU;
            center + egui::vec2(a.cos(), a.sin()) * (radius - 0.5)
        })
        .collect();
    gradient_fan(
        painter,
        center,
        &points,
        center.y - radius,
        2.0 * radius,
        top,
        bottom,
    );
    // Meshes are not antialiased; a hairline ring smooths the edge.
    painter.circle_stroke(
        center,
        radius - 0.5,
        Stroke::new(1.0_f32, lerp(top, bottom, 0.55)),
    );
}

/// A rounded rectangle shaded from `top` to `bottom` (server tiles, logo).
pub fn gradient_rect(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: f32,
    top: Color32,
    bottom: Color32,
) {
    let r = radius.min(rect.width() * 0.5).min(rect.height() * 0.5);
    let inner = rect.shrink(0.5);
    let corners = [
        (egui::pos2(inner.right() - r, inner.bottom() - r), 0.0_f32),
        (egui::pos2(inner.left() + r, inner.bottom() - r), 90.0),
        (egui::pos2(inner.left() + r, inner.top() + r), 180.0),
        (egui::pos2(inner.right() - r, inner.top() + r), 270.0),
    ];
    let mut points = Vec::with_capacity(4 * 9);
    for (c, start) in corners {
        for step in 0..=8 {
            let a = (start + step as f32 * 90.0 / 8.0).to_radians();
            points.push(c + egui::vec2(a.cos(), a.sin()) * (r - 0.5).max(0.0));
        }
    }
    gradient_fan(
        painter,
        rect.center(),
        &points,
        rect.top(),
        rect.height(),
        top,
        bottom,
    );
    painter.rect_stroke(
        rect.shrink(0.5),
        r,
        Stroke::new(1.0_f32, lerp(top, bottom, 0.55)),
        egui::StrokeKind::Middle,
    );
}

/// Triangle fan over a convex outline, colored by vertical position.
fn gradient_fan(
    painter: &egui::Painter,
    center: egui::Pos2,
    outline: &[egui::Pos2],
    top_y: f32,
    height: f32,
    top: Color32,
    bottom: Color32,
) {
    let shade = |y: f32| lerp(top, bottom, ((y - top_y) / height.max(1.0)).clamp(0.0, 1.0));
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, shade(center.y));
    for p in outline {
        mesh.colored_vertex(*p, shade(p.y));
    }
    let n = outline.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(egui::Shape::mesh(mesh));
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(
        f(a.r(), b.r()),
        f(a.g(), b.g()),
        f(a.b(), b.b()),
        f(a.a(), b.a()),
    )
}

pub fn initials(label: &str) -> String {
    let initials: String = label
        .split_whitespace()
        .filter(|w| w.chars().next().is_some_and(char::is_alphanumeric))
        .take(2)
        .filter_map(|word| word.chars().next())
        .flat_map(char::to_uppercase)
        .collect();
    if initials.is_empty() {
        label
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_else(|| "?".to_owned())
    } else {
        initials
    }
}

pub fn stable_hash(label: &str) -> u32 {
    label.bytes().fold(2_166_136_261, |hash, byte| {
        hash.wrapping_mul(16_777_619) ^ u32::from(byte)
    })
}

// ---- Small legacy helpers kept for callers not yet on `kit` ----------------------

/// Compact uppercase section heading.
pub fn section_label(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .font(semibold(11.0))
            .color(MUTED),
    );
}

/// Quiet, noninteractive tinted label chip.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) {
    crate::kit::status_pill(ui, None, text, color);
}

/// Readable text color for a chip of `color`.
pub fn chip_text(color: Color32) -> Color32 {
    if color == PRIMARY {
        PRIMARY_TEXT
    } else if color == OMNI {
        OMNI_TEXT
    } else if color == PRIORITY {
        PRIORITY_TEXT
    } else if color == MUTED || color == BORDER {
        SECONDARY
    } else {
        color
    }
}

/// Fill behind a panel.
pub fn surface(ui: &Ui, rect: egui::Rect, base: Color32) {
    ui.painter().rect_filled(rect, 0.0, base);
}

/// Compact secondary text.
pub fn meta(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).font(regular(13.0)).color(MUTED)
}

/// Title for a destination's center panel plus an optional subtitle.
pub fn page_header(ui: &mut Ui, title: &str, subtitle: Option<&str>) {
    crate::kit::page_title(ui, title, subtitle);
}

/// Honest empty state: what's missing and what to do about it.
pub fn empty_state(ui: &mut Ui, title: &str, body: &str) {
    crate::kit::empty(ui, None, title, body);
}

/// A selectable sidebar/filter row with an optional trailing count.
pub fn nav_row(ui: &mut Ui, label: &str, count: Option<usize>, selected: bool) -> egui::Response {
    crate::kit::side_item(ui, None, label, count, selected)
}

#[cfg(test)]
mod emoji_font_tests {
    use super::*;

    /// Emoji pasted from Discord carry variation selectors and skin tones;
    /// newer emoji need the bundled font. None of them may fall back to the
    /// replacement box.
    #[test]
    fn discord_emoji_have_glyphs_and_modifiers_take_no_space() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let _ = ctx.run_ui(Default::default(), |_| {});
        let font = FontId::proportional(16.0);
        for c in [
            '😂',
            '❤',
            '\u{FE0F}',
            '🫠',
            '🥹',
            '🫡',
            '\u{1F3FD}',
            '\u{200D}',
            '🇺',
        ] {
            assert!(
                ctx.fonts_mut(|f| f.has_glyph(&font, c)),
                "missing glyph U+{:04X}",
                c as u32
            );
        }
        for c in ['\u{FE0F}', '\u{1F3FD}'] {
            let w = ctx.fonts_mut(|f| f.glyph_width(&font, c));
            assert!(w < 0.5, "U+{:04X} should be zero-width, was {w}", c as u32);
        }
    }
}
