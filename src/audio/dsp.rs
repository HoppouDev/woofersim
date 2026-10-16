//! Filters that turn the incoming audio signal into woofer cone excursion and
//! into the sound of the simulated subwoofer.
//!
//! The bass chain is an optional subsonic highpass, a crossover lowpass like
//! the one feeding a subwoofer, then the driver in its enclosure, following
//! the Thiele/Small model (R. H. Small, "Vented-Box Loudspeaker Systems",
//! 1973).
//!
//! The driver is a mass on a spring that resonates at its free air resonance.
//! In a sealed box the trapped air adds stiffness, so the system resonates
//! higher and the cone moves less. In a ported box the air in the port
//! resonates at the tuning frequency and does most of the work there, so the
//! cone barely moves at tuning. Below tuning the port unloads the cone and
//! excursion climbs toward the free air level, which is what the subsonic
//! highpass protects against.
//!
//! Excursion is normalized so that a full scale signal far below resonance
//! moves the cone of a driver in free air by 1.
//!
//! The audio output is the simulated subwoofer's sound pressure, normalized to
//! unity in its passband, plus the stereo input highpassed at the crossover
//! frequency. The crossover has 4th order Linkwitz-Riley magnitudes, so with
//! an ideal subwoofer the two halves sum back to the input level. It is
//! either linear phase FIR (see [`super::fir`]) or minimum phase IIR.

use std::f64::consts::{FRAC_1_SQRT_2, PI, TAU};

use num_complex::Complex64;

use super::driver;
use super::fir::{FirCrossover, FirSlot, MAX_FRAMES};

/// How the crossover splits bass from treble.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CrossoverMode {
    /// Linear phase: no phase shift between the halves, at the cost of a
    /// delay of 6 periods of the crossover frequency.
    #[default]
    Fir,
    /// Minimum phase: no delay, but the phase turns through the crossover.
    Iir,
}

/// Q of each of the two cascaded Butterworth sections that make up a 4th
/// order Linkwitz-Riley crossover.
const CROSSOVER_Q: f64 = FRAC_1_SQRT_2;

/// Q values of the two sections of a 4th order Butterworth highpass.
const SUBSONIC_Q: [f64; 2] = [0.541_196_1, 1.306_563];

/// Change of the FIR share per sample when switching crossover modes, a
/// 2048 sample (43 ms at 48 kHz) crossfade.
const MODE_FADE_STEP: f64 = 1.0 / 2048.0;

// The driver's parameters are in [`super::driver`]. The free air resonance
// is the default of the resonant frequency slider, and the default box is
// the 6 ft³ vented box Dayton recommends for the UMII18-22.

/// Highest filter frequency relative to the sample rate. The bilinear
/// transform breaks down at Nyquist, so slider values past it are clamped.
const MAX_RELATIVE_FREQUENCY: f64 = 0.45;

/// Second order section in transposed direct form II.
///
/// Runs in f64 because the poles of a 20 Hz filter at 48 kHz sit so close to
/// the unit circle that f32 coefficients make it drift or ring.
#[derive(Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    /// Discretizes `num(s) / den(s)` with the bilinear transform. Both
    /// polynomials are given in ascending powers of s (rad/s). The filter
    /// state is kept so the output does not jump while a slider is dragged.
    fn set_analog(&mut self, num: [f64; 3], den: [f64; 3], sample_rate: f64) {
        let k = 2.0 * sample_rate;
        let k2 = k * k;
        let bilinear = |p: [f64; 3]| {
            [
                p[2] * k2 + p[1] * k + p[0],
                2.0 * (p[0] - p[2] * k2),
                p[2] * k2 - p[1] * k + p[0],
            ]
        };
        let b = bilinear(num);
        let a = bilinear(den);
        self.b = b.map(|x| x / a[0]);
        self.a = [a[1] / a[0], a[2] / a[0]];
    }

    /// Butterworth style lowpass with the cutoff prewarped so it lands
    /// exactly on `frequency`. Matches the RBJ audio EQ cookbook.
    fn set_lowpass(&mut self, frequency: f64, q: f64, sample_rate: f64) {
        let w = prewarp(frequency, sample_rate);
        self.set_analog([w * w, 0.0, 0.0], [w * w, w / q, 1.0], sample_rate);
    }

    fn set_highpass(&mut self, frequency: f64, q: f64, sample_rate: f64) {
        let w = prewarp(frequency, sample_rate);
        self.set_analog([0.0, 0.0, 1.0], [w * w, w / q, 1.0], sample_rate);
    }

    /// First order lowpass with its corner prewarped onto `frequency`.
    fn set_first_order_lowpass(&mut self, frequency: f64, sample_rate: f64) {
        let w = prewarp(frequency, sample_rate);
        let k = 2.0 * sample_rate;
        let a0 = k + w;
        self.b = [w / a0, w / a0, 0.0];
        self.a = [(w - k) / a0, 0.0];
    }

    /// Passes the signal through unchanged.
    fn set_identity(&mut self) {
        self.b = [1.0, 0.0, 0.0];
        self.a = [0.0, 0.0];
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        // The state is kept across reconfiguration, so one NaN or infinity
        // would otherwise stay in the filter for good.
        if y.is_finite() {
            y
        } else {
            self.z = [0.0; 2];
            0.0
        }
    }

    /// Complex gain at the frequency where z⁻¹ = `z_inv`.
    fn response(&self, z_inv: Complex64) -> Complex64 {
        let num = self.b[0] + (self.b[1] + self.b[2] * z_inv) * z_inv;
        let den = 1.0 + (self.a[0] + self.a[1] * z_inv) * z_inv;
        num / den
    }
}

