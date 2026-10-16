//! Keeps the woofer and graphics settings in
//! `<config dir>/woofersim/settings.toml`, for example
//! `~/.config/woofersim/settings.toml` on Linux.
//!
//! The file starts with `version = <schema version>`. A file with a newer
//! version is left alone and the defaults are used, so running an older
//! build cannot overwrite settings it does not understand. A file that is
//! unreadable as settings is moved aside to `settings.toml.bak`. Keys missing
//! from the file take their default.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bevy::app::OnAppExitSystems;
use bevy::pbr::{ScreenSpaceAmbientOcclusion, ScreenSpaceAmbientOcclusionQualityLevel};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::anti_aliasing::AntiAliasing;
use crate::audio::WooferSettings;
use crate::lighting::{LightingSettings, clamp_or_default, limits};

/// Version of the file layout. Bump it when a change needs more than new keys
/// with defaults, and migrate older files on load.
const SCHEMA_VERSION: u32 = 1;
/// Time after the last change before saving, so dragging a slider writes the
/// file once instead of every frame.
const SAVE_DELAY: Duration = Duration::from_secs(1);

/// Loads the settings into their resources before the plugins that use them
/// are built, and saves them when they change. Add it before those plugins.
pub struct PersistencePlugin;

impl Plugin for PersistencePlugin {
    fn build(&self, app: &mut App) {
        let (store, file) = SettingsStore::open();
        let (lighting, anti_aliasing) = file.graphics.into_resources();
        app.insert_resource(file.woofer.sanitized())
            .insert_resource(lighting)
            .insert_resource(anti_aliasing)
            .insert_resource(store)
            // After the window systems send AppExit, so closing the window
            // flushes a save that is still waiting for its delay.
            .add_systems(Last, save_changed_settings.in_set(OnAppExitSystems));
    }
}

#[derive(Serialize, Deserialize)]
struct SettingsFile {
    version: u32,
    #[serde(default)]
    woofer: WooferSettings,
    #[serde(default)]
    graphics: GraphicsSettings,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            woofer: WooferSettings::default(),
            graphics: GraphicsSettings::default(),
        }
    }
}

/// Reads only the version, to decide how to read the rest.
#[derive(Deserialize)]
struct FileVersion {
    version: Option<u32>,
}

/// The graphics settings as stored, flattened from [`LightingSettings`],
/// whose ambient occlusion settings are a Bevy type, and [`AntiAliasing`].
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct GraphicsSettings {
    raytracing: bool,
    hdri_intensity: f32,
    key_light: bool,
    key_illuminance: f32,
    anti_aliasing: AntiAliasing,
    ssr: bool,
    bloom: bool,
    bloom_intensity: f32,
    ssao: bool,
    ssao_quality: SsaoQuality,
    ssao_radius: f32,
    ssao_thickness: f32,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum SsaoQuality {
    Low,
    Medium,
    High,
    Ultra,
}

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self::from_resources(&LightingSettings::default(), AntiAliasing::default())
    }
}

impl GraphicsSettings {
    fn from_resources(lighting: &LightingSettings, anti_aliasing: AntiAliasing) -> Self {
        use ScreenSpaceAmbientOcclusionQualityLevel as Level;
        let ssao = &lighting.ssao_settings;
        Self {
            raytracing: lighting.raytracing,
            hdri_intensity: lighting.hdri_intensity,
            key_light: lighting.key_light,
            key_illuminance: lighting.key_illuminance,
            anti_aliasing,
            ssr: lighting.ssr,
            bloom: lighting.bloom,
            bloom_intensity: lighting.bloom_intensity,
            ssao: lighting.ssao,
            ssao_quality: match ssao.quality_level {
                Level::Low => SsaoQuality::Low,
                Level::Medium => SsaoQuality::Medium,
                Level::Ultra => SsaoQuality::Ultra,
                // The UI offers no custom quality, so High stands in for it.
                Level::High | Level::Custom { .. } => SsaoQuality::High,
            },
            ssao_radius: ssao.radius,
            ssao_thickness: ssao.constant_object_thickness,
        }
    }

