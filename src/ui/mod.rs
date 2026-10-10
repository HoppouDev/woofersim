//! Control panels for lighting, screen space effects, anti-aliasing and the
//! shape key wave speed.
//!
//! Widgets only write to the settings resources. `sync_widgets` mirrors the
//! resources back into the widgets, so they always show the current state.

mod panels;
mod settings;

use bevy::feathers::FeathersPlugins;
use bevy::feathers::dark_theme::create_dark_theme;
use bevy::feathers::theme::UiTheme;
use bevy::prelude::*;
use bevy::ui::Checked;
use bevy::ui_widgets::SliderValue;

use crate::anti_aliasing::AntiAliasing;
use crate::lighting::LightingSettings;
use crate::shape_key::SineShapeKey;
use settings::{AntiAliasingChoice, BoolSetting, FloatSetting, SsaoQualityChoice};

pub struct ControlPanelPlugin;

impl Plugin for ControlPanelPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(FeathersPlugins)
            .insert_resource(UiTheme(create_dark_theme()))
            .add_systems(Startup, |mut commands: Commands| {
                commands.spawn_scene(panels::lighting_panel());
                commands.spawn_scene(panels::post_process_panel());
            })
            .add_systems(
                Update,
                (sync_widgets, sync_radio_buttons).run_if(
                    resource_changed::<LightingSettings>
                        .or_else(resource_changed::<SineShapeKey>)
                        .or_else(resource_changed::<AntiAliasing>),
                ),
            );
    }
}

fn sync_widgets(
    mut commands: Commands,
    settings: Res<LightingSettings>,
    wave: Res<SineShapeKey>,
    toggles: Query<(Entity, &BoolSetting)>,
    sliders: Query<(Entity, &FloatSetting)>,
) {
    for (entity, toggle) in &toggles {
        set_checked(&mut commands.entity(entity), toggle.get(&settings));
    }
    for (entity, slider) in &sliders {
        commands
            .entity(entity)
            .insert(SliderValue(slider.get(&settings, &wave)));
    }
}

fn sync_radio_buttons(
    mut commands: Commands,
    settings: Res<LightingSettings>,
    anti_aliasing: Res<AntiAliasing>,
    aa_choices: Query<(Entity, &AntiAliasingChoice)>,
    ssao_choices: Query<(Entity, &SsaoQualityChoice)>,
) {
    for (entity, AntiAliasingChoice(choice)) in &aa_choices {
        set_checked(&mut commands.entity(entity), choice == &*anti_aliasing);
    }
    for (entity, SsaoQualityChoice(quality)) in &ssao_choices {
        set_checked(
            &mut commands.entity(entity),
            *quality == settings.ssao_settings.quality_level,
        );
    }
}

fn set_checked(entity: &mut EntityCommands, checked: bool) {
    if checked {
        entity.insert(Checked);
    } else {
        entity.remove::<Checked>();
    }
}
