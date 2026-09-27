//! Component kit reproducing the mock suite's visual language: cards, tinted
//! icon bubbles, pill tabs, filter chips, buttons, round actions, rows.
//!
//! Everything paints inside an explicitly allocated rect and elides text to
//! its width, so rows never push past their panel (no clipped buttons or
//! overlapping labels at narrow widths).

// A component library: not every component is used by every screen yet.

use std::sync::Arc;

use eframe::egui::{
    self, text::LayoutJob, Align2, Color32, FontId, Galley, Painter, Pos2, Rect, Response, Sense,
    Stroke, Ui,
};

use crate::ph;
use crate::theme::{self, *};

// ---- Tints -----------------------------------------------------------------------

/// A hue used for icon bubbles, tags and accent cards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tint {
    /// Glyph / text color.
    pub fg: Color32,
    /// Bubble fill.
    pub bg: Color32,
}

impl Tint {
    pub const fn new(fg: Color32, bg: Color32) -> Self {
        Self { fg, bg }
    }
}

pub const BLUE: Tint = Tint::new(
    Color32::from_rgb(74, 132, 255),
    Color32::from_rgb(35, 57, 106),
);
pub const GREEN: Tint = Tint::new(
    Color32::from_rgb(40, 226, 132),
    Color32::from_rgb(24, 68, 60),
);
pub const PURPLE: Tint = Tint::new(
    Color32::from_rgb(190, 122, 248),
    Color32::from_rgb(58, 44, 98),
);
pub const TEAL: Tint = Tint::new(
    Color32::from_rgb(64, 228, 246),
    Color32::from_rgb(22, 84, 100),
);
pub const RED: Tint = Tint::new(
    Color32::from_rgb(248, 108, 112),
    Color32::from_rgb(86, 40, 52),
);
pub const YELLOW: Tint = Tint::new(
    Color32::from_rgb(248, 184, 60),
    Color32::from_rgb(80, 62, 36),
);
pub const ORANGE: Tint = Tint::new(
    Color32::from_rgb(250, 146, 70),
    Color32::from_rgb(84, 50, 36),
);
pub const PINK: Tint = Tint::new(
    Color32::from_rgb(240, 110, 170),
    Color32::from_rgb(82, 40, 72),
);
pub const GREY: Tint = Tint::new(
    Color32::from_rgb(196, 204, 222),
    Color32::from_rgb(42, 50, 68),
);

/// Deterministic tint for an entity name (server tiles, group icons).
pub fn tint_for(name: &str) -> Tint {
    const HUES: [Tint; 6] = [BLUE, PURPLE, TEAL, GREEN, ORANGE, PINK];
    HUES[(theme::stable_hash(name) as usize) % HUES.len()]
}

// ---- Text --------------------------------------------------------------------------

/// Single-line galley elided to `width` with an ellipsis.
pub fn elided(
    painter: &Painter,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(8.0));
    painter.layout_job(job)
}

/// Wrapped galley limited to `rows` lines (ellipsis on the last).
pub fn wrapped(
    painter: &Painter,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
    rows: usize,
) -> Arc<Galley> {
    let mut job = LayoutJob::simple(text.to_owned(), font, color, width.max(8.0));
    job.wrap.max_rows = rows.max(1);
    job.wrap.break_anywhere = false;
    job.wrap.overflow_character = Some('…');
    painter.layout_job(job)
}

/// Paint single-line text anchored at `pos` by `align`, elided to `width`.
/// Returns the painted rect.
pub fn text_at(
    painter: &Painter,
    pos: Pos2,
    align: Align2,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
) -> Rect {
    let g = elided(painter, text, font, color, width);
    let rect = align.anchor_size(pos, g.size());
    painter.galley(rect.min, g, color);
    rect
}

/// Width of `text` in `font` on one line.
pub fn text_width(painter: &Painter, text: &str, font: FontId) -> f32 {
    painter.layout_no_wrap(text.to_owned(), font, TEXT).size().x
}

/// A label with an explicit font that truncates to the available width.
pub fn label(ui: &mut Ui, text: impl Into<String>, font: FontId, color: Color32) -> Response {
    ui.add(egui::Label::new(egui::RichText::new(text).font(font).color(color)).truncate())
}

/// A wrapping label with an explicit font.
pub fn para(ui: &mut Ui, text: impl Into<String>, font: FontId, color: Color32) -> Response {
    ui.add(egui::Label::new(egui::RichText::new(text).font(font).color(color)).wrap())
}

// ---- Icons ---------------------------------------------------------------------------

/// Filled Phosphor glyph centered at `center`. Glyphs whose fill weight is
/// an enclosed shape (plus in a square, dots in a pill…) use the bold
/// outline instead, which is what the mocks show.
pub fn icon(painter: &Painter, center: Pos2, glyph: &str, size: f32, color: Color32) {
    let font = if ENCLOSED.contains(&glyph) {
        theme::icon_bold(size)
    } else {
        theme::icon_fill(size)
    };
    painter.text(center, Align2::CENTER_CENTER, glyph, font, color);
}

