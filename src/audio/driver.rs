//! Electrical and mechanical parameters of the simulated driver, a Dayton
//! Audio UMII18-22 with its two voice coils in series, and its electrical
//! impedance in the enclosure.
//!
//! The excursion and sound pressure use Small's normalized model, driven by
//! a voltage source behind the amplifier's output impedance. That impedance
//! adds to the voice coil resistance, which weakens the motor's electrical
//! damping (raising the total Q) and lowers the level. The voice coil
//! inductance rolls the drive off above (Re + Rg) / (2π Le), which for this
//! driver is around 580 Hz. It is applied as a first order lowpass on the
//! drive, which matches the full model closely as long as that corner is far
//! above the resonance, as it is for any woofer.

use std::f64::consts::TAU;

use num_complex::Complex64;

use super::dsp::WooferParams;

/// DC resistance of the voice coil (Re), Ω.
pub const RE_OHM: f64 = 4.2;
/// Electrical Q at the free air resonance (Qes).
const QES: f64 = 0.67;
/// Mechanical Q at the free air resonance (Qms).
const QMS: f64 = 2.53;
/// Volume of air with the same compliance as the suspension (Vas), ft³.
pub const VAS_FT3: f64 = 8.77;
/// Voice coil inductance (Le) from the data sheet, mH.
pub const VOICE_COIL_MH: f32 = 1.15;

/// Total Q at the free air resonance (Qts) when driven through an amplifier
/// with this output impedance. The data sheet's 0.53 is for 0 Ω.
pub fn total_q(amp_ohm: f32) -> f64 {
    let qes = QES * (RE_OHM + f64::from(amp_ohm)) / RE_OHM;
    1.0 / (1.0 / qes + 1.0 / QMS)
}

/// Level of the drive relative to an amplifier with 0 Ω output impedance.
pub fn drive_gain(amp_ohm: f32) -> f64 {
    RE_OHM / (RE_OHM + f64::from(amp_ohm))
}

/// Corner of the voice coil inductance lowpass in Hz, or None without
/// inductance.
pub fn inductance_corner_hz(amp_ohm: f32, voice_coil_mh: f32) -> Option<f64> {
    (voice_coil_mh > 0.0)
        .then(|| (RE_OHM + f64::from(amp_ohm)) / (TAU * f64::from(voice_coil_mh) * 1e-3))
}

/// Electrical impedance at the driver's terminals, in Ω.
///
/// Built from the mechanical circuit with the moving mass taken as 1, which
/// cancels out: suspension compliance and resistance from Fs and Qms, motor
/// strength from Qes, and the box as a compliance, in parallel with the port
/// mass and the leakage resistance when ported.
pub fn impedance(params: &WooferParams, frequency: f32) -> Complex64 {
    let s = Complex64::new(0.0, TAU * f64::from(frequency));
    let ws = TAU * f64::from(params.resonance_hz);
    let suspension_compliance = 1.0 / (ws * ws);
    let suspension_resistance = ws / QMS;
    let bl_squared = RE_OHM * ws / QES;
    let box_compliance = suspension_compliance * f64::from(params.box_volume_ft3) / VAS_FT3;

    let enclosure = if params.ported {
        let wb = TAU * f64::from(params.port_hz);
        let port_mass = 1.0 / (wb * wb * box_compliance);
        let leakage = f64::from(params.box_ql) / (wb * box_compliance);
        1.0 / (s * box_compliance + 1.0 / (s * port_mass) + 1.0 / leakage)
    } else {
        1.0 / (s * box_compliance)
    };
    let mechanical = suspension_resistance + s + 1.0 / (s * suspension_compliance) + enclosure;
    RE_OHM + s * f64::from(params.voice_coil_mh) * 1e-3 + bl_squared / mechanical
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::CrossoverMode;

    const PARAMS: WooferParams = WooferParams {
        sample_rate: 48_000.0,
        lowpass_hz: 90.0,
        resonance_hz: 22.0,
        ported: false,
        port_hz: 20.0,
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

    /// Frequency and magnitude of the highest impedance between 5 and 200 Hz.
    fn peak(params: &WooferParams, range: std::ops::Range<f32>) -> (f32, f64) {
        (0..2000)
            .map(|i| range.start + (range.end - range.start) * i as f32 / 2000.0)
            .map(|f| (f, impedance(params, f).norm()))
            .fold(
                (0.0, 0.0),
                |best, point| if point.1 > best.1 { point } else { best },
            )
    }

    #[test]
    fn data_sheet_total_q_is_reproduced() {
        assert!((total_q(0.0) - 0.53).abs() < 0.005, "{}", total_q(0.0));
    }

    #[test]
    fn sealed_box_peaks_at_its_resonance() {
        let (frequency, magnitude) = peak(&PARAMS, 5.0..200.0);
        let expected_frequency = 22.0 * (1.0 + VAS_FT3 / 6.0).sqrt();
        let expected_magnitude = RE_OHM * (1.0 + QMS / QES);
        assert!(
            (f64::from(frequency) - expected_frequency).abs() < 0.5,
            "peak at {frequency} Hz, expected {expected_frequency}"
        );
        assert!(
            (magnitude - expected_magnitude).abs() < 0.05,
            "peak of {magnitude} Ω, expected {expected_magnitude}"
        );
        assert!((impedance(&PARAMS, 1.0).norm() - RE_OHM).abs() < 0.1);
    }

    #[test]
    fn ported_box_has_two_peaks_around_the_tuning() {
        let ported = WooferParams {
            ported: true,
            port_hz: 30.0,
            ..PARAMS
        };
        let (low, _) = peak(&ported, 5.0..30.0);
        let (high, _) = peak(&ported, 30.0..200.0);
        let at_tuning = impedance(&ported, 30.0).norm();
        assert!(low < 30.0 && high > 30.0, "peaks at {low} and {high} Hz");
        assert!(
            at_tuning < impedance(&ported, low).norm() / 2.0
                && at_tuning < impedance(&ported, high).norm() / 2.0,
            "{at_tuning} Ω at the tuning"
        );
    }

    #[test]
    fn inductance_raises_the_impedance_at_high_frequencies() {
        let with_coil = WooferParams {
            voice_coil_mh: 1.15,
            ..PARAMS
        };
        let expected = (RE_OHM * RE_OHM + (TAU * 3000.0 * 1.15e-3_f64).powi(2)).sqrt();
        let actual = impedance(&with_coil, 3000.0).norm();
        assert!(
            (actual - expected).abs() < 0.1,
            "{actual} Ω, expected {expected}"
        );
    }
}