fn cascade_response(sections: &[Biquad], z_inv: Complex64) -> Complex64 {
    sections
        .iter()
        .map(|section| section.response(z_inv))
        .product()
}

/// Analog angular frequency that the bilinear transform maps onto `frequency`.
fn prewarp(frequency: f64, sample_rate: f64) -> f64 {
    let frequency = frequency.min(sample_rate * MAX_RELATIVE_FREQUENCY);
    2.0 * sample_rate * (PI * frequency / sample_rate).tan()
}

/// Filter parameters, frequencies in Hz.
#[derive(Clone, Copy, PartialEq)]
pub struct WooferParams {
    pub sample_rate: f32,
    pub lowpass_hz: f32,
    /// Free air resonance of the driver.
    pub resonance_hz: f32,
    pub ported: bool,
    pub port_hz: f32,
    pub highpass: bool,
    pub highpass_hz: f32,
    /// Net internal box volume (Vb) in ft³.
    pub box_volume_ft3: f32,
    /// Q of the ported box losses (leakage, port friction). Lower is lossier.
    /// 7 is the usual assumption for a real box.
    pub box_ql: f32,
    /// Output impedance of the amplifier driving the woofer, in Ω.
    pub amp_ohm: f32,
    /// Voice coil inductance (Le) in mH.
    pub voice_coil_mh: f32,
    /// Level of the simulated subwoofer in the audio output, in dB. The
    /// treble and the excursion are not affected.
    pub bass_gain_db: f32,
    /// Whether the highpassed input is part of the audio output at all.
    pub treble: bool,
    /// Level of the highpassed input in the audio output, in dB.
    pub treble_gain_db: f32,
    pub crossover: CrossoverMode,
}

/// Result of one stereo input frame.
#[derive(Clone, Copy, Default)]
pub struct WooferFrame {
    /// Cone excursion, see the module documentation for its scale.
    pub excursion: f32,
    /// Simulated subwoofer plus highpassed input, left and right.
    pub output: [f32; 2],
}

pub struct WooferModel {
    subsonic: [Biquad; 2],
    crossover: [Biquad; 2],
    /// Highpass half of the crossover for each output channel.
    treble: [[Biquad; 2]; 2],
    fir: FirSlot,
    fir_low: Vec<[f32; 2]>,
    fir_high: Vec<[f32; 2]>,
    sealed_excursion: Biquad,
    sealed_pressure: Biquad,
    ported_excursion: [Biquad; 2],
    ported_pressure: [Biquad; 2],
    /// Voice coil inductance lowpass on the drive.
    voice_coil: Biquad,
    /// Level change from the amplifier's output impedance.
    drive_gain: f64,
    /// Linear forms of [`WooferParams::bass_gain_db`] and
    /// [`WooferParams::treble_gain_db`].
    bass_gain: f64,
    treble_gain: f64,
    /// Share of the FIR crossover in the output, 0 for IIR and 1 for FIR.
    /// It ramps between them so switching modes does not click.
    fir_mix: f64,
    params: Option<WooferParams>,
}

impl Default for WooferModel {
    fn default() -> Self {
        Self {
            subsonic: Default::default(),
            crossover: Default::default(),
            treble: Default::default(),
            fir: FirSlot::default(),
            fir_low: vec![[0.0; 2]; MAX_FRAMES],
            fir_high: vec![[0.0; 2]; MAX_FRAMES],
            sealed_excursion: Biquad::default(),
            sealed_pressure: Biquad::default(),
            ported_excursion: Default::default(),
            ported_pressure: Default::default(),
            voice_coil: Biquad::default(),
            drive_gain: 1.0,
            bass_gain: 0.0,
            treble_gain: 0.0,
            fir_mix: 0.0,
            params: None,
        }
    }
}

