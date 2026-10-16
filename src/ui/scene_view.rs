//! The 3D scene, rendered into an image exactly the size of the gap between
//! the two panels and shown there by egui.
//!
//! Rendering to an image keeps every render pass working on a whole texture.
//! A camera viewport would do the same job more directly, but with this
//! scene's post processing it squeezed the scene into the middle half of the
//! gap.

use bevy::camera::RenderTarget;
use bevy::camera_controller::free_camera::FreeCameraState;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureFormat};
use bevy::window::{CursorGrabMode, CursorOptions};
use bevy_egui::egui::{self, Ui, load::SizedTexture};
use bevy_egui::{EguiTextureHandle, EguiUserTextures};

use super::theme::palette;

#[derive(Resource)]
pub struct SceneView {
    image: Handle<Image>,
    texture: egui::TextureId,
    /// Size the image should have, in physical pixels, from the last layout.
    wanted: Extent3d,
    /// Whether the camera may take input: the pointer is over the scene and
    /// no egui widget has keyboard focus.
    camera_input: bool,
}

/// Smallest image side in pixels. SSAO builds a 5 level mip chain of the
/// depth buffer and fails texture validation below 16 pixels, which quits
/// the app.
const MIN_SIZE: u32 = 64;

/// Points the scene camera at a new image and registers it with egui.
pub fn setup(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut egui_textures: ResMut<EguiUserTextures>,
    scene_camera: Single<Entity, With<Camera3d>>,
) {
    let size = Extent3d {
        width: MIN_SIZE,
        height: MIN_SIZE,
        depth_or_array_layers: 1,
    };
    let mut image =
        Image::new_target_texture(size.width, size.height, TextureFormat::Rgba8UnormSrgb, None);
    // Only the GPU ever writes to this image, and the scene is redrawn every
    // frame, so there is nothing to keep on the CPU or copy over on resize.
    image.data = None;
    image.copy_on_resize = false;
    let image = images.add(image);
    let texture = egui_textures.add_image(EguiTextureHandle::Strong(image.clone()));
    commands
        .entity(*scene_camera)
        .insert(RenderTarget::Image(image.clone().into()));
    commands.insert_resource(SceneView {
        image,
        texture,
        wanted: size,
        camera_input: false,
    });
}

/// Turns the free camera off while its input belongs to egui, so scrolling a
/// panel does not change the camera speed and typing into a field does not
/// move it. Runs right before the camera controller.
pub fn gate_camera(
    view: Res<SceneView>,
    cursor: Query<&CursorOptions>,
    mut camera: Query<&mut FreeCameraState>,
) {
    // While the controller holds the cursor for mouse look, egui hover is
    // meaningless, and turning it off would release the cursor.
    let grabbed = cursor
        .iter()
        .any(|options| options.grab_mode != CursorGrabMode::None);
    let enabled = view.camera_input || grabbed;
    for mut state in &mut camera {
        // Compared first so the component is not marked changed every frame.
        if state.enabled != enabled {
            state.enabled = enabled;
        }
    }
}

/// Resizes the image to the size of the last layout. Runs in `Update`, ahead
/// of the camera update in `PostUpdate`, so the camera picks up the new size
/// in the same frame the texture is reallocated. Otherwise it renders one
/// frame at the old size into the new texture, which flickers.
pub fn resize(
    view: Res<SceneView>,
    mut images: ResMut<Assets<Image>>,
    mut projection: Single<&mut Projection, With<Camera3d>>,
) {
    let current = images
        .get(&view.image)
        .map(|image| image.texture_descriptor.size);
    if current == Some(view.wanted) {
        return;
    }
    // Looking the image up mutably marks it modified, which reallocates the
    // texture, so this only happens when the size actually changed.
    if let Some(mut image) = images.get_mut(&view.image) {
        image.texture_descriptor.size = view.wanted;
    }
    // The camera only re-reads its target size on asset events, which it
    // sees one frame late, or when its projection changed.
    projection.set_changed();
}

/// Fills the remaining space of `ui` with the scene and records the size the
/// image needs to match it in physical pixels. With `render` off the camera
/// is inactive and the image would hold a stale frame, so a note stands in.
pub fn show(ui: &mut Ui, view: &mut SceneView, render: bool) {
    let size = ui.available_size();
    let scale = ui.ctx().pixels_per_point();
    view.wanted = Extent3d {
        width: ((size.x * scale).round() as u32).max(MIN_SIZE),
        height: ((size.y * scale).round() as u32).max(MIN_SIZE),
        depth_or_array_layers: 1,
    };
    if !render {
        view.camera_input = false;
        let p = palette();
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        // Crust, one shade darker than the panels around it.
        ui.painter().rect_filled(rect, 0.0, p.crust);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Scene rendering is off",
            egui::TextStyle::Body.resolve(ui.style()),
            p.overlay1,
        );
        return;
    }
    let response = ui.image(SizedTexture::new(view.texture, size));
    view.camera_input = response.contains_pointer() && !ui.ctx().egui_wants_keyboard_input();
}
