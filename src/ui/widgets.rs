//! Setting rows shared by both panels. Every row edits one value in place,
//! has a reset button that restores its default, and returns whether the
//! value changed this frame.

use std::ops::RangeInclusive;

use bevy_egui::egui::{
    self, Align, CollapsingHeader, Layout, Response, RichText, Sense, StrokeKind, Ui, WidgetInfo,
    WidgetType, emath, pos2, vec2,
};

use super::theme::palette;

/// A collapsible group of settings with an accent colored title.
pub fn section(ui: &mut Ui, title: &str, contents: impl FnOnce(&mut Ui)) {
    let title = RichText::new(title).strong().color(palette().accent);
    CollapsingHeader::new(title)
        .default_open(true)
        .show_unindented(ui, |ui| {
            ui.add_space(2.0);
            contents(ui);
            ui.add_space(4.0);
        });
}

/// A label and value on one line with a full width slider below.
pub fn slider_row(
    ui: &mut Ui,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    step: f64,
    decimals: usize,
    default: f32,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(palette().subtext1));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            changed |= reset_button(ui, value, default);
            let drag = egui::DragValue::new(value)
                .range(range.clone())
                .speed(step)
                .fixed_decimals(decimals);
            changed |= ui.add(drag).changed();
        });
    });
    ui.spacing_mut().slider_width = ui.available_width();
    let slider = egui::Slider::new(value, range)
        .step_by(step)
        .show_value(false);
    changed |= ui.add(slider).changed();
    ui.add_space(2.0);
    changed
}

/// A label with a toggle switch on the right.
pub fn toggle_row(ui: &mut Ui, label: &str, value: &mut bool, default: bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            changed |= reset_button(ui, value, default);
            changed |= toggle_switch(ui, value).changed();
        });
    });
    changed
}

/// A label above a row of mutually exclusive options.
pub fn choice_row<T: Copy + PartialEq>(
    ui: &mut Ui,
    label: &str,
    value: &mut T,
    options: &[(T, &str)],
    default: T,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(palette().subtext1));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            changed |= reset_button(ui, value, default);
        });
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for &(option, name) in options {
            changed |= ui.selectable_value(value, option, name).changed();
        }
    });
    ui.add_space(2.0);
    changed
}

/// A label with a dropdown on the right.
pub fn dropdown_row<T: Copy + PartialEq>(
    ui: &mut Ui,
    label: &str,
    value: &mut T,
    options: &[(T, &str)],
    default: T,
) -> bool {
    let mut changed = false;
    let selected = options
        .iter()
        .find(|(option, _)| option == value)
        .map_or("", |(_, name)| name);
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            changed |= reset_button(ui, value, default);
            egui::ComboBox::from_id_salt(label)
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    for &(option, name) in options {
                        changed |= ui.selectable_value(value, option, name).changed();
                    }
                });
        });
    });
    changed
}

/// Restores `value` to `default`. Disabled while the value is the default.
fn reset_button<T: Copy + PartialEq>(ui: &mut Ui, value: &mut T, default: T) -> bool {
    let button = egui::Button::new(RichText::new("⟲").color(palette().overlay1)).frame(false);
    let response = ui
        .add_enabled(*value != default, button)
        .on_hover_text("Reset to default");
    if response.clicked() {
        *value = default;
        true
    } else {
        false
    }
}

/// An iOS style switch: a pill shaped track with a sliding knob.
fn toggle_switch(ui: &mut Ui, on: &mut bool) -> Response {
    let height = ui.spacing().interact_size.y - 4.0;
    let (rect, mut response) = ui.allocate_exact_size(vec2(height * 1.8, height), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, ui.is_enabled(), *on, ""));

    if ui.is_rect_visible(rect) {
        let p = palette();
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let radius = rect.height() / 2.0;
        let hovered = response.hovered();
        let track = p.surface0.lerp_to_gamma(p.accent, t);
        let outline = if hovered {
            p.accent
        } else {
            p.surface1.lerp_to_gamma(p.accent, t)
        };
        ui.painter().rect(
            rect,
            radius,
            track,
            egui::Stroke::new(1.0, outline),
            StrokeKind::Inside,
        );
        let x = emath::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        let knob = p.overlay1.lerp_to_gamma(p.crust, t);
        ui.painter()
            .circle_filled(pos2(x, rect.center().y), radius - 3.0, knob);
    }
    response
}