impl WooferModel {
    /// Recomputes the coefficients when any parameter changed. Cheap to call
    /// for every buffer.
    pub fn configure(&mut self, params: WooferParams) {
        if self.params == Some(params) {
            return;
        }
        let sample_rate = f64::from(params.sample_rate);
        for (section, q) in self.subsonic.iter_mut().zip(SUBSONIC_Q) {
            section.set_highpass(f64::from(params.highpass_hz), q, sample_rate);
        }
        let crossover_hz = f64::from(params.lowpass_hz);
        for section in &mut self.crossover {
            section.set_lowpass(crossover_hz, CROSSOVER_Q, sample_rate);
        }
        for section in self.treble.iter_mut().flatten() {
            section.set_highpass(crossover_hz, CROSSOVER_Q, sample_rate);
        }

        let ws = TAU * f64::from(params.resonance_hz);
        let wb = TAU * f64::from(params.port_hz);
        let qts = driver::total_q(params.amp_ohm);
        // Driver compliance relative to the box air compliance (Vas / Vb).
        // Larger values mean a smaller box.
        let compliance_ratio = driver::VAS_FT3 / f64::from(params.box_volume_ft3);
        self.set_sealed(ws, qts, compliance_ratio, sample_rate);
        self.set_ported(
            ws,
            wb,
            qts,
            compliance_ratio,
            f64::from(params.box_ql),
            sample_rate,
        );
        match driver::inductance_corner_hz(params.amp_ohm, params.voice_coil_mh) {
            Some(corner) => self.voice_coil.set_first_order_lowpass(corner, sample_rate),
            None => self.voice_coil.set_identity(),
        }
        self.drive_gain = driver::drive_gain(params.amp_ohm);
        self.bass_gain = decibels_to_gain(params.bass_gain_db);
        self.treble_gain = if params.treble {
            decibels_to_gain(params.treble_gain_db)
        } else {
            0.0
        };
        self.params = Some(params);
    }

    /// The box air stiffens the suspension: resonance and Q both rise by
    /// sqrt(1 + Vas/Vb) and excursion at low frequencies drops by 1 + Vas/Vb.
    /// The sound pressure is a 2nd order highpass at the system resonance.
    fn set_sealed(&mut self, ws: f64, qts: f64, compliance_ratio: f64, sample_rate: f64) {
        let stiffening = (1.0 + compliance_ratio).sqrt();
        let wc = ws * stiffening;
        let qtc = qts * stiffening;
        let den = [wc * wc, wc / qtc, 1.0];
        self.sealed_excursion
            .set_analog([ws * ws, 0.0, 0.0], den, sample_rate);
        self.sealed_pressure
            .set_analog([0.0, 0.0, 1.0], den, sample_rate);
    }

    /// Excursion and sound pressure of a vented box (Small 1973):
    ///
    /// X(s) = (s²Tb² + sTb/Ql + 1) / D(s)
    /// P(s) = s⁴Ts²Tb² / D(s)
    ///
    /// D(s) is 4th order. It is factored into two quadratics so each runs as
    /// its own biquad, which stays accurate at low frequencies where a single
    /// 4th order section would not.
    fn set_ported(&mut self, ws: f64, wb: f64, qts: f64, alpha: f64, ql: f64, sample_rate: f64) {
        let (ts, tb) = (1.0 / ws, 1.0 / wb);
        let d = [
            1.0,
            ts / qts + tb / ql,
            ts * ts + (alpha + 1.0) * tb * tb + ts * tb / (ql * qts),
            ts * tb * tb / qts + ts * ts * tb / ql,
            ts * ts * tb * tb,
        ];
        let [qa, qb] = factor_quartic(d, (ws * wb).sqrt());
        // d[4] * qa(0) * qb(0) = d[0] = 1, so both sections have unity DC gain.
        let scale = 1.0 / (d[4] * qb[0]);
        self.ported_excursion[0].set_analog(
            [scale, scale * tb / ql, scale * tb * tb],
            [qa[0], qa[1], 1.0],
            sample_rate,
        );
        self.ported_excursion[1].set_analog([qb[0], 0.0, 0.0], [qb[0], qb[1], 1.0], sample_rate);
        // The numerator s⁴d[4] cancels the leading coefficient of D(s), leaving
        // two highpass sections with unity gain at high frequencies.
        self.ported_pressure[0].set_analog([0.0, 0.0, 1.0], [qa[0], qa[1], 1.0], sample_rate);
        self.ported_pressure[1].set_analog([0.0, 0.0, 1.0], [qb[0], qb[1], 1.0], sample_rate);
    }

