//! Settings panels drawn with egui, docked full height on both sides of the
//! window: the woofer simulation and audio output on the left, the graphics
//! settings on the right. Between them, the scene fills the space above a
//! graph of the setup's frequency response and cone excursion, and is only
//! rendered at that size. Every setting has a reset button. The style is
//! Catppuccin Mocha with the mauve accent.

mod panels;
mod response_graph;
mod scene_view;
mod theme;
mod widgets;

use bevy::camera::visibility::RenderLayers;
use bevy::camera_controller::free_camera::run_freecamera_controller;
use bevy::prelude::*;
use bevy_egui::egui::{self, Frame, LayerId, Margin, UiBuilder};
use bevy_egui::{
    EguiContext, EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPreUpdateSet,
    EguiPrimaryContextPass, PrimaryEguiContext,
};

use crate::anti_aliasing::AntiAliasing;
use crate::audio::WooferSettings;
use crate::lighting::LightingSettings;
use scene_view::SceneView;

pub struct ControlPanelPlugin;

impl Plugin for ControlPanelPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin::default())
            .insert_resource(EguiGlobalSettings {
                auto_create_primary_context: false,
                ..default()
            })
            .add_systems(Startup, spawn_ui_camera)
            // The scene camera is spawned during Startup.
            .add_systems(PostStartup, scene_view::setup)
            .add_systems(Update, scene_view::resize)
            .add_systems(
                RunFixedMainLoop,
                scene_view::gate_camera
                    .in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop)
                    .before(run_freecamera_controller),
            )
            .add_systems(
                PreUpdate,
                style_new_context.after(EguiPreUpdateSet::InitContexts),
            )
            .add_systems(EguiPrimaryContextPass, draw_ui);
    }
}

/// The only camera drawing to the window. It renders nothing but egui: the
/// panels and the image the scene camera renders into. It runs after the
/// scene camera, so egui shows this frame's scene. That matters when the
/// image was just resized, because a fresh texture is still empty.
fn spawn_ui_camera(mut commands: Commands) {
    commands.spawn((
        Camera2d,
        Camera {
            order: 1,
            ..default()
        },
        RenderLayers::none(),
        PrimaryEguiContext,
    ));
}

/// Installs the fonts and theme as soon as the context exists. Fonts set
/// during a pass only arrive at the next one, while styles apply at once, so
/// styling inside the first pass would ask for the Inter SemiBold family
/// before egui has it and panic.
fn style_new_context(mut contexts: Query<&mut EguiContext, Added<EguiContext>>) {
    for mut context in &mut contexts {
        let ctx = context.get_mut();
        ctx.set_fonts(theme::fonts());
        ctx.all_styles_mut(theme::apply);
    }
}

/// Height of the graph panel below the scene, including its margins.
const GRAPH_HEIGHT: f32 = 220.0;

fn draw_ui(
    mut contexts: EguiContexts,
    mut woofer: ResMut<WooferSettings>,
    lighting: ResMut<LightingSettings>,
    anti_aliasing: ResMut<AntiAliasing>,
    mut scene: ResMut<SceneView>,
    mut graph: Local<response_graph::ResponseGraph>,
) -> Result {
    let ctx = contexts.ctx_mut()?;

    let mut root = egui::Ui::new(
        ctx.clone(),
        "panels".into(),
        UiBuilder::new()
            .layer_id(LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );
    panels::woofer_panel(&mut root, &mut woofer);
    panels::graphics_panel(&mut root, lighting, anti_aliasing);
    // Added after the side panels, so it only spans the space between them.
    egui::Panel::bottom("response")
        .frame(Frame::side_top_panel(root.style()).inner_margin(Margin::same(10)))
        .exact_size(GRAPH_HEIGHT)
        .resizable(false)
        .show(&mut root, |ui| {
            response_graph::show(ui, &mut graph, &woofer);
        });
    egui::CentralPanel::no_frame().show(&mut root, |ui| {
        scene_view::show(ui, &mut scene);
    });
    Ok(())
}
