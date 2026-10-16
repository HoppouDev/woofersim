//! Catppuccin Mocha with the mauve accent and the Inter typeface, applied to
//! egui: dark opaque panels, thin 1px outlines and soft shadows on popups.

use std::sync::Arc;

use bevy_egui::egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke,
    Style, TextStyle, Vec2, Visuals,
};
use catppuccin::PALETTE;

const INTER: &str = "Inter";
const INTER_SEMIBOLD: &str = "Inter SemiBold";

/// Makes Inter the proportional font, with egui's bundled fonts kept as
/// fallbacks for symbols Inter lacks, such as the reset arrow. Headings use
/// the SemiBold cut.
pub fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        INTER.into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/Inter-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        INTER_SEMIBOLD.into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/fonts/Inter-SemiBold.ttf"
        ))),
    );
    let fallbacks = fonts.families[&FontFamily::Proportional].clone();
    let family = |primary: &str| {
        std::iter::once(primary.to_owned())
            .chain(fallbacks.iter().cloned())
            .collect::<Vec<_>>()
    };
    fonts
        .families
        .insert(FontFamily::Proportional, family(INTER));
    fonts.families.insert(
        FontFamily::Name(INTER_SEMIBOLD.into()),
        family(INTER_SEMIBOLD),
    );
    fonts
}

/// The Catppuccin Mocha colors the UI uses, converted to egui colors.
pub struct Palette {
    pub accent: Color32,
    pub crust: Color32,
    pub mantle: Color32,
    pub base: Color32,
    pub surface0: Color32,
    pub surface1: Color32,
    pub surface2: Color32,
    pub overlay1: Color32,
    pub subtext1: Color32,
    pub text: Color32,
    pub red: Color32,
    pub peach: Color32,
    pub teal: Color32,
}

pub fn palette() -> Palette {
    let colors = &PALETTE.mocha.colors;
    let rgb = |color: &catppuccin::Color| Color32::from_rgb(color.rgb.r, color.rgb.g, color.rgb.b);
    Palette {
        accent: rgb(&colors.mauve),
        crust: rgb(&colors.crust),
        mantle: rgb(&colors.mantle),
        base: rgb(&colors.base),
        surface0: rgb(&colors.surface0),
        surface1: rgb(&colors.surface1),
        surface2: rgb(&colors.surface2),
        overlay1: rgb(&colors.overlay1),
        subtext1: rgb(&colors.subtext1),
        text: rgb(&colors.text),
        red: rgb(&colors.red),
        peach: rgb(&colors.peach),
        teal: rgb(&colors.teal),
    }
}

pub fn apply(style: &mut Style) {
    let p = palette();
    let hairline = |color| Stroke::new(1.0, color);
    let control_radius = CornerRadius::same(6);

    let mut visuals = Visuals::dark();
    visuals.override_text_color = None;
    visuals.hyperlink_color = p.accent;
    visuals.faint_bg_color = p.mantle;
    visuals.extreme_bg_color = p.crust;
    visuals.code_bg_color = p.mantle;
    visuals.warn_fg_color = p.peach;
    visuals.error_fg_color = p.red;

    visuals.window_fill = p.base;
    visuals.panel_fill = p.mantle;
    visuals.window_stroke = hairline(p.surface1);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.window_shadow = Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(110),
    };
    visuals.window_highlight_topmost = false;
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(90),
    };

    visuals.selection.bg_fill = p.accent.gamma_multiply(0.85);
    visuals.selection.stroke = hairline(p.crust);
    visuals.slider_trailing_fill = true;
    visuals.collapsing_header_frame = false;
    visuals.indent_has_left_vline = false;

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = p.base;
    widgets.noninteractive.weak_bg_fill = p.base;
    widgets.noninteractive.bg_stroke = hairline(p.surface0);
    widgets.noninteractive.fg_stroke = hairline(p.subtext1);
    widgets.noninteractive.corner_radius = control_radius;

    widgets.inactive.bg_fill = p.surface0;
    widgets.inactive.weak_bg_fill = p.surface0;
    widgets.inactive.bg_stroke = hairline(p.surface1);
    widgets.inactive.fg_stroke = hairline(p.text);
    widgets.inactive.corner_radius = control_radius;
    widgets.inactive.expansion = 0.0;

    widgets.hovered.bg_fill = p.surface1;
    widgets.hovered.weak_bg_fill = p.surface1;
    widgets.hovered.bg_stroke = hairline(p.accent.gamma_multiply(0.7));
    widgets.hovered.fg_stroke = hairline(p.text);
    widgets.hovered.corner_radius = control_radius;
    widgets.hovered.expansion = 0.0;

    widgets.active.bg_fill = p.surface2;
    widgets.active.weak_bg_fill = p.surface2;
    widgets.active.bg_stroke = hairline(p.accent);
    widgets.active.fg_stroke = hairline(p.text);
    widgets.active.corner_radius = control_radius;
    widgets.active.expansion = 0.0;

    widgets.open = widgets.active;
    style.visuals = visuals;

    let spacing = &mut style.spacing;
    spacing.item_spacing = Vec2::new(8.0, 6.0);
    spacing.button_padding = Vec2::new(10.0, 4.0);
    spacing.window_margin = Margin::same(14);
    spacing.interact_size.y = 22.0;
    spacing.slider_rail_height = 4.0;

    style.text_styles.iter_mut().for_each(|(text_style, font)| {
        *font = match text_style {
            TextStyle::Heading => FontId::new(16.0, FontFamily::Name(INTER_SEMIBOLD.into())),
            TextStyle::Small => FontId::proportional(11.0),
            TextStyle::Monospace => FontId::monospace(12.5),
            _ => FontId::proportional(13.5),
        };
    });
}
