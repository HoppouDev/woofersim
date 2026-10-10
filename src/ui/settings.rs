//! Components that tie each widget to the setting it edits.

use bevy::pbr::ScreenSpaceAmbientOcclusionQualityLevel;
use bevy::prelude::*;

use crate::anti_aliasing::AntiAliasing;
use crate::lighting::LightingSettings;
use crate::shape_key::SineShapeKey;

/// Which boolean setting a checkbox controls.
#[derive(Component, Clone, Copy, Default)]
pub enum BoolSetting {
    #[default]
    Ssr,
    Ssao,
    Bloom,
    KeyLight,
    Raytracing,
}

/// Which numeric setting a slider controls.
#[derive(Component, Clone, Copy, Default)]
pub enum FloatSetting {
    #[default]
    HdriIntensity,
    SsaoRadius,
    SsaoThickness,
    BloomIntensity,
    KeyIlluminance,
    WaveSpeed,
}

/// The anti-aliasing method a radio button selects.
#[derive(Component, Clone, Copy, Default)]
pub struct AntiAliasingChoice(pub AntiAliasing);

/// The SSAO quality a radio button selects.
#[derive(Component, Clone, Copy, Default)]
pub struct SsaoQualityChoice(pub ScreenSpaceAmbientOcclusionQualityLevel);

impl BoolSetting {
    pub fn get(self, s: &LightingSettings) -> bool {
        match self {
            BoolSetting::Ssr => s.ssr,
            BoolSetting::Ssao => s.ssao,
            BoolSetting::Bloom => s.bloom,
            BoolSetting::KeyLight => s.key_light,
            BoolSetting::Raytracing => s.raytracing,
        }
    }

    pub fn set(self, s: &mut LightingSettings, value: bool) {
        match self {
            BoolSetting::Ssr => s.ssr = value,
            BoolSetting::Ssao => s.ssao = value,
            BoolSetting::Bloom => s.bloom = value,
            BoolSetting::KeyLight => s.key_light = value,
            BoolSetting::Raytracing => s.raytracing = value,
        }
    }
}

impl FloatSetting {
    pub fn get(self, s: &LightingSettings, wave: &SineShapeKey) -> f32 {
        match self {
            FloatSetting::HdriIntensity => s.hdri_intensity,
            FloatSetting::SsaoRadius => s.ssao_settings.radius,
            FloatSetting::SsaoThickness => s.ssao_settings.constant_object_thickness,
            FloatSetting::BloomIntensity => s.bloom_intensity,
            FloatSetting::KeyIlluminance => s.key_illuminance,
            FloatSetting::WaveSpeed => wave.frequency,
        }
    }

    pub fn set(self, s: &mut LightingSettings, wave: &mut SineShapeKey, value: f32) {
        match self {
            FloatSetting::HdriIntensity => s.hdri_intensity = value,
            FloatSetting::SsaoRadius => s.ssao_settings.radius = value,
            FloatSetting::SsaoThickness => s.ssao_settings.constant_object_thickness = value,
            FloatSetting::BloomIntensity => s.bloom_intensity = value,
            FloatSetting::KeyIlluminance => s.key_illuminance = value,
            FloatSetting::WaveSpeed => wave.frequency = value,
        }
    }
}
