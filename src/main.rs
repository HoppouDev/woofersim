mod anti_aliasing;
mod audio;
mod environment;
mod lighting;
mod persistence;
mod raytracing;
mod shape_key;
mod ui;

use anti_aliasing::AntiAliasingPlugin;
use audio::WooferAudioPlugin;
use bevy::camera::Hdr;
use bevy::camera_controller::free_camera::{FreeCamera, FreeCameraPlugin};
use bevy::core_pipeline::prepass::{DeferredPrepass, DepthPrepass};
use bevy::log::LogPlugin;
use bevy::pbr::DefaultOpaqueRendererMethod;
use bevy::prelude::*;
use bevy::render::view::Tonemapping;
use bevy::world_serialization::WorldAssetRoot;
use tracing::level_filters::LevelFilter;

use environment::{EnvironmentCamera, EnvironmentPlugin};
use lighting::LightingPlugin;
use persistence::PersistencePlugin;
use raytracing::RaytracingPlugin;
use shape_key::ShapeKeyPlugin;
use ui::ControlPanelPlugin;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(LevelFilter::INFO)
        .compact()
        .without_time()
        .init();

    App::new()
        // Screen space reflections only work with deferred rendering.
        .insert_resource(DefaultOpaqueRendererMethod::deferred())
        // Logging is already set up through tracing_subscriber above.
        .add_plugins(DefaultPlugins.build().disable::<LogPlugin>())
        .insert_resource(GlobalAmbientLight::NONE)
        .add_plugins(FreeCameraPlugin)
        .add_plugins(EnvironmentPlugin {
            path: "citrus_orchard_road_puresky_4k.exr",
            face_size: 1024,
        })
        .add_plugins(ShapeKeyPlugin { name: "Position" })
        // Loads the saved settings, so it goes before the plugins using them.
        .add_plugins(PersistencePlugin)
        .add_plugins(WooferAudioPlugin)
        // .add_plugins(MorphTangentsPlugin)
        .add_plugins(LightingPlugin)
        .add_plugins(RaytracingPlugin)
        .add_plugins(AntiAliasingPlugin)
        .add_plugins(ControlPanelPlugin)
        .add_systems(Startup, setup)
        .run();

    Ok(())
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn((
        Camera3d::default(),
        // Opaque materials are deferred, so they are only drawn by a camera
        // with these prepasses. SSR requires them too, but they must not
        // depend on SSR being on at startup.
        DepthPrepass,
        DeferredPrepass,
        Hdr,
        Tonemapping::AgX,
        Transform::from_xyz(0.0, 1.0, 1.5).looking_at(Vec3::ZERO, Vec3::Y),
        FreeCamera {
            walk_speed: 1.0,
            run_speed: 3.0,
            ..default()
        },
        EnvironmentCamera,
    ));

    commands.spawn(WorldAssetRoot(
        asset_server.load(GltfAssetLabel::Scene(0).from_asset("woofer/woofer.glb")),
    ));
}
