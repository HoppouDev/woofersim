//! Image based lighting from an equirectangular EXR.
//!
//! Bevy's skybox and environment map lighting only accept cubemaps, so the
//! panorama is resampled into a cubemap on the CPU once it has loaded. The
//! cubemap then drives both the [`Skybox`] and a
//! [`GeneratedEnvironmentMapLight`], which Bevy prefilters on the GPU.

use std::f32::consts::{PI, TAU};

use bevy::asset::RenderAssetUsages;
use bevy::image::ExrTextureLoaderSettings;
use bevy::light::Skybox;
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};

/// Brightness in cd/m² that a panorama texel of 1.0 maps to. Together with
/// Bevy's default exposure this roughly matches how Blender displays the HDRI.
/// The lighting settings can change it at runtime.
pub const DEFAULT_BRIGHTNESS: f32 = 1000.0;

pub struct EnvironmentPlugin {
    /// Equirectangular EXR, relative to the asset folder.
    pub path: &'static str,
    /// Edge length of each cubemap face. Must be a power of two.
    pub face_size: u32,
}

impl Plugin for EnvironmentPlugin {
    fn build(&self, app: &mut App) {
        assert!(self.face_size.is_power_of_two());
        app.insert_resource(EnvironmentSettings {
            path: self.path,
            face_size: self.face_size,
        })
        .add_systems(Startup, load_panorama)
        .add_systems(
            Update,
            apply_environment.run_if(resource_exists::<PendingPanorama>),
        );
    }
}

/// Marks the camera that receives the skybox and environment lighting.
#[derive(Component, Default)]
pub struct EnvironmentCamera;

#[derive(Resource)]
struct EnvironmentSettings {
    path: &'static str,
    face_size: u32,
}

#[derive(Resource)]
struct PendingPanorama(Handle<Image>);

fn load_panorama(
    mut commands: Commands,
    settings: Res<EnvironmentSettings>,
    asset_server: Res<AssetServer>,
) {
    // The panorama is only read on the CPU, so it is never uploaded to the GPU.
    let handle = asset_server
        .load_builder()
        .with_settings(|s: &mut ExrTextureLoaderSettings| {
            s.asset_usage = RenderAssetUsages::MAIN_WORLD
        })
        .load(settings.path);
    commands.insert_resource(PendingPanorama(handle));
}

fn apply_environment(
    mut commands: Commands,
    settings: Res<EnvironmentSettings>,
    pending: Res<PendingPanorama>,
    mut images: ResMut<Assets<Image>>,
    cameras: Query<Entity, With<EnvironmentCamera>>,
) {
    let Some(panorama) = images.get(&pending.0) else {
        return;
    };
    let cubemap = match equirect_to_cubemap(panorama, settings.face_size) {
        Ok(cubemap) => images.add(cubemap),
        Err(err) => {
            error!("cannot build environment from {}: {err}", settings.path);
            commands.remove_resource::<PendingPanorama>();
            return;
        }
    };
    for camera in &cameras {
        commands.entity(camera).insert((
            Skybox {
                image: Some(cubemap.clone()),
                brightness: DEFAULT_BRIGHTNESS,
                ..default()
            },
            GeneratedEnvironmentMapLight {
                environment_map: cubemap.clone(),
                intensity: DEFAULT_BRIGHTNESS,
                ..default()
            },
        ));
    }
    // Dropping the handle frees the CPU copy of the panorama.
    commands.remove_resource::<PendingPanorama>();
}

fn equirect_to_cubemap(panorama: &Image, face_size: u32) -> Result<Image, String> {
    if panorama.texture_descriptor.format != TextureFormat::Rgba32Float {
        return Err(format!(
            "expected Rgba32Float, got {:?}",
            panorama.texture_descriptor.format
        ));
    }
    let data = panorama.data.as_deref().ok_or("image has no CPU data")?;
    let texels: Vec<[f32; 4]> = data
        .as_chunks::<16>()
        .0
        .iter()
        .map(|px| {
            std::array::from_fn(|c| f32::from_le_bytes(px[c * 4..c * 4 + 4].try_into().unwrap()))
        })
        .collect();
    let (width, height) = (panorama.width(), panorama.height());

    let mut cubemap = Image::new_fill(
        Extent3d {
            width: face_size,
            height: face_size,
            depth_or_array_layers: 6,
        },
        TextureDimension::D2,
        &[0; 8],
        // Rgba32Float is not filterable on all GPUs, which the prefilter needs.
        TextureFormat::Rgba16Float,
        RenderAssetUsages::RENDER_WORLD,
    );
    cubemap.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..default()
    });

    for face in 0..6 {
        for y in 0..face_size {
            for x in 0..face_size {
                let s = 2.0 * (x as f32 + 0.5) / face_size as f32 - 1.0;
                let t = 2.0 * (y as f32 + 0.5) / face_size as f32 - 1.0;
                let [r, g, b, _] =
                    sample_bilinear(&texels, width, height, cube_direction(face, s, t));
                cubemap
                    .set_color_at_3d(x, y, face, Color::linear_rgb(r, g, b))
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(cubemap)
}

/// World space direction of a cubemap texel. Faces follow the wgpu layer
/// order (+X, -X, +Y, -Y, +Z, -Z). Bevy samples cubemaps with Z negated, so
/// the face's lookup direction is flipped back into world space here.
fn cube_direction(face: u32, s: f32, t: f32) -> Vec3 {
    let lookup = match face {
        0 => Vec3::new(1.0, -t, -s),
        1 => Vec3::new(-1.0, -t, s),
        2 => Vec3::new(s, 1.0, t),
        3 => Vec3::new(s, -1.0, -t),
        4 => Vec3::new(s, -t, 1.0),
        _ => Vec3::new(-s, -t, -1.0),
    };
    Vec3::new(lookup.x, lookup.y, -lookup.z).normalize()
}

/// Samples the panorama in the direction `dir`, oriented like Blender's world
/// texture after the glTF Y-up conversion, so reflections match the .blend.
fn sample_bilinear(texels: &[[f32; 4]], width: u32, height: u32, dir: Vec3) -> [f32; 4] {
    // Blender's mapping: u = 0.5 - atan2(y, x) / 2π in its Z-up frame, where
    // Blender's Y is glTF's -Z.
    let u = 0.5 - (-dir.z).atan2(dir.x) / TAU;
    let v = dir.y.clamp(-1.0, 1.0).acos() / PI;

    let fx = u * width as f32 - 0.5;
    let fy = (v * height as f32 - 0.5).clamp(0.0, height as f32 - 1.0);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (wx, wy) = (fx - x0, fy - y0);
    let x0 = (x0 as i32).rem_euclid(width as i32) as u32;
    let x1 = (x0 + 1) % width;
    let y0 = y0 as u32;
    let y1 = (y0 + 1).min(height - 1);

    let at = |x: u32, y: u32| texels[(y * width + x) as usize];
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * wx;
        let bottom = c[i] + (d[i] - c[i]) * wx;
        top + (bottom - top) * wy
    })
}