const ENCLOSED: [&str; 24] = [
    ph::WAVEFORM,
    ph::PLUS,
    ph::X,
    ph::CHECK,
    ph::CHECKS,
    ph::DOTS_THREE,
    ph::DOTS_THREE_VERTICAL,
    ph::LIST_BULLETS,
    ph::LIST_CHECKS,
    ph::LIST,
    ph::LIST_DASHES,
    ph::LINK,
    ph::LINK_SIMPLE,
    ph::HASH,
    ph::TEXT_AA,
    ph::GIF,
    ph::CODE,
    ph::ARROW_ELBOW_DOWN_RIGHT,
    ph::SELECTION_ALL,
    ph::CARET_RIGHT,
    ph::CARET_DOWN,
    ph::CARET_LEFT,
    ph::CARET_UP,
    ph::ARROW_UP_RIGHT,
];

/// Outline Phosphor glyph centered at `center`.
pub fn icon_o(painter: &Painter, center: Pos2, glyph: &str, size: f32, color: Color32) {
    painter.text(
        center,
        Align2::CENTER_CENTER,
        glyph,
        theme::icon_line(size),
        color,
    );
}

/// Tinted circular icon bubble (metric cards, AI cards, list icons).
pub fn paint_bubble(painter: &Painter, center: Pos2, glyph: &str, tint: Tint, diameter: f32) {
    painter.circle_filled(center, diameter * 0.5, tint.bg);
    icon(painter, center, glyph, diameter * 0.5, tint.fg);
}

pub fn bubble(ui: &mut Ui, glyph: &str, tint: Tint, diameter: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(diameter, diameter), Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_bubble(ui.painter(), rect.center(), glyph, tint, diameter);
    }
    response
}

/// Rounded-square tile with a gradient (servers, collections, lists).
pub fn paint_tile(painter: &Painter, rect: Rect, glyph: &str, tint: Tint, radius: f32) {
    theme::gradient_rect(
        painter,
        rect,
        radius,
        theme::lerp(tint.fg, Color32::WHITE, 0.08),
        theme::lerp(tint.fg, Color32::BLACK, 0.35),
    );
    icon(
        painter,
        rect.center(),
        glyph,
        rect.height() * 0.5,
        Color32::WHITE,
    );
}

/// Rounded-square tile with initials (guilds without an icon).
pub fn paint_initials_tile(painter: &Painter, rect: Rect, name: &str, tint: Tint, radius: f32) {
    theme::gradient_rect(
        painter,
        rect,
        radius,
        theme::lerp(tint.fg, Color32::WHITE, 0.05),
        theme::lerp(tint.fg, Color32::BLACK, 0.4),
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        theme::initials(name),
        theme::semibold(rect.height() * 0.36),
        Color32::WHITE,
    );
}

/// Dark circle holding a group glyph (group DMs, channels in lists).
pub fn paint_group(painter: &Painter, center: Pos2, diameter: f32, glyph: &str, fg: Color32) {
    painter.circle_filled(center, diameter * 0.5, RAISED);
    icon(painter, center, glyph, diameter * 0.46, fg);
}

/// Count badge (unread): a filled circle/capsule with a number.
pub fn badge(painter: &Painter, center: Pos2, n: usize, color: Color32) {
    let text = if n > 99 {
        "99+".to_owned()
    } else {
        n.to_string()
    };
    let font = theme::semibold(11.0);
    let w = text_width(painter, &text, font.clone()).max(8.0);
    let size = egui::vec2((w + 9.0).max(19.0), 19.0);
    painter.rect_filled(Rect::from_center_size(center, size), 9.5, color);
    painter.text(center, Align2::CENTER_CENTER, text, font, Color32::WHITE);
}

pub fn dot(painter: &Painter, center: Pos2, radius: f32, color: Color32) {
    painter.circle_filled(center, radius, color);
}

/// Right chevron in secondary color.
pub fn chevron(painter: &Painter, center: Pos2, color: Color32) {
    icon_o(painter, center, ph::CARET_RIGHT, 15.0, color);
}

/// Subtle restrained glow around an accent card (teal Omni / red priority).
pub fn glow(painter: &Painter, rect: Rect, radius: f32, color: Color32) {
    for (i, a) in [(1.0_f32, 0.10_f32), (2.5, 0.06), (4.5, 0.03)] {
        painter.rect_stroke(
            rect.expand(i),
            radius + i,
            Stroke::new(1.5_f32, color.gamma_multiply(a)),
            egui::StrokeKind::Outside,
        );
    }
}

// ---- Surfaces ------------------------------------------------------------------------

pub fn card_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(14))
}

/// A standard card filling the available width.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui)
    })
}

/// Fill and outline for an accent (Omni teal or priority red) card.
pub fn accent_colors(accent: Color32) -> (Color32, Color32) {
    (
        theme::lerp(CARD, accent, 0.07),
        theme::lerp(CARD, accent, 0.34),
    )
}

/// Paint a card background in `rect` (for painter-driven layouts).
pub fn paint_card(painter: &Painter, rect: Rect, hovered: bool) {
    painter.rect(
        rect,
        12.0,
        if hovered {
            theme::lerp(CARD, HOVER, 0.6)
        } else {
            CARD
        },
        Stroke::new(1.0_f32, BORDER),
        egui::StrokeKind::Inside,
    );
}

