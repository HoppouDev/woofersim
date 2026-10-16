//! Exposes WooferSim as a PipeWire audio sink and moves the cone with
//! whatever is played into it.

mod driver;
mod dsp;
mod fir;
mod history;
mod sink;

use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Instant;

use bevy::prelude::*;

use crate::shape_key::ShapeKey;
pub use dsp::CrossoverMode;
use dsp::{WooferModel, WooferParams};
use history::ExcursionHistory;
use sink::Sink;

/// Sample rate the frequency response is evaluated at. The audio thread runs
/// at 44.1 or 48 kHz, which makes no visible difference in the bass range.
const RESPONSE_SAMPLE_RATE: f32 = 48_000.0;

pub struct WooferAudioPlugin;

impl Plugin for WooferAudioPlugin {
    fn build(&self, app: &mut App) {
        // Settings loaded from disk are already there, otherwise the defaults.
        let settings = *app.world_mut().get_resource_or_init::<WooferSettings>();
        let shared = Arc::new(Shared::new(settings));
        let sink = Sink::spawn(shared.clone(), settings.audio_output)
            .inspect_err(|error| {
                error!("PipeWire sink unavailable, the cone stays at rest: {error:#}")
            })
            .ok();

        app.insert_resource(AudioLink { shared, sink })
            .add_systems(PreUpdate, drive_cone)
            .add_systems(
                Update,
                send_settings.run_if(resource_changed::<WooferSettings>),
            );
    }
}

/// Filter, enclosure, gain and output settings, editable at runtime and
/// saved to the settings file. Missing fields in the file take their
/// default.
#[derive(Resource, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct WooferSettings {
    /// Crossover lowpass cutoff in Hz.
    pub lowpass_hz: f32,
    /// Free air resonant frequency of the driver in Hz.
    pub resonance_hz: f32,
    /// Ported box when true, sealed box otherwise.
    pub ported: bool,
    /// Port tuning frequency in Hz.
    pub port_hz: f32,
    /// Whether the subsonic highpass is active.
    pub highpass: bool,
    /// Subsonic highpass cutoff in Hz.
    pub highpass_hz: f32,
    /// Net internal box volume in ft³.
    pub box_volume_ft3: f32,
    /// Q of the ported box losses. Lower is lossier.
    pub box_ql: f32,
    /// Output impedance of the amplifier driving the woofer, in Ω.
    pub amp_ohm: f32,
    /// Voice coil inductance in mH.
    pub voice_coil_mh: f32,
    /// Master level in dB. Scales the cone movement, where at 0 dB a full
    /// scale signal far below resonance moves a driver in free air to a shape
    /// key weight of ±1, and the whole audio output.
    pub master_gain_db: f32,
    /// Play the input through the default output with its bass replaced by
    /// the simulated subwoofer.
    pub audio_output: bool,
    /// Level of the simulated bass in the audio output in dB, on top of
    /// [`master_gain_db`](Self::master_gain_db).
    pub bass_mix_db: f32,
    /// Whether the highpassed treble is part of the audio output. Without it
    /// only the simulated subwoofer plays.
    pub treble_output: bool,
    /// Level of the highpassed treble in the audio output in dB, on top of
    /// [`master_gain_db`](Self::master_gain_db).
    pub treble_mix_db: f32,
    /// Linear phase FIR or minimum phase IIR crossover.
    pub crossover: CrossoverMode,
}

impl Default for WooferSettings {
    fn default() -> Self {
        Self {
            lowpass_hz: 90.0,
            resonance_hz: 22.0,
            ported: true,
            port_hz: 20.0,
            highpass: true,
            highpass_hz: 18.0,
            box_volume_ft3: 6.0,
            box_ql: 7.0,
            amp_ohm: 0.0,
            voice_coil_mh: driver::VOICE_COIL_MH,
            master_gain_db: 0.0,
            audio_output: false,
            bass_mix_db: 0.0,
            treble_output: true,
            treble_mix_db: 0.0,
            crossover: CrossoverMode::default(),
        }
    }
}

