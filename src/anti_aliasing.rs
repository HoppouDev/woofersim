//! Switchable anti-aliasing. MSAA is unavailable because deferred rendering
//! requires it to be off, so the choice is between the post process methods.

use bevy::anti_alias::fxaa::Fxaa;
use bevy::anti_alias::smaa::Smaa;
use bevy::anti_alias::taa::TemporalAntiAliasing;
use bevy::prelude::*;
use bevy::render::camera::{MipBias, TemporalJitter};

pub struct AntiAliasingPlugin;

impl Plugin for AntiAliasingPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(AntiAliasing::Taa).add_systems(
            Update,
            apply_anti_aliasing.run_if(resource_changed::<AntiAliasing>),
        );
    }
}

/// Anti-aliasing method for the 3D camera.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum AntiAliasing {
    Off,
    /// Also smooths the noise of SSR and SSAO.
    #[default]
    Taa,
    Fxaa,
    Smaa,
}

fn apply_anti_aliasing(
    mut commands: Commands,
    anti_aliasing: Res<AntiAliasing>,
    cameras: Query<Entity, With<Camera3d>>,
) {
    for camera in &cameras {
        let mut camera = commands.entity(camera);
        // TAA's required jitter and mip bias stay behind when only the TAA
        // component is removed, and would make the other methods shimmer.
        camera.remove::<(TemporalAntiAliasing, TemporalJitter, MipBias, Fxaa, Smaa)>();
        match *anti_aliasing {
            AntiAliasing::Off => {}
            AntiAliasing::Taa => {
                camera.insert(TemporalAntiAliasing::default());
            }
            AntiAliasing::Fxaa => {
                camera.insert(Fxaa::default());
            }
            AntiAliasing::Smaa => {
                camera.insert(Smaa::default());
            }
        }
    }
}
