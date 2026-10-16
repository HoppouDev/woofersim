//! Recent cone excursion with timestamps, so the renderer can look up where
//! the cone was at the exact moment a frame is drawn.
//!
//! Audio arrives in buffers (typically 1024 samples, about 21 ms) whose
//! timing has nothing to do with the frame rate. Showing the newest sample of
//! the newest buffer therefore samples the motion at irregular points in
//! time, and a steady tone turns into a drifting sawtooth on screen. Instead,
//! every sample is stored with its index, the audio thread records when each
//! buffer arrived, and the renderer reads the sample that belongs to the
//! current time minus a fixed delay.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

/// Ring buffer length in samples, 1.4 s at 48 kHz. Must be a power of two.
const CAPACITY: usize = 1 << 16;

/// How quickly the sample clock follows the arrival time of new buffers.
/// Small values average out the scheduling jitter of the audio thread.
const CLOCK_SMOOTHING: f64 = 0.02;

/// Arrival time error, in seconds, beyond which the clock jumps to the new
/// value instead of drifting there. Happens when playback resumes after a
/// pause or the sample rate changes.
const RESYNC_THRESHOLD: f64 = 0.02;

/// Extra delay on top of one buffer, in seconds, so the sample being read has
/// arrived even when the audio thread runs a little late.
const SAFETY_MARGIN: f64 = 0.01;

pub struct ExcursionHistory {
    epoch: Instant,
    samples: Box<[AtomicU32]>,
    /// Total number of samples written. Published last, with release
    /// ordering, so everything below it is visible to the reader.
    written: AtomicU64,
    sample_rate: AtomicU32,
    buffer_len: AtomicU32,
    /// Time of sample 0 in seconds since `epoch`, as f64 bits.
    clock_offset: AtomicU64,
}

impl Default for ExcursionHistory {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            samples: (0..CAPACITY).map(|_| AtomicU32::new(0)).collect(),
            written: AtomicU64::new(0),
            sample_rate: AtomicU32::new(0),
            buffer_len: AtomicU32::new(0),
            clock_offset: AtomicU64::new(0),
        }
    }
}

impl ExcursionHistory {
    /// Seconds since the history was created.
    pub fn seconds(&self, instant: Instant) -> f64 {
        instant.saturating_duration_since(self.epoch).as_secs_f64()
    }

    /// Excursion at `time` (see [`seconds`](Self::seconds)) minus one buffer
    /// and a safety margin, interpolated between samples. Holds the newest
    /// sample while no audio is arriving.
    pub fn sample_at(&self, time: f64) -> f32 {
        let written = self.written.load(Ordering::Acquire);
        let rate = f64::from(self.sample_rate.load(Ordering::Relaxed));
        if written == 0 || rate == 0.0 {
            return 0.0;
        }
        let buffer_len = f64::from(self.buffer_len.load(Ordering::Relaxed));
        let offset = f64::from_bits(self.clock_offset.load(Ordering::Relaxed));

        let delay = buffer_len / rate + SAFETY_MARGIN;
        let newest = (written - 1) as f64;
        // Leave a buffer's worth of room so the writer never overwrites the
        // slots being read.
        let oldest = written.saturating_sub((CAPACITY / 2) as u64) as f64;
        let position = ((time - delay - offset) * rate).clamp(oldest, newest);

        let index = position.floor();
        let fraction = (position - index) as f32;
        let before = self.load(index as u64);
        let after = self.load((index as u64 + 1).min(written - 1));
        before + (after - before) * fraction
    }

    fn load(&self, index: u64) -> f32 {
        f32::from_bits(self.samples[index as usize % CAPACITY].load(Ordering::Relaxed))
    }
}

/// Audio thread side of an [`ExcursionHistory`].
#[derive(Default)]
pub struct HistoryWriter {
    written: u64,
    sample_rate: u32,
    clock_offset: Option<f64>,
}

