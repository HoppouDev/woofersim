//! Optional raytraced lighting with Bevy Solari, using ReSTIR.
//!
//! While enabled, the camera gets [`SolariLighting`] and every standard
//! material mesh gets a [`RaytracingMesh3d`]. Both are removed again when it is
//! disabled, so the acceleration structures are not rebuilt every frame for
//! nothing.
//!
//! Screen space reflections and ambient occlusion are suspended meanwhile,
//! since Solari already traces both.

use bevy::camera::CameraMainTextureUsages;
use bevy::core_pipeline::prepass::{DeferredPrepassDoubleBuffer, DepthPrepassDoubleBuffer};
use bevy::mesh::Indices;
use bevy::mesh::morph::MeshMorphWeights;
use bevy::prelude::*;
use bevy::render::render_resource::TextureUsages;
use bevy::solari::prelude::{RaytracingMesh3d, SolariLighting, SolariPlugins};

use crate::lighting::LightingSettings;

pub struct RaytracingPlugin;

impl Plugin for RaytracingPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(SolariPlugins).add_systems(
            Update,
            (
                apply_camera.run_if(resource_changed::<LightingSettings>),
                add_raytracing_meshes,
                remove_raytracing_meshes,
            ),
        );
    }
}

fn apply_camera(
    mut commands: Commands,
    settings: Res<LightingSettings>,
    cameras: Query<(Entity, Has<SolariLighting>), With<Camera3d>>,
) {
    for (camera, has_solari) in &cameras {
        let mut camera = commands.entity(camera);
        match (settings.raytracing, has_solari) {
            (true, false) => {
                camera.insert((
                    SolariLighting {
                        restir: true,
                        primary_di_samples: 64,
                        secondary_di_samples: 32,
                        ..default()
                    },
                    // Solari writes its lighting straight into the main texture.
                    CameraMainTextureUsages::default().with(TextureUsages::STORAGE_BINDING),
                ));
            }
            (false, true) => {
                // Solari only adds the ReSTIR double buffers, so they are
                // left behind otherwise.
                camera.remove::<(
                    SolariLighting,
                    DeferredPrepassDoubleBuffer,
                    DepthPrepassDoubleBuffer,
                )>();
                camera.insert(CameraMainTextureUsages::default());
            }
            _ => {}
        }
    }
}

/// Morphing meshes stay out of the acceleration structure. Solari traces the
/// rest pose, so a deformed cone would shadow itself wherever it moves away
/// from it. Their pixels are still lit by Solari, they just cast no rays.
type NotYetRaytraced = (
    With<MeshMaterial3d<StandardMaterial>>,
    Without<RaytracingMesh3d>,
    Without<MeshMorphWeights>,
);

/// Runs every frame so meshes from scenes that finish loading while
/// raytracing is on are picked up too.
fn add_raytracing_meshes(
    mut commands: Commands,
    settings: Res<LightingSettings>,
    candidates: Query<(Entity, &Mesh3d), NotYetRaytraced>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    if !settings.raytracing {
        return;
    }
    for (entity, Mesh3d(handle)) in &candidates {
        // Wait for the mesh to load. Inserting the component early would
        // leave it without a BLAS.
        let Some(mesh) = meshes.get(handle) else {
            continue;
        };
        if !mesh.contains_attribute(Mesh::ATTRIBUTE_UV_0) {
            warn_once!("mesh {:?} has no UVs and cannot be raytraced", handle.id());
            continue;
        }
        if needs_conversion(mesh)
            && let Some(mut mesh) = meshes.get_mut(handle)
            && let Err(err) = convert(&mut mesh)
        {
            warn_once!("mesh {:?} cannot be raytraced: {err}", handle.id());
            continue;
        }
        commands
            .entity(entity)
            .insert(RaytracingMesh3d(handle.clone()));
    }
}

fn remove_raytracing_meshes(
    mut commands: Commands,
    settings: Res<LightingSettings>,
    raytraced: Query<Entity, With<RaytracingMesh3d>>,
) {
    if settings.raytracing {
        return;
    }
    for entity in &raytraced {
        commands.entity(entity).remove::<RaytracingMesh3d>();
    }
}

/// Solari only accepts triangle lists with exactly position, normal, UV and
/// tangent attributes and 32 bit indices.
fn needs_conversion(mesh: &Mesh) -> bool {
    mesh.contains_attribute(Mesh::ATTRIBUTE_UV_1)
        || !mesh.contains_attribute(Mesh::ATTRIBUTE_TANGENT)
        || matches!(mesh.indices(), Some(Indices::U16(_)))
}

fn convert(mesh: &mut Mesh) -> Result<(), String> {
    if mesh.contains_attribute(Mesh::ATTRIBUTE_UV_1) {
        mesh.remove_attribute(Mesh::ATTRIBUTE_UV_1);
    }
    if !mesh.contains_attribute(Mesh::ATTRIBUTE_TANGENT) {
        mesh.generate_tangents().map_err(|err| err.to_string())?;
    }
    if let Some(indices) = mesh.indices_mut()
        && let Indices::U16(_) = indices
    {
        *indices = Indices::U32(indices.iter().map(|i| i as u32).collect());
    }
    Ok(())
}
