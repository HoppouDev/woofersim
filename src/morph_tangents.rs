//! Tangent deltas for morph targets.
//!
//! Normal maps need tangents that follow the deformation. Blender regenerates
//! tangents on the deformed mesh, but a glTF morph target can only store a
//! tangent delta, and neither source of that delta is usable:
//!
//! * Without exported tangents, Bevy's glTF loader generates rest pose
//!   tangents only, and the targets carry no tangent deltas.
//! * Blender's exporter (Shape Key Tangents) does not regenerate tangents. It
//!   rotates the rest tangent by the inverse of the normal's rotation, so at
//!   full weight the tangents lean about 18 degrees out of the surface.
//!
//! Either way the normal map appears to slide over the surface as the shape
//! key animates. This regenerates tangents for each fully applied target with
//! the same MikkTSpace generator Bevy uses for the rest pose, which matches
//! Blender's rest tangents, and stores the difference as the tangent delta.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::morph::MorphAttributes;
use bevy::mesh::{PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;

pub struct MorphTangentsPlugin;

impl Plugin for MorphTangentsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, add_morph_target_tangents);
    }
}

fn add_morph_target_tangents(
    mut events: MessageReader<AssetEvent<Mesh>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    for event in events.read() {
        // Only on `Added`, since writing the targets emits `Modified`.
        let AssetEvent::Added { id } = event else {
            continue;
        };
        let Some(mut mesh) = meshes.get_mut(*id) else {
            continue;
        };
        match morph_target_tangents(&mesh) {
            Ok(Some(targets)) => {
                if let Err(err) = mesh.try_set_morph_targets(targets) {
                    warn!("cannot store morph target tangents for {id}: {err}");
                }
            }
            Ok(None) => {}
            Err(err) => warn!("cannot compute morph target tangents for {id}: {err}"),
        }
    }
}

/// Returns the mesh's morph targets with regenerated tangent deltas, or
/// `None` if the mesh has no morph targets or no tangents. Exported tangent
/// deltas are replaced.
fn morph_target_tangents(mesh: &Mesh) -> Result<Option<Vec<MorphAttributes>>, String> {
    let Ok(targets) = mesh.try_morph_targets() else {
        return Ok(None);
    };
    let Ok(VertexAttributeValues::Float32x4(base_tangents)) =
        mesh.try_attribute(Mesh::ATTRIBUTE_TANGENT)
    else {
        return Ok(None);
    };

    let float3 = |attribute| match mesh.try_attribute(attribute) {
        Ok(VertexAttributeValues::Float32x3(values)) => Ok(values),
        _ => Err(format!("missing {attribute:?}")),
    };
    let positions = float3(Mesh::ATTRIBUTE_POSITION.id)?;
    let normals = float3(Mesh::ATTRIBUTE_NORMAL.id)?;
    let uvs = mesh
        .try_attribute(Mesh::ATTRIBUTE_UV_0)
        .map_err(|err| format!("missing UVs: {err}"))?;
    let indices = mesh
        .try_indices()
        .map_err(|err| format!("missing indices: {err}"))?;

    let vertex_count = positions.len();
    if vertex_count == 0 || !targets.len().is_multiple_of(vertex_count) {
        return Err("morph target size does not match the vertex count".into());
    }

    let mut targets = targets.clone();
    for target in targets.chunks_exact_mut(vertex_count) {
        let morphed_positions: Vec<[f32; 3]> = positions
            .iter()
            .zip(target.iter())
            .map(|(p, delta)| (Vec3::from(*p) + delta.position).into())
            .collect();
        let morphed_normals: Vec<[f32; 3]> = normals
            .iter()
            .zip(target.iter())
            .map(|(n, delta)| (Vec3::from(*n) + delta.normal).normalize_or_zero().into())
            .collect();
        let mut pose = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::MAIN_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, morphed_positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, morphed_normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs.clone())
        .with_inserted_indices(indices.clone());
        pose.generate_tangents().map_err(|err| err.to_string())?;
        let Ok(VertexAttributeValues::Float32x4(pose_tangents)) =
            pose.try_attribute(Mesh::ATTRIBUTE_TANGENT)
        else {
            return Err("tangent generation produced no tangents".into());
        };

        for (delta, (morphed, base)) in target
            .iter_mut()
            .zip(pose_tangents.iter().zip(base_tangents))
        {
            // The handedness in `w` cannot be animated. Where it flips, a
            // delta would blend between unrelated frames, so leave it at rest.
            delta.tangent = if morphed[3] == base[3] {
                Vec3::from_slice(morphed) - Vec3::from_slice(base)
            } else {
                Vec3::ZERO
            };
        }
    }
    Ok(Some(targets))
}
