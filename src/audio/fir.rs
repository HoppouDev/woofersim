//! Linear phase crossover built from one FIR lowpass per channel.
//!
//! The lowpass has the magnitude response of the 4th order Linkwitz-Riley
//! lowpass the IIR crossover uses, but no phase shift, only a constant delay.
//! The highpass is the input delayed by the same amount minus the lowpass, so
//! the two halves always add up to the delayed input exactly. The price is
//! latency: half the filter length, which is 6 periods of the crossover
//! frequency, 67 ms at 90 Hz.

use fft_convolver::FFTConvolver;
use realfft::RealFftPlanner;

/// Filter half length in periods of the crossover frequency. Together with
/// [`KAISER_BETA`] this keeps the magnitude within 0.1 dB of the
/// Linkwitz-Riley target down to -40 dB.
const HALF_LENGTH_PERIODS: f64 = 6.0;
/// Kaiser window shape. Higher values suppress the stopband further but need
/// a longer filter for the same accuracy near the crossover.
const KAISER_BETA: f64 = 6.0;
const MAX_HALF_LENGTH: usize = 1 << 15;
/// Partition size of the FFT convolution.
const CONVOLUTION_BLOCK: usize = 512;
/// Frames processed per call to the convolvers.
pub const MAX_FRAMES: usize = 4096;

/// Delay of the crossover in samples, half the filter length.
pub fn latency(cutoff_hz: f32, sample_rate: f32) -> usize {
    let periods = HALF_LENGTH_PERIODS * f64::from(sample_rate) / f64::from(cutoff_hz);
    (periods.ceil() as usize).clamp(64, MAX_HALF_LENGTH)
}

/// Coefficients of the linear phase lowpass, `2 * latency + 1` taps long and
/// symmetric around the center tap.
pub fn lowpass_taps(cutoff_hz: f32, sample_rate: f32) -> Vec<f32> {
    let cutoff = f64::from(cutoff_hz);
    let rate = f64::from(sample_rate);
    let half = latency(cutoff_hz, sample_rate);
    let taps = 2 * half + 1;

    // Sample the target magnitude densely and transform it back. A real and
    // even spectrum gives a real, even impulse response centered at index 0.
    let size = (4 * taps).next_power_of_two();
    let inverse = RealFftPlanner::<f64>::new().plan_fft_inverse(size);
    let mut spectrum = inverse.make_input_vec();
    for (bin, value) in spectrum.iter_mut().enumerate() {
        let frequency = bin as f64 * rate / size as f64;
        value.re = 1.0 / (1.0 + (frequency / cutoff).powi(4));
        value.im = 0.0;
    }
    let mut impulse = inverse.make_output_vec();
    inverse
        .process(&mut spectrum, &mut impulse)
        .expect("buffers come from the plan");

    // Kaiser window over the kept part, then unity gain at DC.
    let window_scale = bessel_i0(KAISER_BETA);
    let mut coefficients: Vec<f64> = (0..=2 * half)
        .map(|n| {
            let offset = n.abs_diff(half);
            let position = offset as f64 / half as f64;
            let window = bessel_i0(KAISER_BETA * (1.0 - position * position).sqrt()) / window_scale;
            impulse[offset] * window
        })
        .collect();
    let dc: f64 = coefficients.iter().sum();
    coefficients.iter_mut().for_each(|c| *c /= dc);
    coefficients.into_iter().map(|c| c as f32).collect()
}

/// Modified Bessel function of the first kind, order 0, by its power series.
fn bessel_i0(x: f64) -> f64 {
    let quarter_square = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..50 {
        term *= quarter_square / (k * k) as f64;
        sum += term;
        if term < sum * 1e-16 {
            break;
        }
    }
    sum
}

/// A linear phase crossover for stereo audio. Building one allocates and
/// runs FFTs, so do it off the audio thread. Processing does neither.
pub struct FirCrossover {
    lowpass: [FFTConvolver<f32>; 2],
    /// Input history for the highpass, one ring buffer per channel.
    delay: [Vec<f32>; 2],
    delay_position: usize,
    channel_in: [Vec<f32>; 2],
    channel_low: [Vec<f32>; 2],
}

impl FirCrossover {
    pub fn new(cutoff_hz: f32, sample_rate: f32) -> Self {
        let taps = lowpass_taps(cutoff_hz, sample_rate);
        let latency = taps.len() / 2;
        let convolver = || {
            let mut convolver = FFTConvolver::default();
            convolver
                .init(CONVOLUTION_BLOCK, &taps)
                .expect("block size is not zero");
            convolver
        };
        Self {
            lowpass: [convolver(), convolver()],
            delay: [vec![0.0; latency], vec![0.0; latency]],
            delay_position: 0,
            channel_in: [vec![0.0; MAX_FRAMES], vec![0.0; MAX_FRAMES]],
            channel_low: [vec![0.0; MAX_FRAMES], vec![0.0; MAX_FRAMES]],
        }
    }