/// Valid ranges of the woofer settings, shared by the sliders and the check
/// of settings loaded from disk.
pub mod limits {
    use std::ops::RangeInclusive;

    pub const CROSSOVER_HZ: RangeInclusive<f32> = 20.0..=300.0;
    pub const RESONANCE_HZ: RangeInclusive<f32> = 15.0..=150.0;
    pub const PORT_HZ: RangeInclusive<f32> = 15.0..=100.0;
    pub const BOX_VOLUME_FT3: RangeInclusive<f32> = 1.0..=20.0;
    pub const BOX_QL: RangeInclusive<f32> = 2.0..=20.0;
    pub const AMP_OHM: RangeInclusive<f32> = 0.0..=4.0;
    pub const VOICE_COIL_MH: RangeInclusive<f32> = 0.0..=5.0;
    pub const SUBSONIC_HZ: RangeInclusive<f32> = 5.0..=60.0;
    pub const GAIN_DB: RangeInclusive<f32> = -48.0..=48.0;
}

impl WooferSettings {
    /// The settings with every value clamped to its range, and values that
    /// are not finite replaced by their default. Out of range values, from
    /// a hand edited settings file for example, would otherwise turn the
    /// filters unstable or non-finite.
    pub fn sanitized(self) -> Self {
        let defaults = Self::default();
        let clamp = |value: f32, range: std::ops::RangeInclusive<f32>, default: f32| {
            if value.is_finite() {
                value.clamp(*range.start(), *range.end())
            } else {
                default
            }
        };
        Self {
            lowpass_hz: clamp(self.lowpass_hz, limits::CROSSOVER_HZ, defaults.lowpass_hz),
            resonance_hz: clamp(
                self.resonance_hz,
                limits::RESONANCE_HZ,
                defaults.resonance_hz,
            ),
            port_hz: clamp(self.port_hz, limits::PORT_HZ, defaults.port_hz),
            highpass_hz: clamp(self.highpass_hz, limits::SUBSONIC_HZ, defaults.highpass_hz),
            box_volume_ft3: clamp(
                self.box_volume_ft3,
                limits::BOX_VOLUME_FT3,
                defaults.box_volume_ft3,
            ),
            box_ql: clamp(self.box_ql, limits::BOX_QL, defaults.box_ql),
            amp_ohm: clamp(self.amp_ohm, limits::AMP_OHM, defaults.amp_ohm),
            voice_coil_mh: clamp(
                self.voice_coil_mh,
                limits::VOICE_COIL_MH,
                defaults.voice_coil_mh,
            ),
            master_gain_db: clamp(
                self.master_gain_db,
                limits::GAIN_DB,
                defaults.master_gain_db,
            ),
            bass_mix_db: clamp(self.bass_mix_db, limits::GAIN_DB, defaults.bass_mix_db),
            treble_mix_db: clamp(self.treble_mix_db, limits::GAIN_DB, defaults.treble_mix_db),
            ..self
        }
    }
}

/// State shared with the realtime audio thread, which must not block.
struct Shared {
    lowpass_hz: AtomicF32,
    resonance_hz: AtomicF32,
    ported: AtomicBool,
    port_hz: AtomicF32,
    highpass: AtomicBool,
    highpass_hz: AtomicF32,
    box_volume_ft3: AtomicF32,
    box_ql: AtomicF32,
    amp_ohm: AtomicF32,
    voice_coil_mh: AtomicF32,
    master_gain_db: AtomicF32,
    bass_mix_db: AtomicF32,
    treble_output: AtomicBool,
    treble_mix_db: AtomicF32,
    crossover_fir: AtomicBool,
    excursion: ExcursionHistory,
}

