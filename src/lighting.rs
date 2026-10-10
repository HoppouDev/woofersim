//! Raster lighting settings: HDRI intensity, screen space reflections,
//! ambient occlusion, bloom, and an optional shadow mapped key light.

use bevy::light::Skybox;
use bevy::pbr::{ScreenSpaceAmbientOcclusion, ScreenSpaceReflections};
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;

use crate::environment::DEFAULT_BRIGHTNESS;

pub struct LightingPlugin;

impl Plugin for LightingPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(LightingSettings::default())
            .add_systems(Startup, spawn_key_light)
            .add_systems(
                Update,
                (
                    apply_lighting_settings.run_if(resource_changed::<LightingSettings>),
                    apply_environment_intensity,
                ),
            );
    }
}

/// Lighting state edited by the control panel.
#[derive(Resource)]
pub struct LightingSettings {
    pub ssr: bool,
    pub ssao: bool,
    pub ssao_settings: ScreenSpaceAmbientOcclusion,
    pub bloom: bool,
    /// Energy-conserving scatter amount, from 0 (none) to 1 (maximum).
    pub bloom_intensity: f32,
    /// HDRI brightness in cd/m² for a texel of 1.0, for lighting and skybox.
    pub hdri_intensity: f32,
    pub key_light: bool,
    /// Key light illuminance in lux.
    pub key_illuminance: f32,
    /// Raytraced lighting with Solari, see [`crate::raytracing`].
    pub raytracing: bool,
}

impl Default for LightingSettings {
    fn default() -> Self {
        Self {
            ssr: true,
            ssao: true,
            ssao_settings: ScreenSpaceAmbientOcclusion {
                // The default 0.73 m reaches across most of the 1.1 m woofer.
                // A small radius keeps the occlusion in the creases.
                radius: 0.15,
                constant_object_thickness: 0.05,
                ..default()
            },
            bloom: true,
            bloom_intensity: 0.08,
            hdri_intensity: DEFAULT_BRIGHTNESS,
            key_light: false,
            key_illuminance: 5_000.0,
            raytracing: false,
        }
    }
}

/// Settings used whenever screen space reflections are enabled.
fn ssr_settings() -> ScreenSpaceReflections {
    ScreenSpaceReflections {
        // Bevy's SSR pass ignores clearcoat, so surfaces it handles lose
        // their clearcoat reflection. Stopping below the surround's base
        // roughness of 0.5 keeps its clearcoat on the regular path.
        //
        // The dust cap's brushed roughness texture varies between 0.32 and
        // 0.68 from texel to texel. A narrow fade switched neighbouring texels
        // between SSR and the environment map, so the brush pattern showed up
        // as hard streaks. A wide fade blends them gradually instead.
        max_perceptual_roughness: 1.0..1.0,
        min_perceptual_roughness: 0.0..0.0,
        ..default()
    }
}

#[derive(Component)]
struct KeyLight;

fn spawn_key_light(mut commands: Commands) {
    commands.spawn((
        KeyLight,
        DirectionalLight {
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(-1.0, 2.0, 1.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}

fn apply_lighting_settings(
    mut commands: Commands,
    settings: Res<LightingSettings>,
    cameras: Query<Entity, With<Camera3d>>,
    mut key_lights: Query<&mut DirectionalLight, With<KeyLight>>,
) {
    for camera in &cameras {
        let mut camera = commands.entity(camera);
        // Solari traces reflections and occlusion itself. SSR would paint
        // environment map reflections over its output and wash it out.
        if settings.ssr && !settings.raytracing {
            camera.insert(ssr_settings());
        } else {
            camera.remove::<ScreenSpaceReflections>();
        }
        if settings.ssao && !settings.raytracing {
            camera.insert(settings.ssao_settings.clone());
        } else {
            camera.remove::<ScreenSpaceAmbientOcclusion>();
        }
        if settings.bloom {
            // The natural preset conserves energy and has no brightness
            // threshold, so every pixel scatters a little, like a real lens.
            camera.insert(Bloom {
                intensity: settings.bloom_intensity,
                ..Bloom::NATURAL
            });
        } else {
            camera.remove::<Bloom>();
        }
    }

    for mut light in &mut key_lights {
        light.illuminance = if settings.key_light {
            settings.key_illuminance
        } else {
            0.0
        };
        // Solari traces its own shadows.
        light.shadow_maps_enabled = !settings.raytracing;
    }
}

type EnvironmentComponents = (
    Option<Mut<'static, Skybox>>,
    Option<Mut<'static, GeneratedEnvironmentMapLight>>,
    Option<Mut<'static, EnvironmentMapLight>>,
);

/// Bevy copies [`GeneratedEnvironmentMapLight`] into a filtered
/// [`EnvironmentMapLight`] once, so both need the intensity. They appear after
/// the HDRI loads, hence the `is_added` checks.
fn apply_environment_intensity(
    settings: Res<LightingSettings>,
    mut cameras: Query<EnvironmentComponents>,
) {
    let intensity = settings.hdri_intensity;
    for (skybox, generated, filtered) in &mut cameras {
        let added = skybox.as_ref().is_some_and(|c| c.is_added())
            || generated.as_ref().is_some_and(|c| c.is_added())
            || filtered.as_ref().is_some_and(|c| c.is_added());
        if !settings.is_changed() && !added {
            continue;
        }
        if let Some(mut skybox) = skybox {
            skybox.brightness = intensity;
        }
        if let Some(mut light) = generated {
            light.intensity = intensity;
        }
        if let Some(mut light) = filtered {
            light.intensity = intensity;
        }
    }
}