pub fn paint_accent_card(painter: &Painter, rect: Rect, accent: Color32, hovered: bool) {
    let (fill, stroke) = accent_colors(accent);
    glow(painter, rect, 12.0, accent);
    painter.rect(
        rect,
        12.0,
        if hovered {
            theme::lerp(fill, accent, 0.05)
        } else {
            fill
        },
        Stroke::new(1.0_f32, stroke),
        egui::StrokeKind::Inside,
    );
}

/// Hairline divider across the available width.
pub fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, DIVIDER);
}

/// Allocate a full-width clickable row and paint its hover/selected state.
pub fn row(ui: &mut Ui, height: f32, selected: bool) -> (Rect, Response) {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::click());
    if ui.is_rect_visible(rect) {
        if selected {
            ui.painter().rect_filled(rect, 10.0, SELECTED);
        } else if response.hovered() {
            ui.painter()
                .rect_filled(rect, 10.0, theme::lerp(HOVER, SIDEBAR, 0.3));
        }
    }
    (rect, response)
}

// ---- Headings ---------------------------------------------------------------------------

/// Destination title (28px) plus subtitle (A02 "Home / Good afternoon").
pub fn page_title(ui: &mut Ui, title: &str, subtitle: Option<&str>) {
    label(ui, title, theme::semibold(27.0), TEXT);
    if let Some(sub) = subtitle {
        ui.add_space(-4.0);
        label(
            ui,
            sub,
            theme::regular(16.0),
            theme::lerp(SECONDARY, PRIMARY_TEXT, 0.25),
        );
    }
    ui.add_space(6.0);
}

/// Sidebar title (22px) with an optional right icon action (compose, add).
pub fn sidebar_title(ui: &mut Ui, title: &str, action: Option<(&str, &str)>) -> bool {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::hover());
    ui.painter().text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        title,
        theme::semibold(22.0),
        TEXT,
    );
    let mut clicked = false;
    if let Some((glyph, tip)) = action {
        let r = Rect::from_center_size(
            rect.right_center() - egui::vec2(14.0, 0.0),
            egui::vec2(32.0, 32.0),
        );
        let resp = ui.interact(r, ui.id().with(("sidebar_action", title)), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(r, 8.0, HOVER);
        }
        icon_o(ui.painter(), r.center(), glyph, 21.0, SECONDARY);
        let tip = tip.to_owned();
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &tip));
        clicked = resp.on_hover_text(tip.clone()).clicked();
    }
    clicked
}

/// Section heading (17px) with an optional count and "See all"-style link.
/// Returns whether the link was clicked.
pub fn section(ui: &mut Ui, title: &str, count: Option<usize>, link: Option<&str>) -> bool {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), Sense::hover());
    let painter = ui.painter();
    let t = text_at(
        painter,
        rect.left_center(),
        Align2::LEFT_CENTER,
        title,
        theme::semibold(17.0),
        TEXT,
        rect.width() - 80.0,
    );
    if let Some(n) = count {
        painter.text(
            egui::pos2(
                if link.is_some() {
                    t.right() + 10.0
                } else {
                    rect.right() - 4.0
                },
                rect.center().y,
            ),
            if link.is_some() {
                Align2::LEFT_CENTER
            } else {
                Align2::RIGHT_CENTER
            },
            n.to_string(),
            theme::medium(14.0),
            MUTED,
        );
    }
    let mut clicked = false;
    if let Some(l) = link {
        let w = text_width(painter, l, theme::regular(13.0));
        let r = Rect::from_min_max(
            egui::pos2(rect.right() - w - 4.0, rect.top()),
            rect.right_bottom(),
        );
        let resp = ui.interact(r, ui.id().with(("section_link", title)), Sense::click());
        ui.painter().text(
            egui::pos2(rect.right() - 2.0, rect.center().y),
            Align2::RIGHT_CENTER,
            l,
            theme::regular(13.0),
            if resp.hovered() { PRIMARY_TEXT } else { MUTED },
        );
        let l = l.to_owned();
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, true, &l));
        clicked = resp.clicked();
    }
    clicked
}

/// Small uppercase group label inside sidebars ("APP SETTINGS").
pub fn group_label(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    label(ui, text, theme::medium(15.0), SECONDARY);
    ui.add_space(2.0);
}

/// Honest empty state with an optional glyph.
pub fn empty(ui: &mut Ui, glyph: Option<&str>, title: &str, body: &str) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if let Some(g) = glyph {
            bubble(ui, g, GREY, 34.0);
            ui.add_space(4.0);
        }
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            label(ui, title, theme::medium(14.0), SECONDARY);
            para(ui, body, theme::regular(13.0), MUTED);
        });
    });
    ui.add_space(10.0);
}

// ---- Inputs ------------------------------------------------------------------------------

/// Rounded search field with a magnifier (A01 sidebars): 36px tall.
pub fn search(ui: &mut Ui, text: &mut String, hint: &str) -> Response {
    search_sized(ui, text, hint, 36.0)
}