    /// Frames of input it takes until the output no longer depends on the
    /// silence the filter started from.
    fn history_len(&self) -> usize {
        2 * self.delay[0].len() + 1
    }

    /// Splits up to [`MAX_FRAMES`] stereo frames into the delayed lowpassed
    /// and highpassed signals.
    pub fn process(&mut self, input: &[[f32; 2]], low: &mut [[f32; 2]], high: &mut [[f32; 2]]) {
        let frames = input.len();
        debug_assert!(frames <= MAX_FRAMES);
        for channel in 0..2 {
            let source = &mut self.channel_in[channel][..frames];
            for (sample, frame) in source.iter_mut().zip(input) {
                *sample = frame[channel];
            }
            let low_out = &mut self.channel_low[channel][..frames];
            if self.lowpass[channel].process(source, low_out).is_err() {
                low_out.fill(0.0);
            }
        }

        let delay_len = self.delay[0].len();
        for i in 0..frames {
            for channel in 0..2 {
                let delayed = std::mem::replace(
                    &mut self.delay[channel][self.delay_position],
                    input[i][channel],
                );
                let low_sample = self.channel_low[channel][i];
                low[i][channel] = low_sample;
                high[i][channel] = delayed - low_sample;
            }
            self.delay_position = (self.delay_position + 1) % delay_len;
        }
    }
}

/// Frames over which a newly built crossover fades in.
const FADE_FRAMES: usize = 2048;

/// Holds the crossover in use and swaps in rebuilt ones without clicks: a
/// new crossover first runs alongside the old one until its history is full,
/// then the output fades over to it. A crossover arriving while a fade is
/// under way waits in [`pending`](Self::pending) instead of cutting the fade
/// short, and only the newest one waits.
pub struct FirSlot {
    current: Option<Box<FirCrossover>>,
    /// The replacement and how many frames it has processed so far.
    incoming: Option<(Box<FirCrossover>, usize)>,
    /// The next replacement, started once the current fade finishes.
    pending: Option<Box<FirCrossover>>,
    /// A crossover no longer in use, waiting to be freed off the audio
    /// thread.
    retired: Option<Box<FirCrossover>>,
    incoming_low: Vec<[f32; 2]>,
    incoming_high: Vec<[f32; 2]>,
}

impl Default for FirSlot {
    fn default() -> Self {
        Self {
            current: None,
            incoming: None,
            pending: None,
            retired: None,
            incoming_low: vec![[0.0; 2]; MAX_FRAMES],
            incoming_high: vec![[0.0; 2]; MAX_FRAMES],
        }
    }
}

impl FirSlot {
    /// Queues `crossover` to replace the current one. Returns a queued
    /// replacement it superseded, to be freed off the audio thread.
    pub fn install(&mut self, crossover: Box<FirCrossover>) -> Option<Box<FirCrossover>> {
        if self.current.is_none() {
            self.current = Some(crossover);
            return None;
        }
        match &self.incoming {
            // Still warming up, nothing of it is audible yet.
            Some((incoming, seen)) if *seen < incoming.history_len() => {
                self.incoming.replace((crossover, 0)).map(|(old, _)| old)
            }
            Some(_) => self.pending.replace(crossover),
            None => {
                self.incoming = Some((crossover, 0));
                None
            }
        }
    }

    /// A crossover that went out of use, to be freed off the audio thread.
    pub fn take_retired(&mut self) -> Option<Box<FirCrossover>> {
        self.retired.take()
    }