    fn into_resources(self) -> (LightingSettings, AntiAliasing) {
        use ScreenSpaceAmbientOcclusionQualityLevel as Level;
        let quality_level = match self.ssao_quality {
            SsaoQuality::Low => Level::Low,
            SsaoQuality::Medium => Level::Medium,
            SsaoQuality::High => Level::High,
            SsaoQuality::Ultra => Level::Ultra,
        };
        let defaults = LightingSettings::default();
        let lighting = LightingSettings {
            raytracing: self.raytracing,
            hdri_intensity: clamp_or_default(
                self.hdri_intensity,
                limits::HDRI_INTENSITY,
                defaults.hdri_intensity,
            ),
            key_light: self.key_light,
            key_illuminance: clamp_or_default(
                self.key_illuminance,
                limits::KEY_ILLUMINANCE,
                defaults.key_illuminance,
            ),
            ssr: self.ssr,
            bloom: self.bloom,
            bloom_intensity: clamp_or_default(
                self.bloom_intensity,
                limits::BLOOM_INTENSITY,
                defaults.bloom_intensity,
            ),
            ssao: self.ssao,
            ssao_settings: ScreenSpaceAmbientOcclusion {
                quality_level,
                radius: clamp_or_default(
                    self.ssao_radius,
                    limits::SSAO_RADIUS,
                    defaults.ssao_settings.radius,
                ),
                constant_object_thickness: clamp_or_default(
                    self.ssao_thickness,
                    limits::SSAO_THICKNESS,
                    defaults.ssao_settings.constant_object_thickness,
                ),
            },
        };
        (lighting, self.anti_aliasing)
    }
}

#[derive(Resource)]
struct SettingsStore {
    /// None when there is no config directory or the file must not be
    /// overwritten.
    path: Option<PathBuf>,
    save_at: Option<Instant>,
}

impl SettingsStore {
    /// Finds and reads the settings file. Problems are logged and fall back
    /// to the defaults.
    fn open() -> (Self, SettingsFile) {
        let Some(path) = dirs::config_dir().map(|dir| dir.join("woofersim").join("settings.toml"))
        else {
            warn!("No config directory found, settings will not be saved");
            return (Self::without_saving(), SettingsFile::default());
        };
        let (file, writable) = load(&path);
        let store = Self {
            path: writable.then_some(path),
            save_at: None,
        };
        (store, file)
    }

    fn without_saving() -> Self {
        Self {
            path: None,
            save_at: None,
        }
    }
}

/// Returns the settings and whether the file may be overwritten.
fn load(path: &Path) -> (SettingsFile, bool) {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return (SettingsFile::default(), true);
        }
        // Bytes that are not text at all, a broken file rather than one we
        // are not allowed to read.
        Err(error) if error.kind() == io::ErrorKind::InvalidData => {
            return set_aside(path, &error.to_string());
        }
        Err(error) => {
            warn!("Could not read {}, using defaults: {error}", path.display());
            return (SettingsFile::default(), false);
        }
    };
    let version = toml::from_str::<FileVersion>(&text).map(|file| file.version);
    match version {
        Ok(Some(SCHEMA_VERSION)) => match toml::from_str::<SettingsFile>(&text) {
            Ok(file) => {
                info!("Loaded settings from {}", path.display());
                (file, true)
            }
            Err(error) => set_aside(path, &error.to_string()),
        },
        Ok(Some(version)) if version > SCHEMA_VERSION => {
            warn!(
                "{} is version {version}, newer than this build understands \
                 ({SCHEMA_VERSION}). Using defaults and leaving the file alone.",
                path.display()
            );
            (SettingsFile::default(), false)
        }
        Ok(Some(version)) => set_aside(path, &format!("unknown version {version}")),
        Ok(None) => set_aside(path, "no version"),
        Err(error) => set_aside(path, &error.to_string()),
    }
}

/// Moves an unusable settings file to `settings.toml.bak` and returns the
/// defaults, and whether the file may be overwritten: only once it has been
/// moved, so a file that could not be moved is never lost.
fn set_aside(path: &Path, reason: &str) -> (SettingsFile, bool) {
    let backup = path.with_extension("toml.bak");
    match fs::rename(path, &backup) {
        Ok(()) => {
            warn!(
                "Could not use {} ({reason}), moved it to {} and using defaults",
                path.display(),
                backup.display()
            );
            (SettingsFile::default(), true)
        }
        Err(error) => {
            warn!(
                "Could not use {} ({reason}) or move it aside ({error}), using defaults \
                 and not saving",
                path.display()
            );
            (SettingsFile::default(), false)
        }
    }
}