pub fn search_sized(ui: &mut Ui, text: &mut String, hint: &str, height: f32) -> Response {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    let id = ui.id().with(("search", hint));
    let focused = ui.memory(|m| m.has_focus(id));
    ui.painter().rect(
        rect,
        9.0,
        FIELD,
        Stroke::new(
            1.0_f32,
            if focused {
                PRIMARY.gamma_multiply(0.7)
            } else {
                BORDER
            },
        ),
        egui::StrokeKind::Inside,
    );
    icon_o(
        ui.painter(),
        rect.left_center() + egui::vec2(20.0, 0.0),
        ph::MAGNIFYING_GLASS,
        17.0,
        MUTED,
    );
    let inner = Rect::from_min_max(
        rect.left_top() + egui::vec2(38.0, 0.0),
        rect.right_bottom() - egui::vec2(10.0, 0.0),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.add(
        egui::TextEdit::singleline(text)
            .id(id)
            .frame(egui::Frame::NONE)
            .font(theme::regular(14.0))
            .text_color(TEXT)
            .hint_text(
                egui::RichText::new(hint)
                    .font(theme::regular(14.0))
                    .color(MUTED),
            )
            .desired_width(inner.width()),
    )
}

/// A framed single-line input matching the search field (36px, field fill,
/// blue edge when focused). `password` hides the text.
pub fn field(
    ui: &mut Ui,
    text: &mut String,
    hint: &str,
    password: bool,
    salt: impl std::hash::Hash,
) -> Response {
    let (rect, frame) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), Sense::click());
    let id = ui.id().with(("field", salt));
    if frame.clicked() {
        // The whole frame focuses the text, not just the line of glyphs.
        ui.memory_mut(|m| m.request_focus(id));
    }
    let focused = ui.memory(|m| m.has_focus(id));
    ui.painter().rect(
        rect,
        9.0,
        FIELD,
        Stroke::new(
            1.0_f32,
            if focused {
                PRIMARY.gamma_multiply(0.7)
            } else {
                BORDER
            },
        ),
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink2(egui::vec2(12.0, 0.0));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.add(
        egui::TextEdit::singleline(text)
            .id(id)
            .password(password)
            .frame(egui::Frame::NONE)
            .font(theme::regular(14.0))
            .text_color(TEXT)
            .hint_text(
                egui::RichText::new(hint)
                    .font(theme::regular(14.0))
                    .color(MUTED),
            )
            .desired_width(inner.width()),
    )
}

// ---- Selection controls ---------------------------------------------------------------------

/// A pill tab (A01 "All / Unread 3 / Groups / DMs").
pub fn pill(ui: &mut Ui, label: &str, badge_n: Option<usize>, selected: bool) -> Response {
    let font = theme::medium(14.0);
    let text_w = text_width(ui.painter(), label, font.clone());
    let badge_w = if badge_n.is_some_and(|n| n > 0) {
        26.0
    } else {
        0.0
    };
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(text_w + 28.0 + badge_w, 32.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = if selected {
            SELECTED_SOFT
        } else if response.hovered() {
            theme::lerp(HOVER, SIDEBAR, 0.4)
        } else {
            Color32::TRANSPARENT
        };
        painter.rect_filled(rect, 9.0, fill);
        painter.text(
            rect.left_center() + egui::vec2(14.0, 0.0),
            Align2::LEFT_CENTER,
            label,
            font,
            if selected { TEXT } else { SECONDARY },
        );
        if let Some(n) = badge_n.filter(|n| *n > 0) {
            badge(
                painter,
                egui::pos2(rect.right() - 22.0, rect.center().y),
                n,
                PRIMARY,
            );
        }
    }
    let label = label.to_owned();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label)
    });
    response
}

/// Row of pill tabs that wraps instead of widening the panel.
pub fn pill_row(ui: &mut Ui, items: &[(&str, Option<usize>)], selected: &mut usize) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 6.0);
        for (i, (l, b)) in items.iter().enumerate() {
            if pill(ui, l, *b, *selected == i).clicked() && *selected != i {
                *selected = i;
                changed = true;
            }
        }
    });
    changed
}

/// A bordered filter chip; blue when selected (A02 "All / Messages / …").
pub fn filter_chip(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let font = theme::medium(13.0);
    let w = text_width(ui.painter(), label, font.clone()) + 26.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(w, 28.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let (fill, stroke, color) = if selected {
            (PRIMARY, PRIMARY, Color32::WHITE)
        } else if response.hovered() {
            (HOVER, BORDER, TEXT)
        } else {
            (theme::lerp(CARD, RAISED, 0.5), BORDER, SECONDARY)
        };
        ui.painter().rect(
            rect,
            7.0,
            fill,
            Stroke::new(1.0_f32, stroke),
            egui::StrokeKind::Inside,
        );
        ui.painter()
            .text(rect.center(), Align2::CENTER_CENTER, label, font, color);
    }
    let label = label.to_owned();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label)
    });
    response
}

/// Row of filter chips; returns whether the selection changed.
pub fn chip_row(ui: &mut Ui, labels: &[&str], selected: &mut usize) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (i, l) in labels.iter().enumerate() {
            if filter_chip(ui, l, *selected == i).clicked() && *selected != i {
                *selected = i;
                changed = true;
            }
        }
    });
    changed
}