    /// Queues a FIR crossover built for the current crossover frequency and
    /// sample rate. Returns a superseded one, to be freed off the audio
    /// thread.
    pub fn install_fir(&mut self, crossover: Box<FirCrossover>) -> Option<Box<FirCrossover>> {
        self.fir.install(crossover)
    }

    /// A FIR crossover that went out of use, to be freed off the audio
    /// thread.
    pub fn take_retired_fir(&mut self) -> Option<Box<FirCrossover>> {
        self.fir.take_retired()
    }

    /// Filters stereo frames, calling `emit` with the result of each.
    /// [`configure`](Self::configure) must have been called first. In FIR
    /// mode the IIR crossover stands in until a FIR crossover is installed.
    pub fn process_block(&mut self, input: &[[f32; 2]], mut emit: impl FnMut(WooferFrame)) {
        let Some(params) = self.params else {
            input.iter().for_each(|_| emit(WooferFrame::default()));
            return;
        };
        for chunk in input.chunks(MAX_FRAMES) {
            let frames = chunk.len();
            let fir_low = &mut self.fir_low[..frames];
            let fir_high = &mut self.fir_high[..frames];
            // Every filter runs all the time, the FIR crossover included, so
            // switching modes or toggling a stage never resumes from stale
            // state.
            let fir_ready = self.fir.process(chunk, fir_low, fir_high);
            let fir_target = if fir_ready && params.crossover == CrossoverMode::Fir {
                1.0
            } else {
                0.0
            };

            for (i, &[left, right]) in chunk.iter().enumerate() {
                let (left, right) = (f64::from(left), f64::from(right));
                let iir_low = cascade(&mut self.crossover, (left + right) / 2.0);
                let iir_high = [
                    cascade(&mut self.treble[0], left),
                    cascade(&mut self.treble[1], right),
                ];
                // The FIR path is delayed, the IIR path is not, so a hard
                // switch would jump in time. A short crossfade hides it.
                let step = (fir_target - self.fir_mix).clamp(-MODE_FADE_STEP, MODE_FADE_STEP);
                self.fir_mix += step;
                let (low, high) = if self.fir_mix > 0.0 {
                    let [low_left, low_right] = fir_low[i];
                    let [high_left, high_right] = fir_high[i];
                    let fir_low = (f64::from(low_left) + f64::from(low_right)) / 2.0;
                    let mix = |iir: f64, fir: f64| iir + (fir - iir) * self.fir_mix;
                    (
                        mix(iir_low, fir_low),
                        [
                            mix(iir_high[0], f64::from(high_left)),
                            mix(iir_high[1], f64::from(high_right)),
                        ],
                    )
                } else {
                    (iir_low, iir_high)
                };

                let highpassed = cascade(&mut self.subsonic, low);
                let low = if params.highpass { highpassed } else { low };
                let driven = self.voice_coil.process(low) * self.drive_gain;
                let sealed_excursion = self.sealed_excursion.process(driven);
                let sealed_pressure = self.sealed_pressure.process(driven);
                let ported_excursion = cascade(&mut self.ported_excursion, driven);
                let ported_pressure = cascade(&mut self.ported_pressure, driven);
                let (excursion, bass) = if params.ported {
                    (ported_excursion, ported_pressure)
                } else {
                    (sealed_excursion, sealed_pressure)
                };

                let bass = bass * self.bass_gain;
                emit(WooferFrame {
                    excursion: excursion as f32,
                    output: [
                        (high[0] * self.treble_gain + bass) as f32,
                        (high[1] * self.treble_gain + bass) as f32,
                    ],
                });
            }
        }
    }