impl HistoryWriter {
    pub fn push(&mut self, history: &ExcursionHistory, excursion: f32) {
        history.samples[self.written as usize % CAPACITY]
            .store(excursion.to_bits(), Ordering::Relaxed);
        self.written += 1;
    }

    /// Publishes the samples pushed since the last call. `now` is the arrival
    /// time of the buffer they came from, in [`ExcursionHistory::seconds`].
    pub fn finish_buffer(
        &mut self,
        history: &ExcursionHistory,
        now: f64,
        sample_rate: u32,
        buffer_len: u32,
    ) {
        if sample_rate == 0 {
            return;
        }
        if sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            self.clock_offset = None;
        }
        // The newest sample of the buffer belongs to roughly `now`, so sample
        // 0 belongs to `now` minus the duration of everything written.
        let measured = now - self.written as f64 / f64::from(sample_rate);
        let offset = match self.clock_offset {
            Some(offset) if (measured - offset).abs() < RESYNC_THRESHOLD => {
                offset + (measured - offset) * CLOCK_SMOOTHING
            }
            _ => measured,
        };
        self.clock_offset = Some(offset);

        history.sample_rate.store(sample_rate, Ordering::Relaxed);
        history.buffer_len.store(buffer_len, Ordering::Relaxed);
        history
            .clock_offset
            .store(offset.to_bits(), Ordering::Relaxed);
        history.written.store(self.written, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;

    const RATE: u32 = 48_000;
    const BUFFER: u32 = 1024;

    /// Feeds `seconds` of a sine in 1024 sample buffers. Each buffer arrives
    /// late by a pseudo random amount of up to `jitter` seconds, like a real
    /// audio thread.
    fn feed_sine(frequency: f64, seconds: f64, jitter: f64) -> ExcursionHistory {
        let history = ExcursionHistory::default();
        let mut writer = HistoryWriter::default();
        let buffers = (seconds * f64::from(RATE) / f64::from(BUFFER)) as u64;
        let mut lcg = 12345_u64;
        for buffer in 0..buffers {
            for n in 0..u64::from(BUFFER) {
                let index = buffer * u64::from(BUFFER) + n;
                let t = index as f64 / f64::from(RATE);
                writer.push(&history, (TAU * frequency * t).sin() as f32);
            }
            lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
            let late = (lcg >> 11) as f64 / (1u64 << 53) as f64 * jitter;
            let arrival = (buffer + 1) as f64 * f64::from(BUFFER) / f64::from(RATE) + late;
            writer.finish_buffer(&history, arrival, RATE, BUFFER);
        }
        history
    }

    #[test]
    fn tone_at_frame_rate_looks_stationary() {
        let history = feed_sine(30.0, 3.0, 0.004);
        // 14 frames, all within the last 0.7 s the history keeps.
        let frames: Vec<f32> = (0..14)
            .map(|frame| history.sample_at(2.5 + f64::from(frame) / 30.0))
            .collect();
        let spread = frames.iter().fold(f32::MIN, |a, &b| a.max(b))
            - frames.iter().fold(f32::MAX, |a, &b| a.min(b));
        assert!(
            spread < 0.15,
            "30 Hz tone at 30 fps moved by {spread}: {frames:?}"
        );
    }

    #[test]
    fn readback_follows_the_signal_with_a_fixed_delay() {
        let history = feed_sine(5.0, 3.0, 0.004);
        let delay = f64::from(BUFFER) / f64::from(RATE) + SAFETY_MARGIN;
        for step in 0..40 {
            let time = 2.4 + f64::from(step) * 0.0137;
            let expected = (TAU * 5.0 * (time - delay)).sin() as f32;
            let actual = history.sample_at(time);
            assert!(
                (actual - expected).abs() < 0.1,
                "at {time}: {actual} vs {expected}"
            );
        }
    }

    #[test]
    fn holds_the_newest_sample_after_audio_stops() {
        let history = feed_sine(5.0, 1.0, 0.0);
        let newest = history.load(history.written.load(Ordering::Relaxed) - 1);
        assert_eq!(history.sample_at(60.0), newest);
    }
}