/// Underlined tabs (A11 "Today / Upcoming / Waiting / Completed").
pub fn tab_row(ui: &mut Ui, labels: &[&str], selected: &mut usize) -> bool {
    let mut changed = false;
    let (full, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::hover());
    ui.painter().rect_filled(
        Rect::from_min_max(
            egui::pos2(full.left(), full.bottom() - 1.0),
            full.right_bottom(),
        ),
        0.0,
        DIVIDER,
    );
    let mut x = full.left();
    for (i, l) in labels.iter().enumerate() {
        let font = theme::medium(15.0);
        let w = text_width(ui.painter(), l, font.clone()) + 40.0;
        let r = Rect::from_min_size(egui::pos2(x, full.top()), egui::vec2(w, full.height()));
        let resp = ui.interact(r, ui.id().with(("tab", l)), Sense::click());
        let sel = *selected == i;
        ui.painter().text(
            r.center(),
            Align2::CENTER_CENTER,
            *l,
            font,
            if sel || resp.hovered() {
                TEXT
            } else {
                SECONDARY
            },
        );
        if sel {
            ui.painter().rect_filled(
                Rect::from_min_max(
                    egui::pos2(r.left() + 4.0, r.bottom() - 3.0),
                    egui::pos2(r.right() - 4.0, r.bottom()),
                ),
                1.5,
                PRIMARY,
            );
        }
        let label = l.to_string();
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, sel, &label)
        });
        if resp.clicked() && !sel {
            *selected = i;
            changed = true;
        }
        x += w;
    }
    changed
}

/// Sidebar item with optional tinted glyph and trailing count (A11/A12).
pub fn side_item(
    ui: &mut Ui,
    glyph: Option<(&str, Color32)>,
    label: &str,
    count: Option<usize>,
    selected: bool,
) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 40.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, 9.0, SELECTED);
            painter.rect_filled(
                Rect::from_min_size(
                    rect.left_top() + egui::vec2(0.0, 8.0),
                    egui::vec2(3.0, rect.height() - 16.0),
                ),
                1.5,
                PRIMARY,
            );
        } else if response.hovered() {
            painter.rect_filled(rect, 9.0, theme::lerp(HOVER, SIDEBAR, 0.3));
        }
        let mut x = rect.left() + 14.0;
        if let Some((g, color)) = glyph {
            icon(
                painter,
                egui::pos2(x + 10.0, rect.center().y),
                g,
                20.0,
                color,
            );
            x += 34.0;
        }
        let count_w = if count.is_some() { 36.0 } else { 0.0 };
        text_at(
            painter,
            egui::pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            theme::medium(15.0),
            if selected {
                TEXT
            } else {
                theme::lerp(TEXT, SECONDARY, 0.35)
            },
            rect.right() - x - count_w - 8.0,
        );
        if let Some(n) = count {
            painter.text(
                rect.right_center() - egui::vec2(14.0, 0.0),
                Align2::RIGHT_CENTER,
                n.to_string(),
                theme::medium(14.0),
                if selected { TEXT } else { MUTED },
            );
        }
    }
    let label = label.to_owned();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label)
    });
    response
}

// ---- Buttons ------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Solid blue.
    Primary,
    /// Dark fill with outline.
    Secondary,
    /// Transparent until hovered.
    Ghost,
    /// Teal outline (Omni actions).
    Omni,
    /// Red outline (priority actions).
    Danger,
}

pub fn button_ex(
    ui: &mut Ui,
    kind: Kind,
    glyph: Option<&str>,
    label: &str,
    height: f32,
    enabled: bool,
) -> Response {
    let font = theme::medium(if height >= 34.0 { 14.0 } else { 13.0 });
    let text_w = text_width(ui.painter(), label, font.clone());
    let icon_w = if glyph.is_some() { 24.0 } else { 0.0 };
    let pad = if height >= 34.0 { 16.0 } else { 12.0 };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(text_w + icon_w + pad * 2.0, height),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    paint_button(ui, rect, &response, kind, glyph, label, font, enabled);
    let l = label.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, &l));
    response
}

/// A button stretched to `width`.
pub fn button_wide(
    ui: &mut Ui,
    kind: Kind,
    glyph: Option<&str>,
    label: &str,
    width: f32,
    height: f32,
) -> Response {
    let font = theme::medium(14.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), Sense::click());
    paint_button(ui, rect, &response, kind, glyph, label, font, true);
    let l = label.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &l));
    response
}

