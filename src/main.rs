mod anti_aliasing;
mod environment;
mod lighting;
mod morph_tangents;
mod raytracing;
mod shape_key;
mod ui;

use anti_aliasing::AntiAliasingPlugin;
use bevy::camera::Hdr;
use bevy::camera_controller::free_camera::{FreeCamera, FreeCameraPlugin};
use bevy::log::LogPlugin;
use bevy::pbr::DefaultOpaqueRendererMethod;
use bevy::prelude::*;
use bevy::render::view::Tonemapping;
use bevy::world_serialization::WorldAssetRoot;
use bevy_framepace::{FramepacePlugin, FramepaceSettings, Limiter};
use tracing::level_filters::LevelFilter;

use environment::{EnvironmentCamera, EnvironmentPlugin};
use lighting::LightingPlugin;
use morph_tangents::MorphTangentsPlugin;
use raytracing::RaytracingPlugin;
use shape_key::SineShapeKeyPlugin;
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
            path: "art_studio_2k.exr",
            face_size: 512,
        })
        .add_plugins(SineShapeKeyPlugin {
            name: "Position",
            frequency: 1.0,
        })
        // .add_plugins(MorphTangentsPlugin)
        .add_plugins(LightingPlugin)
        .add_plugins(RaytracingPlugin)
        .add_plugins(AntiAliasingPlugin)
        .add_plugins(ControlPanelPlugin)
        .add_plugins(bevy_framepace::FramepacePlugin)
        .add_systems(Startup, setup)
        .add_systems(Startup, setup_fps_limit)
        .run();

    Ok(())
}

fn setup_fps_limit(mut settings: ResMut<FramepaceSettings>) {
    settings.limiter = Limiter::from_framerate(30.0);
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn((
        Camera3d::default(),
        Hdr,
        Tonemapping::AgX,
        // Deferred rendering, which SSR needs, does not support MSAA.
        Msaa::Off,
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
