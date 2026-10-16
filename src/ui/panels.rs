//! The two settings panels, docked full height to the left and right edges.
//! Each widget edits its resource without change detection, and the resource
//! is only marked changed when a widget reports a change, so the systems
//! watching it don't run every frame.

use bevy::pbr::ScreenSpaceAmbientOcclusionQualityLevel as SsaoQuality;
use bevy::prelude::*;
use bevy_egui::egui::{self, Frame, Margin, RichText, ScrollArea, Ui};

use super::widgets::{choice_row, dropdown_row, section, slider_row, toggle_row};
use crate::anti_aliasing::AntiAliasing;
use crate::audio::{CrossoverMode, WooferSettings, limits as woofer_limits};
use crate::lighting::{LightingSettings, limits as lighting_limits};

/// Outer width of each panel, including its margins.
const PANEL_WIDTH: f32 = 320.0;

/// Woofer simulation and audio output, on the left.
pub fn woofer_panel(root: &mut Ui, woofer: &mut ResMut<WooferSettings>) {
    let defaults = WooferSettings::default();
    let w = woofer.bypass_change_detection();
    let mut changed = false;

    side_panel(root, egui::Panel::left("woofer"), "Woofer", |ui| {
        section(ui, "Crossover", |ui| {
            changed |= dropdown_row(
                ui,
                "Filter",
                &mut w.crossover,
                &[
                    (CrossoverMode::Fir, "FIR (linear phase)"),
                    (CrossoverMode::Iir, "IIR (minimum phase)"),
                ],
                defaults.crossover,
            );
            changed |= slider_row(
                ui,
                "Crossover frequency (Hz)",
                &mut w.lowpass_hz,
                woofer_limits::CROSSOVER_HZ,
                1.0,
                0,
                defaults.lowpass_hz,
            );
        });
        section(ui, "Driver", |ui| {
            changed |= slider_row(
                ui,
                "Resonant frequency (Hz)",
                &mut w.resonance_hz,
                woofer_limits::RESONANCE_HZ,
                1.0,
                0,
                defaults.resonance_hz,
            );
        });
        section(ui, "Enclosure", |ui| {
            changed |= toggle_row(ui, "Ported enclosure", &mut w.ported, defaults.ported);
            changed |= slider_row(
                ui,
                "Port tuning (Hz)",
                &mut w.port_hz,
                woofer_limits::PORT_HZ,
                1.0,
                0,
                defaults.port_hz,
            );
            changed |= slider_row(
                ui,
                "Box volume (ft³)",
                &mut w.box_volume_ft3,
                woofer_limits::BOX_VOLUME_FT3,
                0.1,
                1,
                defaults.box_volume_ft3,
            );
            changed |= slider_row(
                ui,
                "Box losses (QL)",
                &mut w.box_ql,
                woofer_limits::BOX_QL,
                0.5,
                1,
                defaults.box_ql,
            );
        });
        section(ui, "Electrical", |ui| {
            changed |= slider_row(
                ui,
                "Amplifier output impedance (Ω)",
                &mut w.amp_ohm,
                woofer_limits::AMP_OHM,
                0.05,
                2,
                defaults.amp_ohm,
            );
            changed |= slider_row(
                ui,
                "Voice coil inductance (mH)",
                &mut w.voice_coil_mh,
                woofer_limits::VOICE_COIL_MH,
                0.05,
                2,
                defaults.voice_coil_mh,
            );
        });
        section(ui, "Subsonic filter", |ui| {
            changed |= toggle_row(ui, "Subsonic highpass", &mut w.highpass, defaults.highpass);
            changed |= slider_row(
                ui,
                "Cutoff (Hz)",
                &mut w.highpass_hz,
                woofer_limits::SUBSONIC_HZ,
                1.0,
                0,
                defaults.highpass_hz,
            );
        });
        section(ui, "Levels", |ui| {
            changed |= slider_row(
                ui,
                "Master gain (dB)",
                &mut w.master_gain_db,
                woofer_limits::GAIN_DB,
                0.5,
                1,
                defaults.master_gain_db,
            );
            changed |= slider_row(
                ui,
                "Output bass mix (dB)",
                &mut w.bass_mix_db,
                woofer_limits::GAIN_DB,
                0.5,
                1,
                defaults.bass_mix_db,
            );
            changed |= slider_row(
                ui,
                "Output treble mix (dB)",
                &mut w.treble_mix_db,
                woofer_limits::GAIN_DB,
                0.5,
                1,
                defaults.treble_mix_db,
            );
        });
        section(ui, "Speaker output", |ui| {
            changed |= toggle_row(
                ui,
                "Play through speakers",
                &mut w.audio_output,
                defaults.audio_output,
            );
            changed |= toggle_row(
                ui,
                "Include treble",
                &mut w.treble_output,
                defaults.treble_output,
            );
        });
    });

    if changed {
        woofer.set_changed();
    }
}