#[allow(clippy::too_many_arguments)]
fn paint_button(
    ui: &Ui,
    rect: Rect,
    response: &Response,
    kind: Kind,
    glyph: Option<&str>,
    label: &str,
    font: FontId,
    enabled: bool,
) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let hovered = enabled && response.hovered();
    let (fill, stroke, fg) = match kind {
        Kind::Primary => (
            if hovered { PRIMARY_HOVER } else { PRIMARY },
            Color32::TRANSPARENT,
            Color32::WHITE,
        ),
        Kind::Secondary => (
            if hovered {
                HOVER
            } else {
                theme::lerp(CARD, RAISED, 0.6)
            },
            BORDER,
            TEXT,
        ),
        Kind::Ghost => (
            if hovered { HOVER } else { Color32::TRANSPARENT },
            Color32::TRANSPARENT,
            SECONDARY,
        ),
        Kind::Omni => {
            let (f, s) = accent_colors(OMNI);
            (
                if hovered {
                    theme::lerp(f, OMNI, 0.12)
                } else {
                    f
                },
                s,
                OMNI_TEXT,
            )
        }
        Kind::Danger => {
            let (f, s) = accent_colors(PRIORITY);
            (
                if hovered {
                    theme::lerp(f, PRIORITY, 0.12)
                } else {
                    theme::lerp(f, PRIORITY, 0.05)
                },
                s,
                PRIORITY_TEXT,
            )
        }
    };
    let (fill, fg) = if enabled {
        (fill, fg)
    } else {
        (fill.gamma_multiply(0.55), FAINT)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        8.0,
        fill,
        Stroke::new(1.0_f32, stroke),
        egui::StrokeKind::Inside,
    );
    let content_w =
        text_width(painter, label, font.clone()) + if glyph.is_some() { 24.0 } else { 0.0 };
    let mut x = rect.center().x - content_w * 0.5;
    if let Some(g) = glyph {
        icon(painter, egui::pos2(x + 8.0, rect.center().y), g, 16.0, fg);
        x += 24.0;
    }
    painter.text(
        egui::pos2(x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        fg,
    );
}

pub fn icon_button_ex(
    ui: &mut Ui,
    glyph: &str,
    tooltip: &str,
    size: f32,
    color: Color32,
    enabled: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(size, size),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        if enabled && response.hovered() {
            ui.painter().rect_filled(rect, 8.0, HOVER);
        }
        icon(
            ui.painter(),
            rect.center(),
            glyph,
            size * 0.62,
            if enabled { color } else { FAINT },
        );
    }
    let t = tooltip.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, &t));
    response.on_hover_text(tooltip)
}

/// Small round icon button on a raised disc (row actions in "Online now").
pub fn disc_button(ui: &mut Ui, glyph: &str, tooltip: &str, diameter: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(diameter, diameter), Sense::click());
    if ui.is_rect_visible(rect) {
        ui.painter().circle_filled(
            rect.center(),
            diameter * 0.5,
            if response.hovered() { HOVER } else { RAISED },
        );
        icon(
            ui.painter(),
            rect.center(),
            glyph,
            diameter * 0.5,
            theme::lerp(TEXT, SECONDARY, 0.3),
        );
    }
    let t = tooltip.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &t));
    response.on_hover_text(tooltip)
}

/// Round action with caption (A01 inspector: Message / Voice / Video / More).
pub fn round_action(ui: &mut Ui, glyph: &str, label: &str, width: f32, enabled: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, 72.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let c = egui::pos2(rect.center().x, rect.top() + 22.0);
        painter.circle_filled(
            c,
            22.0,
            if enabled && response.hovered() {
                HOVER
            } else {
                RAISED
            },
        );
        icon(
            painter,
            c,
            glyph,
            21.0,
            if enabled {
                theme::lerp(TEXT, SECONDARY, 0.2)
            } else {
                FAINT
            },
        );
        text_at(
            painter,
            egui::pos2(rect.center().x, rect.bottom() - 6.0),
            Align2::CENTER_BOTTOM,
            label,
            theme::regular(13.0),
            if enabled { SECONDARY } else { FAINT },
            rect.width() + 8.0,
        );
    }
    let l = label.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, &l));
    response
}

/// Text link ("See all", "Edit").
pub fn link(ui: &mut Ui, text: &str) -> Response {
    let resp = ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::regular(13.0))
                .color(PRIMARY_TEXT),
        )
        .sense(Sense::click()),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

// ---- Data display ---------------------------------------------------------------------------

/// Tinted rounded label ("Linked", "High priority", "Important").
pub fn status_pill(ui: &mut Ui, glyph: Option<&str>, text: &str, color: Color32) -> Response {
    let font = theme::medium(13.0);
    let w =
        text_width(ui.painter(), text, font.clone()) + if glyph.is_some() { 38.0 } else { 20.0 };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(w, 26.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        paint_status_pill(ui.painter(), rect, glyph, text, color);
    }
    response
}

