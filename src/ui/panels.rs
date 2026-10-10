//! Panel layouts and the observers that write widget changes into settings.

use bevy::feathers::controls::{FeathersCheckbox, FeathersRadio, FeathersSlider};
use bevy::feathers::display::{caption, label};
use bevy::feathers::theme::ThemeBackgroundColor;
use bevy::feathers::tokens;
use bevy::input_focus::tab_navigation::TabGroup;
use bevy::pbr::ScreenSpaceAmbientOcclusionQualityLevel as SsaoQuality;
use bevy::prelude::*;
use bevy::ui_widgets::{RadioGroup, SliderPrecision, SliderStep, ValueChange};

use super::settings::{AntiAliasingChoice, BoolSetting, FloatSetting, SsaoQualityChoice};
use crate::anti_aliasing::AntiAliasing;
use crate::lighting::LightingSettings;
use crate::shape_key::SineShapeKey;

/// Lighting and animation, in the top left corner.
pub fn lighting_panel() -> impl Scene {
    bsn! {
        @panel(px(8), Val::Auto)
        Children [
            @checkbox("Raytracing (Solari, ReSTIR)", BoolSetting::Raytracing)
            --
            @slider("HDRI intensity (cd/m²)", FloatSetting::HdriIntensity, 0.0, 5000.0, 50.0, 0)
            --
            @checkbox("Key light", BoolSetting::KeyLight)
            --
            @slider("Key light illuminance (lux)", FloatSetting::KeyIlluminance, 0.0, 50000.0, 100.0, 0)
            --
            @slider("Wave speed (Hz)", FloatSetting::WaveSpeed, 0.0, 60.0, 0.1, 1)
        ]
    }
}

/// Screen space effects, bloom and anti-aliasing, in the top right corner.
pub fn post_process_panel() -> impl Scene {
    bsn! {
        @panel(Val::Auto, px(8))
        Children [
            @label("Anti-aliasing")
            --
            Node {
                flex_direction: FlexDirection::Row,
                column_gap: px(8),
            }
            RadioGroup
            on(select_anti_aliasing)
            Children [
                @FeathersRadio { @caption: bsn! { @caption("Off") } }
                AntiAliasingChoice(AntiAliasing::Off)
                --
                @FeathersRadio { @caption: bsn! { @caption("TAA") } }
                AntiAliasingChoice(AntiAliasing::Taa)
                --
                @FeathersRadio { @caption: bsn! { @caption("FXAA") } }
                AntiAliasingChoice(AntiAliasing::Fxaa)
                --
                @FeathersRadio { @caption: bsn! { @caption("SMAA") } }
                AntiAliasingChoice(AntiAliasing::Smaa)
            ]
            --
            @checkbox("Screen space reflections", BoolSetting::Ssr)
            --
            @checkbox("Ambient occlusion", BoolSetting::Ssao)
            --
            Node {
                flex_direction: FlexDirection::Row,
                column_gap: px(8),
            }
            RadioGroup
            on(select_ssao_quality)
            Children [
                @FeathersRadio { @caption: bsn! { @caption("Low") } }
                SsaoQualityChoice(SsaoQuality::Low)
                --
                @FeathersRadio { @caption: bsn! { @caption("Medium") } }
                SsaoQualityChoice(SsaoQuality::Medium)
                --
                @FeathersRadio { @caption: bsn! { @caption("High") } }
                SsaoQualityChoice(SsaoQuality::High)
                --
                @FeathersRadio { @caption: bsn! { @caption("Ultra") } }
                SsaoQualityChoice(SsaoQuality::Ultra)
            ]
            --
            @slider("AO radius (m)", FloatSetting::SsaoRadius, 0.01, 1.0, 0.01, 2)
            --
            @slider("AO object thickness (m)", FloatSetting::SsaoThickness, 0.01, 0.5, 0.01, 2)
            --
            @checkbox("Bloom", BoolSetting::Bloom)
            --
            @slider("Bloom intensity", FloatSetting::BloomIntensity, 0.0, 1.0, 0.01, 2)
        ]
    }
}

// Media player panel (seek, pause, play, stop etc.)
fn media_player_panel() -> impl Scene {}

fn panel(left: Val, right: Val) -> impl Scene {
    bsn! {
        Node {
            position_type: PositionType::Absolute,
            top: px(8),
            left: left,
            right: right,
            width: px(280),
            flex_direction: FlexDirection::Column,
            row_gap: px(6),
            padding: px(8),
        }
        TabGroup
        ThemeBackgroundColor(tokens::WINDOW_BG)
    }
}

fn checkbox(text: &'static str, setting: BoolSetting) -> impl Scene {
    bsn! {
        @FeathersCheckbox {
            @caption: bsn! { @caption(text) }
        }
        setting
        on(set_toggle)
    }
}

/// A label above a slider, grouped so the panel spacing treats them as one row.
fn slider(
    text: &'static str,
    setting: FloatSetting,
    min: f32,
    max: f32,
    step: f32,
    precision: i32,
) -> impl Scene {
    bsn! {
        Node {
            flex_direction: FlexDirection::Column,
            row_gap: px(2),
        }
        Children [
            @label(text)
            --
            @FeathersSlider { @min: min, @max: max }
            setting
            SliderStep(step)
            SliderPrecision(precision)
            on(set_slider)
        ]
    }
}

fn set_toggle(
    change: On<ValueChange<bool>>,
    toggles: Query<&BoolSetting>,
    mut settings: ResMut<LightingSettings>,
) {
    if let Ok(toggle) = toggles.get(change.source) {
        toggle.set(&mut settings, change.value);
    }
}

fn set_slider(
    change: On<ValueChange<f32>>,
    sliders: Query<&FloatSetting>,
    mut settings: ResMut<LightingSettings>,
    mut wave: ResMut<SineShapeKey>,
) {
    if let Ok(slider) = sliders.get(change.source) {
        slider.set(&mut settings, &mut wave, change.value);
    }
}

/// Radio groups report the selected radio button entity.
fn select_anti_aliasing(
    change: On<ValueChange<Entity>>,
    choices: Query<&AntiAliasingChoice>,
    mut anti_aliasing: ResMut<AntiAliasing>,
) {
    if let Ok(AntiAliasingChoice(choice)) = choices.get(change.value) {
        *anti_aliasing = *choice;
    }
}

fn select_ssao_quality(
    change: On<ValueChange<Entity>>,
    choices: Query<&SsaoQualityChoice>,
    mut settings: ResMut<LightingSettings>,
) {
    if let Ok(SsaoQualityChoice(quality)) = choices.get(change.value) {
        settings.ssao_settings.quality_level = *quality;
    }
}
