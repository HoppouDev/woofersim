//! Applies a weight to a named shape key (glTF morph target).

use bevy::mesh::morph::{MeshMorphWeights, MorphWeights};
use bevy::prelude::*;

pub struct ShapeKeyPlugin {
    /// Shape key name as exported from Blender.
    pub name: &'static str,
}

impl Plugin for ShapeKeyPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ShapeKey {
            name: self.name,
            weight: 0.0,
        })
        .add_systems(Update, (find_shape_key, apply_shape_key).chain())
        .add_systems(Last, refresh_morph_descriptors);
    }
}

#[derive(Resource)]
pub struct ShapeKey {
    name: &'static str,
    /// Weight applied to every mesh that has the shape key.
    pub weight: f32,
}

/// Index of the shape key within an entity's [`MorphWeights`].
#[derive(Component)]
struct ShapeKeyIndex(usize);

fn find_shape_key(
    mut commands: Commands,
    shape_key: Res<ShapeKey>,
    meshes: Res<Assets<Mesh>>,
    added: Query<(Entity, &MorphWeights), Added<MorphWeights>>,
) {
    for (entity, weights) in &added {
        let index = weights
            .first_mesh()
            .and_then(|mesh| meshes.get(mesh))
            .and_then(|mesh| mesh.morph_target_names())
            .and_then(|names| names.iter().position(|n| n == shape_key.name));
        if let Some(index) = index {
            commands.entity(entity).insert(ShapeKeyIndex(index));
        }
    }
}

fn apply_shape_key(
    shape_key: Res<ShapeKey>,
    mut query: Query<(&mut MorphWeights, &ShapeKeyIndex)>,
) {
    for (mut weights, ShapeKeyIndex(index)) in &mut query {
        weights.weights_mut()[*index] = shape_key.weight;
    }
}

/// Works around a Bevy 0.20 bug that permanently freezes a morphed mesh in
/// its neutral pose once it has been invisible for a frame, for example after
/// being frustum culled while the camera looks away.
///
/// While a mesh is invisible its GPU morph descriptor is freed. When it becomes
/// visible again the mesh instance is extracted before a new descriptor is
/// allocated, so it stores "no morph targets", and it is never extracted again
/// because nothing about it changes. Flagging the transform as changed makes
/// Bevy re-extract these few meshes every frame, so they pick up the current
/// descriptor one frame later.
fn refresh_morph_descriptors(mut meshes: Query<&mut GlobalTransform, With<MeshMorphWeights>>) {
    for mut transform in &mut meshes {
        transform.set_changed();
    }
}