/// Lighting, post processing and ambient occlusion, on the right.
pub fn graphics_panel(
    root: &mut Ui,
    mut lighting: ResMut<LightingSettings>,
    mut anti_aliasing: ResMut<AntiAliasing>,
) {
    let defaults = LightingSettings::default();
    let l = lighting.bypass_change_detection();
    let aa = anti_aliasing.bypass_change_detection();
    let (mut lighting_changed, mut aa_changed) = (false, false);

    side_panel(root, egui::Panel::right("graphics"), "Graphics", |ui| {
        section(ui, "Scene", |ui| {
            lighting_changed |= toggle_row(
                ui,
                "Render scene",
                &mut l.render_scene,
                defaults.render_scene,
            );
        });
        section(ui, "Lighting", |ui| {
            lighting_changed |=
                toggle_row(ui, "Raytracing", &mut l.raytracing, defaults.raytracing);
            lighting_changed |= slider_row(
                ui,
                "HDRI intensity (cd/m²)",
                &mut l.hdri_intensity,
                lighting_limits::HDRI_INTENSITY,
                50.0,
                0,
                defaults.hdri_intensity,
            );
            lighting_changed |= toggle_row(ui, "Key light", &mut l.key_light, defaults.key_light);
            lighting_changed |= slider_row(
                ui,
                "Key light illuminance (lux)",
                &mut l.key_illuminance,
                lighting_limits::KEY_ILLUMINANCE,
                100.0,
                0,
                defaults.key_illuminance,
            );
        });
        section(ui, "Post processing", |ui| {
            aa_changed |= choice_row(
                ui,
                "Anti-aliasing",
                aa,
                &[
                    (AntiAliasing::Off, "Off"),
                    (AntiAliasing::Taa, "TAA"),
                    (AntiAliasing::Fxaa, "FXAA"),
                    (AntiAliasing::Smaa, "SMAA"),
                ],
                AntiAliasing::default(),
            );
            lighting_changed |=
                toggle_row(ui, "Screen space reflections", &mut l.ssr, defaults.ssr);
            lighting_changed |= toggle_row(ui, "Bloom", &mut l.bloom, defaults.bloom);
            lighting_changed |= slider_row(
                ui,
                "Bloom intensity",
                &mut l.bloom_intensity,
                lighting_limits::BLOOM_INTENSITY,
                0.01,
                2,
                defaults.bloom_intensity,
            );
        });
        section(ui, "Ambient occlusion", |ui| {
            let ssao = &defaults.ssao_settings;
            lighting_changed |= toggle_row(ui, "Ambient occlusion", &mut l.ssao, defaults.ssao);
            lighting_changed |= choice_row(
                ui,
                "Quality",
                &mut l.ssao_settings.quality_level,
                &[
                    (SsaoQuality::Low, "Low"),
                    (SsaoQuality::Medium, "Medium"),
                    (SsaoQuality::High, "High"),
                    (SsaoQuality::Ultra, "Ultra"),
                ],
                ssao.quality_level,
            );
            lighting_changed |= slider_row(
                ui,
                "Radius (m)",
                &mut l.ssao_settings.radius,
                lighting_limits::SSAO_RADIUS,
                0.01,
                2,
                ssao.radius,
            );
            lighting_changed |= slider_row(
                ui,
                "Object thickness (m)",
                &mut l.ssao_settings.constant_object_thickness,
                lighting_limits::SSAO_THICKNESS,
                0.01,
                2,
                ssao.constant_object_thickness,
            );
        });
    });

    if lighting_changed {
        lighting.set_changed();
    }
    if aa_changed {
        anti_aliasing.set_changed();
    }
}

/// Horizontal space between the panel edges and the scroll area. There is
/// none vertically, so the scroll area reaches the bottom of the window.
const PANEL_MARGIN_X: i8 = 2;
/// Space around the title and around the contents inside the scroll area.
const PADDING: i8 = 8;

/// A fixed width, full height panel with a title. The contents scroll when
/// they are taller than the window.
fn side_panel(root: &mut Ui, panel: egui::Panel, title: &str, contents: impl FnOnce(&mut Ui)) {
    let frame =
        Frame::side_top_panel(root.style()).inner_margin(Margin::symmetric(PANEL_MARGIN_X, 0));
    // Pads the title like the scrolled contents, so they line up.
    let title_frame = Frame::NONE.inner_margin(PADDING);
    panel
        .frame(frame)
        .exact_size(PANEL_WIDTH)
        .resizable(false)
        .show(root, |ui| {
            // The paddings alone space the title, divider and scroll area.
            let row_spacing = ui.spacing().item_spacing.y;
            ui.spacing_mut().item_spacing.y = 0.0;
            title_frame.show(ui, |ui| {
                ui.label(RichText::new(title).heading().strong());
            });
            ui.separator();
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Frame::NONE.inner_margin(PADDING).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = row_spacing;
                        contents(ui);
                    });
                });
        });
}