pub fn paint_status_pill(
    painter: &Painter,
    rect: Rect,
    glyph: Option<&str>,
    text: &str,
    color: Color32,
) {
    let fg = theme::chip_text(color);
    painter.rect(
        rect,
        7.0,
        theme::lerp(CARD, color, 0.14),
        Stroke::new(1.0_f32, theme::lerp(CARD, color, 0.32)),
        egui::StrokeKind::Inside,
    );
    let mut x = rect.left() + 10.0;
    if let Some(g) = glyph {
        icon(painter, egui::pos2(x + 7.0, rect.center().y), g, 14.0, fg);
        x += 18.0;
    }
    painter.text(
        egui::pos2(x, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        theme::medium(13.0),
        fg,
    );
}

/// Tag chip with a small tinted glyph (A11 "Pixel Heights", "Design").
pub fn tag(ui: &mut Ui, glyph: &str, text: &str, tint: Tint) -> Response {
    let font = theme::regular(13.0);
    let w = text_width(ui.painter(), text, font.clone()) + 38.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(w, 28.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.rect(
            rect,
            7.0,
            theme::lerp(CARD, RAISED, 0.7),
            Stroke::new(1.0_f32, BORDER),
            egui::StrokeKind::Inside,
        );
        icon(
            painter,
            egui::pos2(rect.left() + 15.0, rect.center().y),
            glyph,
            14.0,
            tint.fg,
        );
        painter.text(
            egui::pos2(rect.left() + 28.0, rect.center().y),
            Align2::LEFT_CENTER,
            text,
            font,
            theme::lerp(TEXT, SECONDARY, 0.3),
        );
    }
    response
}

/// Toggle switch (A12 "Show avatars"). Returns the response; flips `on`.
pub fn toggle(ui: &mut Ui, on: &mut bool, enabled: bool) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(
        egui::vec2(46.0, 26.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if enabled && response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let fill = theme::lerp(
            theme::lerp(RAISED, BORDER, 0.5),
            theme::lerp(SUCCESS, GREEN.fg, 0.4),
            t,
        );
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            13.0,
            if enabled {
                fill
            } else {
                fill.gamma_multiply(0.5)
            },
        );
        let x = egui::lerp(rect.left() + 13.0..=rect.right() - 13.0, t);
        painter.circle_filled(egui::pos2(x, rect.center().y), 10.0, Color32::WHITE);
    }
    let checked = *on;
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, checked, "")
    });
    response
}

/// Square checkbox (A11 task rows). Returns the response (click = toggle).
pub fn checkbox(ui: &mut Ui, checked: bool, enabled: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(22.0, 22.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if checked {
            painter.rect_filled(rect, 5.0, theme::lerp(SUCCESS, GREEN.fg, 0.5));
            icon(painter, rect.center(), ph::CHECK, 14.0, Color32::WHITE);
        } else {
            painter.rect_stroke(
                rect.shrink(1.0),
                5.0,
                Stroke::new(1.6_f32, if response.hovered() { SECONDARY } else { MUTED }),
                egui::StrokeKind::Inside,
            );
        }
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, enabled, checked, "")
    });
    response
}

/// Thin progress bar.
pub fn progress(ui: &mut Ui, fraction: f32, color: Color32, height: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, height * 0.5, RAISED);
    let w = rect.width() * fraction.clamp(0.0, 1.0);
    if w > 0.5 {
        painter.rect_filled(
            Rect::from_min_size(rect.min, egui::vec2(w.max(height), height)),
            height * 0.5,
            color,
        );
    }
}

/// Number of equal columns of at least `min_width` that fit in `width`.
pub fn columns_for(width: f32, min_width: f32, gap: f32, max: usize) -> usize {
    (((width + gap) / (min_width + gap)).floor() as usize).clamp(1, max.max(1))
}

/// A painter-driven list row: avatar area + title/subtitle + trailing text.
pub struct RowSpec<'a> {
    pub title: &'a str,
    pub subtitle: Option<&'a str>,
    pub trailing: Option<&'a str>,
    pub title_color: Color32,
    pub subtitle_color: Color32,
    pub leading: f32,
}

/// Paint the text part of a row whose leading visual occupies `spec.leading`
/// pixels; returns the text column rect.
pub fn paint_row_text(painter: &Painter, rect: Rect, spec: &RowSpec<'_>) -> Rect {
    let x = rect.left() + spec.leading;
    let trailing_w = spec
        .trailing
        .map_or(0.0, |t| text_width(painter, t, theme::regular(13.0)) + 10.0);
    let text_w = (rect.right() - x - trailing_w - 10.0).max(20.0);
    let (ty, sy) = if spec.subtitle.is_some() {
        (rect.center().y - 11.0, rect.center().y + 11.0)
    } else {
        (rect.center().y, rect.center().y)
    };
    text_at(
        painter,
        egui::pos2(x, ty),
        Align2::LEFT_CENTER,
        spec.title,
        theme::medium(15.0),
        spec.title_color,
        text_w,
    );
    if let Some(s) = spec.subtitle {
        text_at(
            painter,
            egui::pos2(x, sy),
            Align2::LEFT_CENTER,
            s,
            theme::regular(13.5),
            spec.subtitle_color,
            text_w + trailing_w - 10.0,
        );
    }
    if let Some(t) = spec.trailing {
        painter.text(
            egui::pos2(rect.right() - 10.0, ty),
            Align2::RIGHT_CENTER,
            t,
            theme::regular(13.0),
            MUTED,
        );
    }
    Rect::from_min_max(
        egui::pos2(x, rect.top()),
        egui::pos2(x + text_w, rect.bottom()),
    )
}

// ---- Painter-driven card grids --------------------------------------------------------