fn save_changed_settings(
    mut store: ResMut<SettingsStore>,
    woofer: Res<WooferSettings>,
    lighting: Res<LightingSettings>,
    anti_aliasing: Res<AntiAliasing>,
    mut exit: MessageReader<AppExit>,
) {
    let changed = |added: bool, changed: bool| changed && !added;
    if changed(woofer.is_added(), woofer.is_changed())
        || changed(lighting.is_added(), lighting.is_changed())
        || changed(anti_aliasing.is_added(), anti_aliasing.is_changed())
    {
        store.save_at = Some(Instant::now() + SAVE_DELAY);
    }
    let exiting = exit.read().count() > 0;
    let Some(save_at) = store.save_at else {
        return;
    };
    if !exiting && Instant::now() < save_at {
        return;
    }
    store.save_at = None;
    let Some(path) = &store.path else {
        return;
    };
    let file = SettingsFile {
        version: SCHEMA_VERSION,
        woofer: *woofer,
        graphics: GraphicsSettings::from_resources(&lighting, *anti_aliasing),
    };
    if let Err(error) = save(path, &file) {
        warn!("Could not save settings to {}: {error}", path.display());
    }
}

/// Writes to a temporary file first and renames it over the old one, so a
/// crash mid-write never leaves a truncated settings file.
fn save(path: &Path, file: &SettingsFile) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, toml::to_string_pretty(file)?)?;
    fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("woofersim-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join("settings.toml")
    }

    #[test]
    fn saved_settings_load_back_unchanged() {
        let path = temp_path("roundtrip");
        let mut file = SettingsFile::default();
        file.woofer.lowpass_hz = 120.0;
        file.woofer.crossover = crate::audio::CrossoverMode::Iir;
        file.graphics.anti_aliasing = AntiAliasing::Smaa;
        file.graphics.ssao_quality = SsaoQuality::Ultra;
        save(&path, &file).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with(&format!("version = {SCHEMA_VERSION}\n")),
            "{text}"
        );

        let (loaded, writable) = load(&path);
        assert!(writable);
        assert!(loaded.woofer == file.woofer);
        assert!(loaded.graphics.anti_aliasing == AntiAliasing::Smaa);
        assert!(matches!(loaded.graphics.ssao_quality, SsaoQuality::Ultra));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn missing_keys_take_their_defaults() {
        let path = temp_path("partial");
        fs::write(&path, "version = 1\n[woofer]\nport_hz = 30.0\n").unwrap();
        let (loaded, _) = load(&path);
        assert_eq!(loaded.woofer.port_hz, 30.0);
        assert!(loaded.woofer.lowpass_hz == WooferSettings::default().lowpass_hz);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn newer_files_are_left_alone() {
        let path = temp_path("newer");
        let text = "version = 99\n[woofer]\nport_hz = 30.0\n";
        fs::write(&path, text).unwrap();
        let (loaded, writable) = load(&path);
        assert!(!writable);
        assert!(loaded.woofer == WooferSettings::default());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn broken_files_are_moved_aside() {
        let path = temp_path("broken");
        fs::write(&path, "version = 1\n[woofer]\nport_hz = \"loud\"\n").unwrap();
        let (loaded, writable) = load(&path);
        assert!(writable);
        assert!(loaded.woofer == WooferSettings::default());
        assert!(!path.exists());
        assert!(path.with_extension("toml.bak").exists());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn files_that_are_not_text_are_moved_aside() {
        let path = temp_path("binary");
        fs::write(&path, [0xff, 0xfe, 0x00, 0x80]).unwrap();
        let (_, writable) = load(&path);
        assert!(writable);
        assert!(path.with_extension("toml.bak").exists());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let text = "version = 1\n[woofer]\nbox_volume_ft3 = 0.0\nresonance_hz = nan\n\
                    [graphics]\nssao_radius = -1.0\n";
        let path = temp_path("range");
        fs::write(&path, text).unwrap();
        let (file, _) = load(&path);
        let woofer = file.woofer.sanitized();
        assert_eq!(woofer.box_volume_ft3, 1.0);
        assert_eq!(woofer.resonance_hz, WooferSettings::default().resonance_hz);
        let (lighting, _) = file.graphics.into_resources();
        assert_eq!(lighting.ssao_settings.radius, 0.01);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
