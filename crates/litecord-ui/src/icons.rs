use eframe::egui::{self, Color32, Painter, Pos2, Stroke};
use litecord_layout::Destination;

/// Paint a small, unfilled destination icon using vector strokes.
pub fn paint(painter: &Painter, center: Pos2, size: f32, destination: Destination, color: Color32) {
    let size = size.max(1.0);
    let stroke = Stroke::new((size * 0.075).clamp(1.4, 2.2), color);

    match destination {
        Destination::Home => {
            outline(
                painter,
                center,
                size,
                &[
                    (0.24, 0.43),
                    (0.50, 0.21),
                    (0.76, 0.43),
                    (0.72, 0.43),
                    (0.72, 0.78),
                    (0.28, 0.78),
                    (0.28, 0.43),
                ],
                stroke,
            );
            line(
                painter,
                center,
                size,
                &[(0.43, 0.78), (0.43, 0.57), (0.57, 0.57), (0.57, 0.78)],
                stroke,
            );
        }
        Destination::Messages => {
            outline(
                painter,
                center,
                size,
                &[
                    (0.23, 0.25),
                    (0.77, 0.25),
                    (0.77, 0.65),
                    (0.50, 0.65),
                    (0.34, 0.79),
                    (0.35, 0.65),
                    (0.23, 0.65),
                ],
                stroke,
            );
            line(painter, center, size, &[(0.34, 0.39), (0.66, 0.39)], stroke);
            line(painter, center, size, &[(0.34, 0.51), (0.57, 0.51)], stroke);
        }
        Destination::Friends => {
            circle(painter, center, size, 0.41, 0.34, 0.095, stroke);
            circle(painter, center, size, 0.67, 0.40, 0.075, stroke);
            arc(
                painter,
                center,
                size,
                (0.41, 0.78),
                (0.25, 0.22),
                (std::f32::consts::PI, 2.0 * std::f32::consts::PI),
                stroke,
            );
            arc(
                painter,
                center,
                size,
                (0.69, 0.78),
                (0.17, 0.17),
                (std::f32::consts::PI, 2.0 * std::f32::consts::PI),
                stroke,
            );
        }
        Destination::Servers => {
            for (x, y) in [(0.35, 0.35), (0.65, 0.35), (0.35, 0.65), (0.65, 0.65)] {
                outline(
                    painter,
                    center,
                    size,
                    &[
                        (x - 0.105, y - 0.105),
                        (x + 0.105, y - 0.105),
                        (x + 0.105, y + 0.105),
                        (x - 0.105, y + 0.105),
                    ],
                    stroke,
                );
            }
        }
        Destination::Voice => {
            line(
                painter,
                center,
                size,
                &[
                    (0.20, 0.52),
                    (0.32, 0.52),
                    (0.40, 0.34),
                    (0.49, 0.68),
                    (0.58, 0.30),
                    (0.67, 0.59),
                    (0.76, 0.44),
                    (0.82, 0.52),
                ],
                stroke,
            );
        }
        Destination::Inbox => {
            outline(
                painter,
                center,
                size,
                &[(0.22, 0.29), (0.78, 0.29), (0.78, 0.72), (0.22, 0.72)],
                stroke,
            );
            line(
                painter,
                center,
                size,
                &[(0.23, 0.34), (0.50, 0.54), (0.77, 0.34)],
                stroke,
            );
            line(
                painter,
                center,
                size,
                &[(0.40, 0.58), (0.45, 0.63), (0.55, 0.63), (0.60, 0.58)],
                stroke,
            );
        }
        Destination::Memory => {
            outline(
                painter,
                center,
                size,
                &[
                    (0.22, 0.27),
                    (0.40, 0.30),
                    (0.50, 0.38),
                    (0.60, 0.30),
                    (0.78, 0.27),
                    (0.78, 0.74),
                    (0.60, 0.77),
                    (0.50, 0.84),
                    (0.40, 0.77),
                    (0.22, 0.74),
                ],
                stroke,
            );
            line(painter, center, size, &[(0.50, 0.38), (0.50, 0.83)], stroke);
            line(painter, center, size, &[(0.29, 0.41), (0.40, 0.43)], stroke);
            line(painter, center, size, &[(0.60, 0.43), (0.71, 0.41)], stroke);
        }
        Destination::Tasks => {
            for y in [0.32, 0.50, 0.68] {
                outline(
                    painter,
                    center,
                    size,
                    &[
                        (0.22, y - 0.06),
                        (0.34, y - 0.06),
                        (0.34, y + 0.06),
                        (0.22, y + 0.06),
                    ],
                    stroke,
                );
                line(painter, center, size, &[(0.43, y), (0.76, y)], stroke);
            }
            line(
                painter,
                center,
                size,
                &[(0.235, 0.32), (0.275, 0.36), (0.33, 0.27)],
                stroke,
            );
        }
        Destination::Settings => {
            let mut gear = Vec::with_capacity(24);
            for index in 0..24 {
                let angle =
                    std::f32::consts::TAU * index as f32 / 24.0 - std::f32::consts::FRAC_PI_2;
                let radius = if index % 3 == 1 { 0.37 } else { 0.29 };
                gear.push((0.5 + angle.cos() * radius, 0.5 + angle.sin() * radius));
            }
            outline(painter, center, size, &gear, stroke);
            circle(painter, center, size, 0.5, 0.5, 0.12, stroke);
        }
    }
}

fn point(center: Pos2, size: f32, x: f32, y: f32) -> Pos2 {
    center + egui::vec2((x - 0.5) * size, (y - 0.5) * size)
}

fn line(painter: &Painter, center: Pos2, size: f32, points: &[(f32, f32)], stroke: Stroke) {
    let points = points
        .iter()
        .map(|&(x, y)| point(center, size, x, y))
        .collect();
    painter.add(egui::Shape::line(points, stroke));
}

fn outline(painter: &Painter, center: Pos2, size: f32, points: &[(f32, f32)], stroke: Stroke) {
    let mut points = points.to_vec();
    if let Some(first) = points.first().copied() {
        points.push(first);
    }
    line(painter, center, size, &points, stroke);
}

fn circle(painter: &Painter, center: Pos2, size: f32, x: f32, y: f32, radius: f32, stroke: Stroke) {
    painter.circle_stroke(point(center, size, x, y), size * radius, stroke);
}

fn arc(
    painter: &Painter,
    center: Pos2,
    size: f32,
    origin: (f32, f32),
    radii: (f32, f32),
    angles: (f32, f32),
    stroke: Stroke,
) {
    const SEGMENTS: usize = 16;
    let points: Vec<_> = (0..=SEGMENTS)
        .map(|index| {
            let angle = angles.0 + (angles.1 - angles.0) * index as f32 / SEGMENTS as f32;
            point(
                center,
                size,
                origin.0 + radii.0 * angle.cos(),
                origin.1 + radii.1 * angle.sin(),
            )
        })
        .collect();
    painter.add(egui::Shape::line(points, stroke));
}