impl Shared {
    fn new(settings: WooferSettings) -> Self {
        let shared = Self {
            lowpass_hz: AtomicF32::new(0.0),
            resonance_hz: AtomicF32::new(0.0),
            ported: AtomicBool::new(false),
            port_hz: AtomicF32::new(0.0),
            highpass: AtomicBool::new(false),
            highpass_hz: AtomicF32::new(0.0),
            box_volume_ft3: AtomicF32::new(0.0),
            box_ql: AtomicF32::new(0.0),
            amp_ohm: AtomicF32::new(0.0),
            voice_coil_mh: AtomicF32::new(0.0),
            master_gain_db: AtomicF32::new(0.0),
            bass_mix_db: AtomicF32::new(0.0),
            treble_output: AtomicBool::new(false),
            treble_mix_db: AtomicF32::new(0.0),
            crossover_fir: AtomicBool::new(false),
            excursion: ExcursionHistory::default(),
        };
        shared.store(&settings.sanitized());
        shared
    }

    fn store(&self, settings: &WooferSettings) {
        self.lowpass_hz.store(settings.lowpass_hz);
        self.resonance_hz.store(settings.resonance_hz);
        self.ported.store(settings.ported, Ordering::Relaxed);
        self.port_hz.store(settings.port_hz);
        self.highpass.store(settings.highpass, Ordering::Relaxed);
        self.highpass_hz.store(settings.highpass_hz);
        self.box_volume_ft3.store(settings.box_volume_ft3);
        self.box_ql.store(settings.box_ql);
        self.amp_ohm.store(settings.amp_ohm);
        self.voice_coil_mh.store(settings.voice_coil_mh);
        self.master_gain_db.store(settings.master_gain_db);
        self.bass_mix_db.store(settings.bass_mix_db);
        self.treble_output
            .store(settings.treble_output, Ordering::Relaxed);
        self.treble_mix_db.store(settings.treble_mix_db);
        self.crossover_fir
            .store(settings.crossover == CrossoverMode::Fir, Ordering::Relaxed);
    }

    fn woofer_params(&self, sample_rate: f32) -> WooferParams {
        WooferSettings {
            lowpass_hz: self.lowpass_hz.load(),
            resonance_hz: self.resonance_hz.load(),
            ported: self.ported.load(Ordering::Relaxed),
            port_hz: self.port_hz.load(),
            highpass: self.highpass.load(Ordering::Relaxed),
            highpass_hz: self.highpass_hz.load(),
            box_volume_ft3: self.box_volume_ft3.load(),
            box_ql: self.box_ql.load(),
            amp_ohm: self.amp_ohm.load(),
            voice_coil_mh: self.voice_coil_mh.load(),
            master_gain_db: self.master_gain_db.load(),
            // The audio thread never looks at this, the sink handles it.
            audio_output: false,
            bass_mix_db: self.bass_mix_db.load(),
            treble_output: self.treble_output.load(Ordering::Relaxed),
            treble_mix_db: self.treble_mix_db.load(),
            crossover: if self.crossover_fir.load(Ordering::Relaxed) {
                CrossoverMode::Fir
            } else {
                CrossoverMode::Iir
            },
        }
        .params(sample_rate)
    }
}

impl WooferSettings {
    fn params(&self, sample_rate: f32) -> WooferParams {
        WooferParams {
            sample_rate,
            lowpass_hz: self.lowpass_hz,
            resonance_hz: self.resonance_hz,
            ported: self.ported,
            port_hz: self.port_hz,
            highpass: self.highpass,
            highpass_hz: self.highpass_hz,
            box_volume_ft3: self.box_volume_ft3,
            box_ql: self.box_ql,
            amp_ohm: self.amp_ohm,
            voice_coil_mh: self.voice_coil_mh,
            bass_gain_db: self.master_gain_db + self.bass_mix_db,
            treble: self.treble_output,
            treble_gain_db: self.master_gain_db + self.treble_mix_db,
            crossover: self.crossover,
        }
    }