    /// Complex gain of the simulated subwoofer's sound pressure (including
    /// [`WooferParams::bass_gain_db`]) and of the cone excursion for a sine
    /// at `frequency` played equally on both channels. Evaluates the same
    /// digital filters [`process_block`](Self::process_block) runs in IIR
    /// mode, so it matches the simulation exactly. In FIR mode the crossover
    /// counts with its magnitude only, which matches the FIR crossover to
    /// within 0.1 dB, and without its constant delay. Both are 0 before
    /// [`configure`](Self::configure).
    pub fn frequency_response(&self, frequency: f32) -> (Complex64, Complex64) {
        let Some(params) = self.params else {
            return (Complex64::ZERO, Complex64::ZERO);
        };
        let omega = TAU * f64::from(frequency) / f64::from(params.sample_rate);
        let z_inv = Complex64::from_polar(1.0, -omega);
        let crossover = cascade_response(&self.crossover, z_inv);
        let mut drive = match params.crossover {
            CrossoverMode::Iir => crossover,
            CrossoverMode::Fir => Complex64::from(crossover.norm()),
        };
        if params.highpass {
            drive *= cascade_response(&self.subsonic, z_inv);
        }
        drive *= self.voice_coil.response(z_inv) * self.drive_gain;
        let (excursion, pressure) = if params.ported {
            (
                cascade_response(&self.ported_excursion, z_inv),
                cascade_response(&self.ported_pressure, z_inv),
            )
        } else {
            (
                self.sealed_excursion.response(z_inv),
                self.sealed_pressure.response(z_inv),
            )
        };
        (drive * pressure * self.bass_gain, drive * excursion)
    }
}

fn decibels_to_gain(decibels: f32) -> f64 {
    10f64.powf(f64::from(decibels) / 20.0)
}

fn cascade(sections: &mut [Biquad], input: f64) -> f64 {
    sections
        .iter_mut()
        .fold(input, |signal, section| section.process(signal))
}

