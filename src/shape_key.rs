//! Drives a named shape key (glTF morph target) with a sine wave.

use std::f32::consts::TAU;

use bevy::mesh::morph::MorphWeights;
use bevy::prelude::*;

pub struct SineShapeKeyPlugin {
    /// Shape key name as exported from Blender.
    pub name: &'static str,
    /// Initial oscillations per second.
    pub frequency: f32,
}

impl Plugin for SineShapeKeyPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SineShapeKey {
            name: self.name,
            frequency: self.frequency,
            phase: 0.0,
        })
        .add_systems(Update, (find_shape_key, animate_shape_key).chain());
    }
}

#[derive(Resource)]
pub struct SineShapeKey {
    name: &'static str,
    /// Oscillations per second. Can be changed at runtime.
    pub frequency: f32,
    /// Accumulated so that changing the frequency does not make the wave jump.
    phase: f32,
}

/// Index of the animated shape key within an entity's [`MorphWeights`].
#[derive(Component)]
struct ShapeKeyIndex(usize);

fn find_shape_key(
    mut commands: Commands,
    shape_key: Res<SineShapeKey>,
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

fn animate_shape_key(
    time: Res<Time>,
    mut shape_key: ResMut<SineShapeKey>,
    mut query: Query<(&mut MorphWeights, &ShapeKeyIndex)>,
) {
    // The phase changes every frame, so keep it out of change detection.
    // Otherwise anything watching the frequency would run every frame.
    let shape_key = shape_key.bypass_change_detection();
    shape_key.phase = (shape_key.phase + time.delta_secs() * shape_key.frequency * TAU) % TAU;
    let value = shape_key.phase.sin();
    for (mut weights, ShapeKeyIndex(index)) in &mut query {
        weights.weights_mut()[*index] = value;
    }
}