    /// The simulated subwoofer at each frequency, for a full scale sine.
    pub fn frequency_response(&self, frequencies: &[f32]) -> Vec<ResponsePoint> {
        let params = self.params(RESPONSE_SAMPLE_RATE);
        let mut model = WooferModel::default();
        model.configure(params);
        let master_gain = 10f32.powf(self.master_gain_db / 20.0);
        // The FIR crossover's constant delay, which the model leaves out.
        let crossover_delay = match self.crossover {
            CrossoverMode::Fir => {
                fir::latency(self.lowpass_hz, RESPONSE_SAMPLE_RATE) as f32 / RESPONSE_SAMPLE_RATE
            }
            CrossoverMode::Iir => 0.0,
        };
        frequencies
            .iter()
            .map(|&frequency| {
                let (pressure, excursion) = model.frequency_response(frequency);
                // Group delay is minus the slope of the phase, taken over a
                // small step. The ratio keeps the phase difference unwrapped.
                let step = frequency * 1e-3;
                let (next, _) = model.frequency_response(frequency + step);
                let phase_step = (next / pressure).arg() as f32;
                let impedance = driver::impedance(&params, frequency);
                ResponsePoint {
                    level_db: 20.0 * (pressure.norm() as f32).max(1e-6).log10(),
                    phase_deg: (pressure.arg() as f32).to_degrees(),
                    group_delay_ms: (-phase_step / (TAU * step) + crossover_delay) * 1e3,
                    excursion: excursion.norm() as f32 * master_gain,
                    impedance_ohm: impedance.norm() as f32,
                    impedance_phase_deg: (impedance.arg() as f32).to_degrees(),
                }
            })
            .collect()
    }
}

/// The simulated subwoofer at one frequency.
pub struct ResponsePoint {
    /// Sound pressure relative to the input, with the master gain and bass
    /// mix applied.
    pub level_db: f32,
    /// Phase of the sound pressure in degrees, without the constant delay
    /// of the FIR crossover.
    pub phase_deg: f32,
    /// Group delay of the sound pressure in ms, including the FIR crossover.
    pub group_delay_ms: f32,
    /// Cone excursion as a shape key weight, with the master gain applied.
    pub excursion: f32,
    /// Electrical impedance at the driver's terminals, in Ω.
    pub impedance_ohm: f32,
    pub impedance_phase_deg: f32,
}

struct AtomicF32(AtomicU32);

impl AtomicF32 {
    fn new(value: f32) -> Self {
        Self(AtomicU32::new(value.to_bits()))
    }

    fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    fn store(&self, value: f32) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}

#[derive(Resource)]
struct AudioLink {
    shared: Arc<Shared>,
    /// Kept so the sink lives as long as the app, and to switch the output.
    sink: Option<Sink>,
}

fn send_settings(
    settings: Res<WooferSettings>,
    link: Res<AudioLink>,
    mut audio_output: Local<Option<bool>>,
) {
    link.shared.store(&settings.sanitized());
    if *audio_output != Some(settings.audio_output) {
        *audio_output = Some(settings.audio_output);
        if let Some(sink) = &link.sink {
            sink.set_output(settings.audio_output);
        }
    }
}

/// How often the cone position is sampled, in Hz. The ticks sit on a fixed
/// grid in wall clock time, so the cone moves exactly this many times per
/// second however fast frames are rendered. A tone at this frequency, or a
/// multiple of it, then appears to stand still, like under a strobe light.
const CONE_UPDATE_RATE: f64 = 30.0;

/// Excursion at the most recent tick, and which tick that was.
#[derive(Default)]
struct ConeTick {
    index: Option<i64>,
    excursion: f32,
}

fn drive_cone(
    link: Res<AudioLink>,
    settings: Res<WooferSettings>,
    mut tick: Local<ConeTick>,
    mut shape_key: ResMut<ShapeKey>,
) {
    let history = &link.shared.excursion;
    let index = (history.seconds(Instant::now()) * CONE_UPDATE_RATE).floor() as i64;
    if tick.index != Some(index) {
        tick.index = Some(index);
        tick.excursion = history.sample_at(index as f64 / CONE_UPDATE_RATE);
    }
    let gain = 10f32.powf(settings.master_gain_db / 20.0);
    shape_key.weight = tick.excursion * gain;
}