/// Lay out `n` fixed-height cards in rows of equal width and call `paint`
/// with each card's rect. Nothing can overflow: widths come from the
/// available width, heights are fixed.
#[allow(clippy::too_many_arguments)]
pub fn card_grid(
    ui: &mut Ui,
    n: usize,
    height: f32,
    gap: f32,
    min_width: f32,
    max_cols: usize,
    mut paint: impl FnMut(&mut Ui, usize, Rect),
) {
    if n == 0 {
        return;
    }
    let width = ui.available_width();
    let cols = columns_for(width, min_width, gap, max_cols).min(n.max(1));
    let cell = (width - gap * (cols as f32 - 1.0)) / cols as f32;
    let rows = n.div_ceil(cols);
    let (area, _) = ui.allocate_exact_size(
        egui::vec2(width, rows as f32 * height + (rows as f32 - 1.0) * gap),
        Sense::hover(),
    );
    for i in 0..n {
        let (r, c) = (i / cols, i % cols);
        let rect = Rect::from_min_size(
            area.min + egui::vec2(c as f32 * (cell + gap), r as f32 * (height + gap)),
            egui::vec2(cell, height),
        );
        paint(ui, i, rect);
    }
}

/// Width a painted button needs.
pub fn button_width(painter: &Painter, glyph: Option<&str>, label: &str, height: f32) -> f32 {
    let font = theme::medium(if height >= 34.0 { 14.0 } else { 13.0 });
    let pad = if height >= 34.0 { 16.0 } else { 12.0 };
    text_width(painter, label, font) + if glyph.is_some() { 24.0 } else { 0.0 } + pad * 2.0
}

/// A button painted at an explicit rect (inside painter-driven cards).
#[allow(clippy::too_many_arguments)]
pub fn button_at(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    kind: Kind,
    glyph: Option<&str>,
    label: &str,
    enabled: bool,
) -> Response {
    let response = ui.interact(
        rect,
        id,
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let font = theme::medium(if rect.height() >= 34.0 { 14.0 } else { 13.0 });
    paint_button(ui, rect, &response, kind, glyph, label, font, enabled);
    let l = label.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, &l));
    response
}

/// An icon button painted at an explicit rect.
pub fn icon_button_at(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    glyph: &str,
    tooltip: &str,
    color: Color32,
) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 8.0, HOVER);
    }
    icon(
        ui.painter(),
        rect.center(),
        glyph,
        rect.height() * 0.55,
        color,
    );
    let t = tooltip.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &t));
    response.on_hover_text(tooltip)
}

/// Round disc icon button painted at an explicit center.
pub fn disc_at(
    ui: &mut Ui,
    center: Pos2,
    diameter: f32,
    id: egui::Id,
    glyph: &str,
    tooltip: &str,
) -> Response {
    let rect = Rect::from_center_size(center, egui::vec2(diameter, diameter));
    let response = ui.interact(rect, id, Sense::click());
    ui.painter().circle_filled(
        center,
        diameter * 0.5,
        if response.hovered() { HOVER } else { RAISED },
    );
    icon(
        ui.painter(),
        center,
        glyph,
        diameter * 0.48,
        theme::lerp(TEXT, SECONDARY, 0.3),
    );
    let t = tooltip.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &t));
    response.on_hover_text(tooltip)
}

/// A clickable card area painted at `rect`; returns the response.
pub fn card_at(ui: &mut Ui, rect: Rect, id: egui::Id, accent: Option<Color32>) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    match accent {
        Some(a) => paint_accent_card(ui.painter(), rect, a, response.hovered()),
        None => paint_card(ui.painter(), rect, response.hovered()),
    }
    response
}

/// Row inside a card: leading glyph, text, trailing chevron. Returns the
/// response of the whole row.
pub fn list_line(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    glyph: &str,
    glyph_color: Color32,
    text: &str,
    text_color: Color32,
) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect.expand2(egui::vec2(6.0, 0.0)),
            6.0,
            theme::lerp(CARD, HOVER, 0.7),
        );
    }
    let painter = ui.painter();
    icon(
        painter,
        rect.left_center() + egui::vec2(10.0, 0.0),
        glyph,
        17.0,
        glyph_color,
    );
    text_at(
        painter,
        rect.left_center() + egui::vec2(30.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        theme::regular(14.0),
        text_color,
        rect.width() - 56.0,
    );
    chevron(painter, rect.right_center() - egui::vec2(8.0, 0.0), MUTED);
    let t = text.to_owned();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &t));
    response
}

/// Metric card painted at `rect` (A02/A03 top rows).
pub fn metric_card_at(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    glyph: &str,
    tint: Tint,
    value: &str,
    caption: &str,
) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    let painter = ui.painter();
    paint_card(painter, rect, response.hovered());
    paint_bubble(
        painter,
        egui::pos2(rect.left() + 34.0, rect.center().y),
        glyph,
        tint,
        46.0,
    );
    painter.text(
        egui::pos2(rect.left() + 68.0, rect.center().y - 1.0),
        Align2::LEFT_BOTTOM,
        value,
        theme::semibold(21.0),
        TEXT,
    );
    let wide = rect.width() >= 205.0;
    text_at(
        painter,
        egui::pos2(rect.left() + 68.0, rect.center().y + 3.0),
        Align2::LEFT_TOP,
        caption,
        theme::regular(13.0),
        MUTED,
        rect.width() - if wide { 96.0 } else { 74.0 },
    );
    if wide {
        chevron(
            painter,
            egui::pos2(rect.right() - 16.0, rect.center().y),
            MUTED,
        );
    }
    let l = format!("{value} {caption}");
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &l));
    response
}