/// Splits the monic part of `d[4]s⁴ + d[3]s³ + d[2]s² + d[1]s + d[0]` into
/// two real quadratics `s² + q[1]s + q[0]`. `scale` is the rough magnitude of
/// the roots and keeps the root finder well conditioned.
fn factor_quartic(d: [f64; 5], scale: f64) -> [[f64; 2]; 2] {
    // Roots of the polynomial in p = s / scale, made monic.
    let c: [f64; 4] = std::array::from_fn(|k| d[k] * scale.powi(k as i32) / (d[4] * scale.powi(4)));
    let poly = |p: Complex64| (((p + c[3]) * p + c[2]) * p + c[1]) * p + c[0];

    // Durand-Kerner iteration from the customary non-symmetric start points.
    let seed = Complex64::new(0.4, 0.9);
    let mut roots: [Complex64; 4] = std::array::from_fn(|k| seed.powu(k as u32));
    for _ in 0..200 {
        for i in 0..4 {
            let denominator: Complex64 = (0..4)
                .filter(|&j| j != i)
                .map(|j| roots[i] - roots[j])
                .product();
            roots[i] -= poly(roots[i]) / denominator;
        }
    }

    // Sorting by imaginary part puts conjugates at opposite ends, so the
    // outer and inner pairs each multiply out to a real quadratic. Real roots
    // land in the middle and pair with each other or with nothing complex.
    roots.sort_by(|a, b| b.im.total_cmp(&a.im));
    let quadratic =
        |r1: Complex64, r2: Complex64| [(r1 * r2).re * scale * scale, -(r1 + r2).re * scale];
    [quadratic(roots[0], roots[3]), quadratic(roots[1], roots[2])]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: f32 = 48_000.0;

    const BASE: WooferParams = WooferParams {
        sample_rate: SAMPLE_RATE,
        lowpass_hz: 200.0,
        resonance_hz: 30.0,
        ported: false,
        port_hz: 35.0,
        highpass: false,
        highpass_hz: 18.0,
        box_volume_ft3: 6.0,
        box_ql: 7.0,
        amp_ohm: 0.0,
        voice_coil_mh: 0.0,
        bass_gain_db: 0.0,
        treble: true,
        treble_gain_db: 0.0,
        crossover: CrossoverMode::Iir,
    };

    const PORTED: WooferParams = WooferParams {
        ported: true,
        ..BASE
    };

    fn model(params: WooferParams) -> WooferModel {
        let mut model = WooferModel::default();
        model.configure(params);
        if params.crossover == CrossoverMode::Fir {
            model.install_fir(Box::new(FirCrossover::new(
                params.lowpass_hz,
                params.sample_rate,
            )));
        }
        model
    }

    /// Peak absolute values of one model output.
    #[derive(Default)]
    struct Peaks {
        excursion: f32,
        left: f32,
        right: f32,
    }

    /// Peaks for a sine with the given amplitude on each channel, measured
    /// after two seconds of settling.
    fn peaks(params: WooferParams, frequency: f32, [left, right]: [f32; 2]) -> Peaks {
        let mut model = model(params);
        let samples = SAMPLE_RATE as usize * 2;
        let sine = |n: usize| (std::f32::consts::TAU * frequency * n as f32 / SAMPLE_RATE).sin();
        let input = |range: std::ops::Range<usize>| -> Vec<[f32; 2]> {
            range.map(|n| [left * sine(n), right * sine(n)]).collect()
        };
        model.process_block(&input(0..samples), |_| {});
        let mut peaks = Peaks::default();
        model.process_block(&input(samples..2 * samples), |frame| {
            peaks.excursion = peaks.excursion.max(frame.excursion.abs());
            peaks.left = peaks.left.max(frame.output[0].abs());
            peaks.right = peaks.right.max(frame.output[1].abs());
        });
        peaks
    }

    fn peak_excursion(params: WooferParams, frequency: f32) -> f32 {
        peaks(params, frequency, [1.0, 1.0]).excursion
    }

    fn peak_output(params: WooferParams, frequency: f32) -> f32 {
        peaks(params, frequency, [1.0, 1.0]).left
    }

    fn settled(params: WooferParams) -> f32 {
        let mut model = model(params);
        let mut last = 0.0;
        model.process_block(&vec![[1.0; 2]; 96_000], |frame| last = frame.excursion);
        last
    }

    #[test]
    fn sealed_box_stiffens_the_suspension() {
        for box_volume_ft3 in [3.0, 6.0, 12.0] {
            let sealed = settled(WooferParams {
                box_volume_ft3,
                ..BASE
            });
            let expected = 1.0 / (1.0 + driver::VAS_FT3 / f64::from(box_volume_ft3)) as f32;
            assert!(
                (sealed - expected).abs() < 1e-3,
                "{box_volume_ft3} ft³ settled at {sealed}, expected {expected}"
            );
        }
    }

    #[test]
    fn ported_box_leaks_static_pressure() {
        let ported = settled(PORTED);
        assert!((ported - 1.0).abs() < 1e-3, "settled at {ported}");
    }

    #[test]
    fn ported_box_unloads_the_cone_at_tuning() {
        let at_tuning = peak_excursion(PORTED, 35.0);
        let below = peak_excursion(PORTED, 15.0);
        let above = peak_excursion(PORTED, 55.0);
        assert!(at_tuning < below / 3.0, "tuning {at_tuning}, 15 Hz {below}");
        assert!(at_tuning < above / 2.0, "tuning {at_tuning}, 55 Hz {above}");
    }

    #[test]
    fn box_losses_fill_in_the_notch_at_tuning() {
        let tight = peak_excursion(
            WooferParams {
                box_ql: 20.0,
                ..PORTED
            },
            35.0,
        );
        let lossy = peak_excursion(
            WooferParams {
                box_ql: 3.0,
                ..PORTED
            },
            35.0,
        );
        assert!(lossy > tight * 1.5, "QL 3 {lossy}, QL 20 {tight}");
    }

    #[test]
    fn port_tuning_moves_the_notch() {
        let tuned_here = peak_excursion(
            WooferParams {
                port_hz: 50.0,
                ..PORTED
            },
            50.0,
        );
        let tuned_elsewhere = peak_excursion(
            WooferParams {
                port_hz: 25.0,
                ..PORTED
            },
            50.0,
        );
        assert!(
            tuned_here < tuned_elsewhere / 3.0,
            "tuned to 50 Hz {tuned_here}, tuned to 25 Hz {tuned_elsewhere}"
        );
    }

    #[test]
    fn subsonic_highpass_limits_excursion_below_tuning() {
        let open = peak_excursion(PORTED, 8.0);
        let protected = peak_excursion(
            WooferParams {
                highpass: true,
                ..PORTED
            },
            8.0,
        );
        assert!(
            protected < open / 10.0,
            "with highpass {protected}, without {open}"
        );
    }

    #[test]
    fn treble_barely_moves_the_cone() {
        let peak = peak_excursion(
            WooferParams {
                lowpass_hz: 90.0,
                ..BASE
            },
            1_000.0,
        );
        assert!(peak < 1e-3, "1 kHz peak excursion {peak}");
    }

    #[test]
    fn lowpass_cutoff_limits_excursion_above_it() {
        let stiff = WooferParams {
            resonance_hz: 150.0,
            ..BASE
        };
        let wide = peak_excursion(stiff, 120.0);
        let narrow = peak_excursion(
            WooferParams {
                lowpass_hz: 60.0,
                ..stiff
            },
            120.0,
        );
        assert!(
            narrow < wide / 2.0,
            "60 Hz cutoff {narrow}, 200 Hz cutoff {wide}"
        );
    }

    #[test]
    fn excursion_falls_off_above_resonance() {
        let stiff = peak_excursion(
            WooferParams {
                resonance_hz: 100.0,
                ..BASE
            },
            60.0,
        );
        let loose = peak_excursion(
            WooferParams {
                resonance_hz: 20.0,
                ..BASE
            },
            60.0,
        );
        assert!(
            loose < stiff / 2.0,
            "20 Hz resonance {loose}, 100 Hz resonance {stiff}"
        );
    }

    #[test]
    fn treble_passes_through_at_unity() {
        let output = peak_output(PORTED, 2_000.0);
        assert!((output - 1.0).abs() < 0.02, "2 kHz output {output}");
    }

    #[test]
    fn treble_keeps_stereo_separation() {
        let peaks = peaks(PORTED, 2_000.0, [1.0, 0.0]);
        assert!(peaks.right < 0.01, "right channel {}", peaks.right);
    }

    #[test]
    fn simulated_subwoofer_plays_bass_in_its_passband() {
        for sealed_or_ported in [BASE, PORTED] {
            let output = peak_output(sealed_or_ported, 80.0);
            assert!((0.7..1.4).contains(&output), "80 Hz output {output}");
        }
    }

    #[test]
    fn mix_gains_scale_only_their_own_band() {
        let mixed = WooferParams {
            bass_gain_db: 6.0,
            treble_gain_db: -6.0,
            ..PORTED
        };
        let bass_ratio = peak_output(mixed, 40.0) / peak_output(PORTED, 40.0);
        let treble_ratio = peak_output(mixed, 2_000.0) / peak_output(PORTED, 2_000.0);
        assert!((bass_ratio - 2.0).abs() < 0.05, "40 Hz gain {bass_ratio}");
        assert!(
            (treble_ratio - 0.5).abs() < 0.01,
            "2 kHz gain {treble_ratio}"
        );
    }

    #[test]
    fn disabled_treble_leaves_only_the_subwoofer() {
        let bass_only = WooferParams {
            treble: false,
            ..PORTED
        };
        let treble = peak_output(bass_only, 2_000.0);
        let bass_ratio = peak_output(bass_only, 40.0) / peak_output(PORTED, 40.0);
        assert!(treble < 1e-3, "2 kHz output {treble}");
        assert!((bass_ratio - 1.0).abs() < 0.05, "40 Hz gain {bass_ratio}");
    }

    #[test]
    fn ported_box_rolls_off_bass_below_tuning() {
        let passband = peak_output(PORTED, 80.0);
        let below_tuning = peak_output(PORTED, 12.0);
        assert!(
            below_tuning < passband / 5.0,
            "12 Hz {below_tuning}, 80 Hz {passband}"
        );
    }

    #[test]
    fn returning_to_fir_does_not_replay_old_audio() {
        let fir = WooferParams {
            crossover: CrossoverMode::Fir,
            ..PORTED
        };
        let mut model = model(fir);
        let second = SAMPLE_RATE as usize;
        let loud: Vec<[f32; 2]> = (0..second)
            .map(|n| [(std::f32::consts::TAU * 60.0 * n as f32 / SAMPLE_RATE).sin(); 2])
            .collect();
        model.process_block(&loud, |_| {});

        // Switch to IIR, then back, with silence in between and after.
        let silence = vec![[0.0; 2]; second];
        model.configure(WooferParams {
            crossover: CrossoverMode::Iir,
            ..fir
        });
        model.process_block(&silence, |_| {});
        model.configure(fir);
        let mut loudest = 0.0f32;
        model.process_block(&silence, |frame| {
            loudest = loudest
                .max(frame.output[0].abs())
                .max(frame.excursion.abs());
        });
        assert!(loudest < 1e-3, "replayed {loudest}");
    }

    #[test]
    fn switching_crossover_modes_does_not_click() {
        let fir = WooferParams {
            crossover: CrossoverMode::Fir,
            ..BASE
        };
        let iir = WooferParams {
            crossover: CrossoverMode::Iir,
            ..fir
        };
        let mut model = model(fir);
        model.configure(iir);
        let block = 512;
        let sine = |n: usize| (std::f32::consts::TAU * 1_100.0 * n as f32 / SAMPLE_RATE).sin();
        let mut previous: Option<f32> = None;
        let mut largest_step = 0.0f32;
        for (index, start) in (0..SAMPLE_RATE as usize * 3).step_by(block).enumerate() {
            // Toggle every 0.5 s, after a first second to settle.
            if start >= SAMPLE_RATE as usize && index % 47 == 0 {
                let params = if model.params.unwrap().crossover == CrossoverMode::Fir {
                    iir
                } else {
                    fir
                };
                model.configure(params);
            }
            let input: Vec<[f32; 2]> = (start..start + block).map(|n| [sine(n); 2]).collect();
            model.process_block(&input, |frame| {
                if start >= SAMPLE_RATE as usize
                    && let Some(previous) = previous
                {
                    largest_step = largest_step.max((frame.output[0] - previous).abs());
                }
                previous = Some(frame.output[0]);
            });
        }
        // A 1.1 kHz full scale sine moves by up to 0.15 per sample, a jump
        // between the delayed and undelayed signal by up to 2.
        assert!(largest_step < 0.2, "largest step {largest_step}");
    }

    #[test]
    fn fir_crossover_keeps_the_levels_of_the_iir_crossover() {
        let fir = WooferParams {
            crossover: CrossoverMode::Fir,
            ..PORTED
        };
        let bass_only = WooferParams {
            treble: false,
            ..fir
        };
        for frequency in [30.0, 60.0, 90.0, 150.0] {
            let bass = model(bass_only).frequency_response(frequency).0.norm() as f32;
            let fir_bass = peaks(bass_only, frequency, [1.0, 1.0]).left;
            assert!(
                (fir_bass - bass).abs() < 0.02 * bass.max(0.05),
                "{frequency} Hz FIR bass {fir_bass} vs {bass}"
            );
        }
        let treble = peak_output(fir, 2_000.0);
        assert!((treble - 1.0).abs() < 0.01, "2 kHz FIR output {treble}");
    }

    #[test]
    fn frequency_response_matches_the_simulation() {
        for params in [
            BASE,
            WooferParams {
                highpass: true,
                bass_gain_db: 6.0,
                amp_ohm: 0.5,
                voice_coil_mh: 1.15,
                ..PORTED
            },
        ] {
            let params = WooferParams {
                treble: false,
                ..params
            };
            let model = model(params);
            for frequency in [12.0, 25.0, 40.0, 80.0, 150.0] {
                let (pressure, excursion) = model.frequency_response(frequency);
                let (pressure, excursion) = (pressure.norm() as f32, excursion.norm() as f32);
                let simulated = peaks(params, frequency, [1.0, 1.0]);
                assert!(
                    (pressure - simulated.left).abs() < 0.01 * pressure.max(0.1),
                    "{frequency} Hz pressure {pressure} vs {}",
                    simulated.left
                );
                assert!(
                    (excursion - simulated.excursion).abs() < 0.01 * excursion.max(0.1),
                    "{frequency} Hz excursion {excursion} vs {}",
                    simulated.excursion
                );
            }
        }
    }

    #[test]
    fn amplifier_impedance_weakens_the_drive_and_the_damping() {
        let amp = 1.0;
        let loose = WooferParams {
            amp_ohm: amp,
            ..BASE
        };
        let expected = settled(BASE) * driver::drive_gain(amp) as f32;
        let actual = settled(loose);
        assert!(
            (actual - expected).abs() < 1e-3,
            "settled at {actual}, expected {expected}"
        );

        // Less electrical damping makes the sealed box ring more: the response
        // just below the system resonance rises relative to the passband.
        let relative = |params| {
            let response = model(params);
            (response.frequency_response(40.0).0 / response.frequency_response(150.0).0).norm()
        };
        assert!(relative(loose) > relative(BASE) * 1.05);
    }

    #[test]
    fn voice_coil_inductance_rolls_off_the_top() {
        let wide = WooferParams {
            lowpass_hz: 20_000.0,
            ..BASE
        };
        let with_coil = WooferParams {
            voice_coil_mh: 1.15,
            ..wide
        };
        let frequency = 500.0;
        let ratio = model(with_coil).frequency_response(frequency).0.norm()
            / model(wide).frequency_response(frequency).0.norm();
        let corner = driver::RE_OHM / (TAU * 1.15e-3);
        let expected = 1.0 / (1.0 + (f64::from(frequency) / corner).powi(2)).sqrt();
        assert!((ratio - expected).abs() < 0.01, "{ratio} vs {expected}");
    }

    #[test]
    fn frequencies_above_nyquist_stay_stable() {
        let extreme = WooferParams {
            lowpass_hz: 40_000.0,
            resonance_hz: 40_000.0,
            port_hz: 40_000.0,
            highpass_hz: 40_000.0,
            ..BASE
        };
        for ported in [false, true] {
            let peaks = peaks(WooferParams { ported, ..extreme }, 100.0, [1.0, 1.0]);
            for value in [peaks.excursion, peaks.left, peaks.right] {
                assert!(value.is_finite() && value < 2.0, "peak {value}");
            }
        }
    }
}
