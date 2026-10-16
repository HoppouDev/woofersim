//! Graphs of the current setup over a logarithmic frequency axis, one view at
//! a time:
//!
//! - Response: sound pressure in dB (left axis), cone excursion as a shape
//!   key weight and the phase of the sound pressure in degrees (right axes).
//! - Phase: phase of the sound pressure in degrees (left) and its group
//!   delay in ms (right).
//! - Impedance: electrical impedance in Ω (left) and its phase in degrees
//!   (right).

use bevy_egui::egui::{
    Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2,
    vec2,
};

use super::theme::palette;
use crate::audio::{ResponsePoint, WooferSettings};

const MIN_HZ: f32 = 10.0;
const MAX_HZ: f32 = 500.0;
const POINTS: usize = 240;
/// Frequencies with a grid line, the labelled ones in [`LABELLED_HZ`].
const GRID_HZ: [f32; 13] = [
    10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 80.0, 100.0, 150.0, 200.0, 300.0, 400.0, 500.0,
];
const LABELLED_HZ: [f32; 6] = [10.0, 20.0, 50.0, 100.0, 200.0, 500.0];
const GRID_STEPS: usize = 8;
/// Lowest the response axis goes, its bottom when the level is tiny.
const LEVEL_FLOOR_DB: f32 = -96.0;

const AXIS_WIDTH: f32 = 44.0;
/// Room above the plot for the axis titles.
const TITLE_HEIGHT: f32 = 18.0;
const FREQUENCY_LABEL_HEIGHT: f32 = 18.0;
/// Space between axis titles on the same side.
const TITLE_GAP: f32 = 12.0;
/// Width of every curve.
const CURVE_WIDTH: f32 = 1.5;

#[derive(Clone, Copy, PartialEq, Default)]
pub enum View {
    #[default]
    Response,
    Phase,
    Impedance,
}

/// A vertical axis: its range, gridline spacing and label.
struct Axis {
    title: &'static str,
    min: f32,
    max: f32,
    /// Labels on every nth grid line.
    label_every: usize,
    decimals: usize,
    signed: bool,
}

impl Axis {
    fn y(&self, value: f32, plot: Rect) -> f32 {
        plot.bottom() - (value - self.min) / (self.max - self.min) * plot.height()
    }

    fn label(&self, value: f32) -> String {
        if self.signed {
            format!("{value:+.*}", self.decimals)
        } else {
            format!("{value:.*}", self.decimals)
        }
    }

    /// ±180°, for phases that wrap around.
    fn phase() -> Self {
        Self {
            title: "Phase (°)",
            min: -180.0,
            max: 180.0,
            label_every: 2,
            decimals: 0,
            signed: true,
        }
    }