    /// Like [`FirCrossover::process`]. Returns false, leaving the outputs
    /// untouched, while no crossover has been installed yet.
    pub fn process(
        &mut self,
        input: &[[f32; 2]],
        low: &mut [[f32; 2]],
        high: &mut [[f32; 2]],
    ) -> bool {
        let Some(current) = &mut self.current else {
            return false;
        };
        current.process(input, low, high);

        let Some((incoming, seen)) = &mut self.incoming else {
            return true;
        };
        let frames = input.len();
        let (incoming_low, incoming_high) = (
            &mut self.incoming_low[..frames],
            &mut self.incoming_high[..frames],
        );
        incoming.process(input, incoming_low, incoming_high);
        let warm = incoming.history_len();
        for i in 0..frames {
            let progress = (*seen + i).saturating_sub(warm);
            let t = (progress as f32 / FADE_FRAMES as f32).min(1.0);
            if *seen + i >= warm {
                for channel in 0..2 {
                    low[i][channel] += (incoming_low[i][channel] - low[i][channel]) * t;
                    high[i][channel] += (incoming_high[i][channel] - high[i][channel]) * t;
                }
            }
        }
        *seen += frames;
        // Waits while the previously retired crossover has not been taken,
        // so nothing is ever dropped here on the audio thread. The output
        // stays fully faded to the incoming one meanwhile.
        if *seen >= warm + FADE_FRAMES
            && self.retired.is_none()
            && let Some((incoming, _)) = self.incoming.take()
        {
            self.retired = self.current.replace(incoming);
            self.incoming = self.pending.take().map(|next| (next, 0));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    /// Gain of a symmetric filter at `frequency`.
    fn gain(taps: &[f32], frequency: f32) -> f64 {
        let half = taps.len() / 2;
        let omega = std::f64::consts::TAU * f64::from(frequency) / f64::from(RATE);
        f64::from(taps[half])
            + (1..=half)
                .map(|k| 2.0 * f64::from(taps[half + k]) * (omega * k as f64).cos())
                .sum::<f64>()
    }

    #[test]
    fn lowpass_matches_the_linkwitz_riley_magnitude() {
        for cutoff in [30.0, 90.0, 250.0] {
            let taps = lowpass_taps(cutoff, RATE);
            for frequency in [5.0, 10.0, 20.0, 40.0, 60.0, 90.0, 150.0, 250.0, 400.0] {
                let target = 1.0 / (1.0 + (f64::from(frequency) / f64::from(cutoff)).powi(4));
                if target < 0.01 {
                    continue;
                }
                let actual = gain(&taps, frequency);
                let error_db = 20.0 * (actual / target).log10();
                assert!(
                    error_db.abs() < 0.1,
                    "{cutoff} Hz cutoff, {frequency} Hz: {error_db:.3} dB off"
                );
            }
        }
    }

    #[test]
    fn halves_sum_to_the_delayed_input() {
        let mut crossover = FirCrossover::new(90.0, RATE);
        let latency = crossover.delay[0].len();
        let mut state = 1u32;
        let input: Vec<[f32; 2]> = (0..3 * latency)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let sample = (state >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                [sample, -sample]
            })
            .collect();
        let mut low = vec![[0.0; 2]; input.len()];
        let mut high = vec![[0.0; 2]; input.len()];
        for ((input, low), high) in input
            .chunks(1000)
            .zip(low.chunks_mut(1000))
            .zip(high.chunks_mut(1000))
        {
            crossover.process(input, low, high);
        }
        for i in latency..input.len() {
            for channel in 0..2 {
                let sum = low[i][channel] + high[i][channel];
                let expected = input[i - latency][channel];
                assert!(
                    (sum - expected).abs() < 1e-5,
                    "frame {i}: {sum} vs {expected}"
                );
            }
        }
    }

    #[test]
    fn replacing_crossovers_mid_fade_stays_continuous() {
        const BLOCK: usize = 512;
        let mut slot = FirSlot::default();
        slot.install(Box::new(FirCrossover::new(90.0, RATE)));
        let sine = |n: usize| (std::f32::consts::TAU * 30.0 * n as f32 / RATE).sin();
        let mut low = [[0.0; 2]; BLOCK];
        let mut high = [[0.0; 2]; BLOCK];
        let mut previous = None;
        let mut largest_step = 0.0f32;
        let mut n = 0;
        // New cutoffs keep arriving about every 100 ms, like while a slider
        // is dragged. For these cutoffs that is after the newcomer has warmed
        // up but before its fade has finished.
        for (block, cutoff) in (0..400).zip([180.0, 160.0, 200.0, 170.0].into_iter().cycle()) {
            if block % 9 == 0 && block > 0 {
                drop(slot.install(Box::new(FirCrossover::new(cutoff, RATE))));
            }
            let input: Vec<[f32; 2]> = (n..n + BLOCK).map(|i| [sine(i); 2]).collect();
            n += BLOCK;
            slot.process(&input, &mut low, &mut high);
            drop(slot.take_retired());
            // Skip the first second, while the very first crossover fills up.
            if n < RATE as usize {
                previous = Some(low[BLOCK - 1][0]);
                continue;
            }
            for frame in &low {
                if let Some(previous) = previous {
                    largest_step = largest_step.max((frame[0] - previous).abs());
                }
                previous = Some(frame[0]);
            }
        }
        // A 30 Hz full scale sine moves by at most 0.004 per sample.
        assert!(largest_step < 0.01, "largest step {largest_step}");
    }
}