    /// From 0 up to a round value at or above `peak`, labelled on every
    /// other grid line.
    fn scaled(title: &'static str, peak: f32) -> Self {
        let max = nice_ceiling(peak);
        let label_every = 2;
        Self {
            title,
            min: 0.0,
            max,
            label_every,
            decimals: decimals_for(max / GRID_STEPS as f32 * label_every as f32),
            signed: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Side {
    Left,
    Right,
}

/// One plotted quantity with its axis.
struct Curve {
    axis: Axis,
    values: Vec<f32>,
    side: Side,
    color: Color32,
}

/// The curves for the settings and view they were computed from, so they
/// are only recomputed when either changes.
#[derive(Default)]
pub struct ResponseGraph {
    view: View,
    computed: Option<(WooferSettings, View)>,
    frequencies: Vec<f32>,
    points: Vec<ResponsePoint>,
    curves: Vec<Curve>,
}

impl ResponseGraph {
    fn update(&mut self, settings: &WooferSettings) {
        let wanted = (*settings, self.view);
        if self.computed == Some(wanted) {
            return;
        }
        if self.computed.map(|(computed, _)| computed) != Some(*settings) {
            let ratio = MAX_HZ / MIN_HZ;
            self.frequencies = (0..POINTS)
                .map(|i| MIN_HZ * ratio.powf(i as f32 / (POINTS - 1) as f32))
                .collect();
            self.points = settings.frequency_response(&self.frequencies);
        }
        self.curves = self.curves();
        self.computed = Some(wanted);
    }

    /// The curves of the current view, the main one first. Curves on the
    /// same side get their own label column, counting outwards.
    fn curves(&self) -> Vec<Curve> {
        let p = palette();
        let values = |f: fn(&ResponsePoint) -> f32| self.points.iter().map(f).collect::<Vec<_>>();
        let curve = |axis, values, side, color| Curve {
            axis,
            values,
            side,
            color,
        };
        match self.view {
            View::Response => {
                let level = values(|p| p.level_db);
                let excursion = values(|p| p.excursion);
                // 48 dB tall, its top following the peak in 6 dB steps, down
                // to a floor of -96 dB for very low gains.
                let top = ((max(&level) / 6.0).ceil() * 6.0).max(LEVEL_FLOOR_DB + 48.0);
                let level_axis = Axis {
                    title: "Response (dB)",
                    min: top - 48.0,
                    max: top,
                    label_every: 2,
                    decimals: 0,
                    signed: true,
                };
                let excursion_axis = Axis::scaled("Excursion", max(&excursion).max(1.0));
                vec![
                    curve(level_axis, level, Side::Left, p.accent),
                    curve(excursion_axis, excursion, Side::Right, p.peach),
                    curve(Axis::phase(), values(|p| p.phase_deg), Side::Right, p.teal),
                ]
            }
            View::Phase => {
                let delay = values(|p| p.group_delay_ms);
                let delay_axis = Axis::scaled("Group delay (ms)", max(&delay).max(1.0));
                vec![
                    curve(Axis::phase(), values(|p| p.phase_deg), Side::Left, p.accent),
                    curve(delay_axis, delay, Side::Right, p.peach),
                ]
            }
            View::Impedance => {
                let impedance = values(|p| p.impedance_ohm);
                let impedance_axis = Axis::scaled("Impedance (Ω)", max(&impedance));
                let phase_axis = Axis {
                    min: -90.0,
                    max: 90.0,
                    ..Axis::phase()
                };
                let phase = values(|p| p.impedance_phase_deg);
                vec![
                    curve(impedance_axis, impedance, Side::Left, p.accent),
                    curve(phase_axis, phase, Side::Right, p.peach),
                ]
            }
        }
    }
}

fn max(values: &[f32]) -> f32 {
    values.iter().copied().fold(f32::MIN, f32::max)
}

pub fn show(ui: &mut Ui, graph: &mut ResponseGraph, settings: &WooferSettings) {
    let p = palette();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (view, name) in [
            (View::Response, "Response"),
            (View::Phase, "Phase"),
            (View::Impedance, "Impedance"),
        ] {
            ui.selectable_value(&mut graph.view, view, name);
        }
    });
    ui.add_space(4.0);
    graph.update(settings);
    let curves = &graph.curves;
    let columns = |side| curves.iter().filter(|c| c.side == side).count() as f32;

    let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
    let plot = Rect::from_min_max(
        pos2(
            rect.left() + AXIS_WIDTH * columns(Side::Left),
            rect.top() + TITLE_HEIGHT,
        ),
        pos2(
            rect.right() - AXIS_WIDTH * columns(Side::Right),
            rect.bottom() - FREQUENCY_LABEL_HEIGHT,
        ),
    );
    if plot.width() < 40.0 || plot.height() < 20.0 {
        return;
    }
    let x = |frequency: f32| {
        plot.left() + (frequency / MIN_HZ).log10() / (MAX_HZ / MIN_HZ).log10() * plot.width()
    };

    let painter = ui.painter_at(rect);
    let small = FontId::proportional(11.0);
    painter.rect(
        plot,
        CornerRadius::same(4),
        p.base,
        Stroke::new(1.0, p.surface0),
        StrokeKind::Inside,
    );

    for frequency in GRID_HZ {
        let gx = x(frequency);
        painter.line_segment(
            [pos2(gx, plot.top()), pos2(gx, plot.bottom())],
            Stroke::new(1.0, p.surface0),
        );
        if LABELLED_HZ.contains(&frequency) {
            painter.text(
                pos2(gx, plot.bottom() + 3.0),
                Align2::CENTER_TOP,
                format!("{frequency:.0}"),
                small.clone(),
                p.overlay1,
            );
        }
    }
    painter.text(
        pos2(plot.right(), rect.bottom()),
        Align2::RIGHT_BOTTOM,
        "Hz",
        small.clone(),
        p.overlay1,
    );
    for step in 1..GRID_STEPS {
        let gy = plot.bottom() - step as f32 / GRID_STEPS as f32 * plot.height();
        painter.line_segment(
            [pos2(plot.left(), gy), pos2(plot.right(), gy)],
            Stroke::new(1.0, p.surface0),
        );
    }

    // Label columns count outwards from the plot on each side.
    let mut column = [0.0f32; 2];
    for curve in curves {
        let (edge, anchor, direction) = match curve.side {
            Side::Left => (plot.left(), Align2::RIGHT_CENTER, -1.0),
            Side::Right => (plot.right(), Align2::LEFT_CENTER, 1.0),
        };
        let label_x = edge + direction * (4.0 + column[curve.side as usize] * AXIS_WIDTH);
        for step in (0..=GRID_STEPS).step_by(curve.axis.label_every) {
            let fraction = step as f32 / GRID_STEPS as f32;
            let value = curve.axis.min + fraction * (curve.axis.max - curve.axis.min);
            painter.text(
                pos2(label_x, plot.bottom() - fraction * plot.height()),
                anchor,
                curve.axis.label(value),
                small.clone(),
                curve.color,
            );
        }
        column[curve.side as usize] += 1.0;
    }

    // Titles in a row above the plot, left axes from the left edge and right
    // axes from the right edge, so each sits on the same side as its labels
    // and the outermost column's title is outermost.
    let title = |curve: &Curve| {
        painter.layout_no_wrap(curve.axis.title.to_owned(), small.clone(), curve.color)
    };
    let mut left_x = rect.left();
    for curve in curves.iter().filter(|c| c.side == Side::Left) {
        let galley = title(curve);
        let width = galley.size().x;
        painter.galley(pos2(left_x, rect.top()), galley, curve.color);
        left_x += width + TITLE_GAP;
    }
    let mut right_x = rect.right();
    for curve in curves.iter().rev().filter(|c| c.side == Side::Right) {
        let galley = title(curve);
        right_x -= galley.size().x;
        painter.galley(pos2(right_x, rect.top()), galley, curve.color);
        right_x -= TITLE_GAP;
    }

    let clipped = painter.with_clip_rect(plot);
    // Phase curves wrap at ±180°, so they are split where they jump. The
    // main curve is drawn last, on top.
    for curve in curves.iter().rev() {
        let points: Vec<Pos2> = graph
            .frequencies
            .iter()
            .zip(&curve.values)
            .map(|(&f, &v)| pos2(x(f), curve.axis.y(v, plot)))
            .collect();
        let mut start = 0;
        for i in 1..=points.len() {
            let jump = i < points.len() && (curve.values[i] - curve.values[i - 1]).abs() > 180.0;
            if i == points.len() || jump {
                clipped.add(Shape::line(
                    points[start..i].to_vec(),
                    Stroke::new(CURVE_WIDTH, curve.color),
                ));
                start = i;
            }
        }
    }

    // Readout of every curve at the hovered frequency.
    if let Some(pointer) = response.hover_pos().filter(|pos| plot.contains(*pos))
        && let Some(i) = (0..graph.frequencies.len()).min_by(|&a, &b| {
            (x(graph.frequencies[a]) - pointer.x)
                .abs()
                .total_cmp(&(x(graph.frequencies[b]) - pointer.x).abs())
        })
    {
        let sx = x(graph.frequencies[i]);
        clipped.line_segment(
            [pos2(sx, plot.top()), pos2(sx, plot.bottom())],
            Stroke::new(1.0, p.overlay1),
        );
        let mut text = format!("{:.0} Hz", graph.frequencies[i]);
        for curve in curves {
            let y = curve.axis.y(curve.values[i], plot);
            clipped.circle_filled(pos2(sx, y), 3.0, curve.color);
            text.push_str("   ");
            text.push_str(&readout(&curve.axis, curve.values[i]));
        }
        let on_left = sx < plot.center().x;
        let anchor = if on_left {
            Align2::LEFT_TOP
        } else {
            Align2::RIGHT_TOP
        };
        let offset = if on_left { 6.0 } else { -6.0 };
        let label = painter.layout_no_wrap(text, small, p.text);
        let label_rect = anchor
            .anchor_size(pos2(sx + offset, plot.top() + 6.0), label.size())
            .expand2(vec2(5.0, 3.0));
        painter.rect(
            label_rect,
            CornerRadius::same(4),
            p.crust,
            Stroke::new(1.0, p.surface1),
            StrokeKind::Inside,
        );
        painter.galley(label_rect.shrink2(vec2(5.0, 3.0)).min, label, p.text);
    }
}

/// A value with the unit from its axis title, for the hover readout.
fn readout(axis: &Axis, value: f32) -> String {
    let unit = axis
        .title
        .split_once('(')
        .map_or("", |(_, unit)| unit.trim_end_matches(')'));
    let value = if axis.signed {
        format!("{value:+.1}")
    } else {
        format!("{value:.2}")
    };
    if unit.is_empty() {
        value
    } else {
        format!("{value} {unit}")
    }
}

/// Rounds up to 1, 2 or 5 times a power of ten, for readable axis labels.
fn nice_ceiling(value: f32) -> f32 {
    let magnitude = 10f32.powf(value.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|step| step * magnitude)
        .find(|&candidate| candidate >= value)
        .unwrap_or(10.0 * magnitude)
}

/// Fewest decimals that show `step` exactly, so labels like 1.25 are not
/// rounded to 1.
fn decimals_for(step: f32) -> usize {
    (0..3)
        .find(|&decimals| {
            let scaled = step * 10f32.powi(decimals as i32);
            (scaled - scaled.round()).abs() < 1e-3 * scaled.max(1.0)
        })
        .unwrap_or(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_labels_show_every_labelled_value_exactly() {
        for peak in [0.3, 1.0, 1.5, 3.0, 7.0, 15.0, 40.0, 90.0, 400.0] {
            let axis = Axis::scaled("Test", peak);
            for line in (0..=GRID_STEPS).step_by(axis.label_every) {
                let value = axis.max * line as f32 / GRID_STEPS as f32;
                let shown: f32 = axis.label(value).parse().unwrap();
                assert!(
                    (shown - value).abs() < 1e-4 * axis.max,
                    "{value} shown as {} on a 0 to {} axis",
                    axis.label(value),
                    axis.max
                );
            }
        }
    }
}
