//! The parts of the audio path that every platform backend shares: the
//! processor that runs on the capture thread, the FIR crossover builder that
//! runs off it, and the queue that feeds the playback stream.
//!
//! FIR crossovers are built off the audio thread whenever the crossover
//! frequency or sample rate changes, handed to it through a queue, and
//! handed back the same way to be freed, because building and freeing them
//! allocates.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer, RingBuffer};

use super::Shared;
use super::dsp::WooferModel;
use super::fir::FirCrossover;
use super::history::HistoryWriter;

/// Processing rate used for device rates that are a multiple of neither
/// 44.1 nor 48 kHz.
pub const DEFAULT_RATE: u32 = 48_000;
const CD_RATE: u32 = 44_100;

/// How often the FIR crossover builder should be ticked.
pub const FIR_CHECK_INTERVAL: Duration = Duration::from_millis(100);
/// Crossovers in flight between the threads in either direction.
const FIR_QUEUE: usize = 4;
/// Frames decoded from a buffer at a time.
const DECODE_FRAMES: usize = 4096;

/// Frames passed from the processor to the playback stream, 170 ms at
/// 48 kHz. Only a couple of buffers are ever in flight, the rest is headroom.
const OUTPUT_QUEUE_FRAMES: usize = 8192;
/// Most buffers' worth of audio the playback stream lets queue up before it
/// drops the oldest frames. Bounds the latency when the two streams drift.
const MAX_QUEUED_BUFFERS: usize = 2;

/// The four ends of the audio path, to be moved to their threads.
pub struct Engine {
    pub processor: Processor,
    pub fir_builder: FirBuilder,
    pub output: OutputReader,
    pub controls: OutputControls,
}

impl Engine {
    /// `output` decides whether the playback stream starts out active.
    /// `bass_only` leaves the treble out of the playback stream, for when the
    /// original audio reaches the speakers anyway.
    pub fn new(shared: Arc<Shared>, output: bool, bass_only: bool) -> Self {
        let (producer, consumer) = RingBuffer::new(OUTPUT_QUEUE_FRAMES);
        let (new_firs_tx, new_firs) = RingBuffer::new(FIR_QUEUE);
        // Room for every crossover the audio thread can hold besides the one
        // in use: the whole incoming queue, plus incoming, pending and retired.
        let (old_firs, old_firs_rx) = RingBuffer::new(FIR_QUEUE + 3);
        let stream_rate = Arc::new(AtomicU32::new(0));
        let controls = OutputControls {
            enabled: Arc::new(AtomicBool::new(output)),
            flush: Arc::new(AtomicBool::new(false)),
        };
        Self {
            processor: Processor {
                shared: shared.clone(),
                model: WooferModel::default(),
                history: HistoryWriter::default(),
                output: producer,
                output_enabled: controls.enabled.clone(),
                stream_rate: stream_rate.clone(),
                new_firs,
                old_firs,
                frames: vec![[0.0; 2]; DECODE_FRAMES],
                bass_only,
            },
            fir_builder: FirBuilder {
                shared,
                stream_rate,
                new_firs: new_firs_tx,
                old_firs: old_firs_rx,
                built: None,
            },
            output: OutputReader {
                queue: consumer,
                flush: controls.flush.clone(),
            },
            controls,
        }
    }
}

/// Filters captured audio into cone excursion and the playback queue. Lives
/// on the capture thread and never allocates or blocks.
pub struct Processor {
    shared: Arc<Shared>,
    model: WooferModel,
    history: HistoryWriter,
    output: Producer<[f32; 2]>,
    /// Whether the playback stream is on. While it is off nothing is queued
    /// for it, so it never starts with old audio.
    output_enabled: Arc<AtomicBool>,
    /// Sample rate of the processed audio, for building FIR crossovers.
    stream_rate: Arc<AtomicU32>,
    new_firs: Consumer<Box<FirCrossover>>,
    old_firs: Producer<Box<FirCrossover>>,
    frames: Vec<[f32; 2]>,
    bass_only: bool,
}

impl Processor {
    /// Processes one buffer of interleaved little endian `f32` samples with
    /// `channels` channels, of which the first two are used, that started
    /// arriving at `arrival`.
    pub fn process(&mut self, bytes: &[u8], channels: usize, rate: u32, arrival: Instant) {
        if channels == 0 {
            return;
        }
        let history = &self.shared.excursion;
        let arrival = history.seconds(arrival);
        self.stream_rate.store(rate, Ordering::Relaxed);
        let mut params = self.shared.woofer_params(rate as f32);
        if self.bass_only {
            params.treble = false;
        }
        self.model.configure(params);
        // A replaced crossover goes back to be freed, so only take new ones
        // while there is room to return one.
        while self.old_firs.slots() > 1
            && let Ok(crossover) = self.new_firs.pop()
        {
            if let Some(superseded) = self.model.install_fir(crossover) {
                let _ = self.old_firs.push(superseded);
            }
        }

        let frame_bytes = channels * size_of::<f32>();
        let buffer_len = (bytes.len() / frame_bytes) as u32;
        for encoded in bytes.chunks(DECODE_FRAMES * frame_bytes) {
            let frames = &mut self.frames[..encoded.len() / frame_bytes];
            for (frame, encoded) in frames.iter_mut().zip(encoded.chunks_exact(frame_bytes)) {
                let (samples, _) = encoded.as_chunks::<4>();
                let left = f32::from_le_bytes(samples[0]);
                let right = samples
                    .get(1)
                    .map_or(left, |&sample| f32::from_le_bytes(sample));
                *frame = [left, right];
            }
            let (writer, output) = (&mut self.history, &mut self.output);
            let playing = self.output_enabled.load(Ordering::Acquire);
            self.model.process_block(frames, |frame| {
                writer.push(history, frame.excursion);
                if playing {
                    // Only full when the playback stream stalls, the frame is
                    // then not needed.
                    let _ = output.push(frame.output);
                }
            });
        }
        // The slot keeps a retired crossover until it is taken, so it is only
        // taken when it can be handed back.
        if self.old_firs.slots() > 0
            && let Some(retired) = self.model.take_retired_fir()
        {
            let _ = self.old_firs.push(retired);
        }
        self.history
            .finish_buffer(history, arrival, rate, buffer_len);
    }
}

/// Builds FIR crossovers for the processor and frees the ones it is done
/// with. Ticked every [`FIR_CHECK_INTERVAL`] on a thread other than the
/// capture thread.
pub struct FirBuilder {
    shared: Arc<Shared>,
    stream_rate: Arc<AtomicU32>,
    new_firs: Producer<Box<FirCrossover>>,
    old_firs: Consumer<Box<FirCrossover>>,
    built: Option<(f32, u32)>,
}

impl FirBuilder {
    pub fn tick(&mut self) {
        // Dropping them here frees them off the audio thread.
        while self.old_firs.pop().is_ok() {}

        let rate = self.stream_rate.load(Ordering::Relaxed);
        let wanted = (self.shared.lowpass_hz.load(), rate);
        // Built in IIR mode too, because the FIR crossover keeps running
        // there and must have the right cutoff when switched back to.
        // Nothing is built while the queue is full, for example while no
        // audio flows, since the result could not be handed over.
        if rate == 0 || self.built == Some(wanted) || self.new_firs.is_full() {
            return;
        }
        let crossover = Box::new(FirCrossover::new(wanted.0, rate as f32));
        if self.new_firs.push(crossover).is_ok() {
            self.built = Some(wanted);
        }
    }
}

/// The playback stream's end of the queue.
pub struct OutputReader {
    queue: Consumer<[f32; 2]>,
    flush: Arc<AtomicBool>,
}

impl OutputReader {
    /// Fills `bytes` with `frames` frames of interleaved little endian `f32`
    /// samples with `channels` channels. The left and right output go to the
    /// first two channels, any others stay silent.
    pub fn fill(&mut self, bytes: &mut [u8], channels: usize, frames: usize) {
        let keep = if self.flush.swap(false, Ordering::AcqRel) {
            0
        } else {
            frames * MAX_QUEUED_BUFFERS
        };
        let stale = self.queue.slots().saturating_sub(keep);
        if let Ok(chunk) = self.queue.read_chunk(stale) {
            chunk.commit_all();
        }
        let frame_bytes = channels * size_of::<f32>();
        for out in bytes[..frames * frame_bytes].chunks_exact_mut(frame_bytes) {
            // Silence when nothing new was captured, for example when
            // nothing plays.
            let [left, right] = self.queue.pop().unwrap_or_default();
            let (samples, _) = out.as_chunks_mut::<4>();
            for (channel, sample) in samples.iter_mut().enumerate() {
                let value = match (channel, channels) {
                    (0, 1) => (left + right) / 2.0,
                    (0, _) => left,
                    (1, _) => right,
                    _ => 0.0,
                };
                *sample = value.to_le_bytes();
            }
        }
    }
}

/// Switches the playback queue on and off from the control thread.
#[derive(Clone)]
pub struct OutputControls {
    enabled: Arc<AtomicBool>,
    flush: Arc<AtomicBool>,
}

impl OutputControls {
    pub fn set_enabled(&self, enabled: bool) {
        if enabled {
            // Drops whatever is still queued from before it was switched off.
            self.flush.store(true, Ordering::Release);
        }
        self.enabled.store(enabled, Ordering::Release);
    }

    /// Only the Windows backend polls this, PipeWire gets told directly.
    #[cfg(windows)]
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
}

/// The processing rate for a device or graph rate: 44.1 kHz when it is a
/// multiple of it, 48 kHz otherwise. The audio system then converts between
/// them by an integer ratio instead of resampling 44.1 kHz material to
/// 48 kHz or the other way around.
pub fn processing_rate(device_rate: u32) -> u32 {
    if device_rate.is_multiple_of(CD_RATE) && !device_rate.is_multiple_of(DEFAULT_RATE) {
        CD_RATE
    } else {
        DEFAULT_RATE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processing_rate_follows_the_device_rate_family() {
        for (device, expected) in [
            (44_100, 44_100),
            (88_200, 44_100),
            (352_800, 44_100),
            (48_000, 48_000),
            (96_000, 48_000),
            (384_000, 48_000),
            // A multiple of both: 48 kHz is the default family.
            (7_056_000, 48_000),
            // A multiple of neither.
            (32_000, 48_000),
        ] {
            assert_eq!(processing_rate(device), expected, "device rate {device}");
        }
    }

    #[test]
    fn output_goes_to_the_first_two_channels() {
        let shared = Arc::new(Shared::new(Default::default()));
        let mut engine = Engine::new(shared, true, false);
        let mut queue = engine.processor.output;
        queue.push([0.25, -0.5]).unwrap();

        let mut bytes = [0xffu8; 4 * 4 * 2];
        engine.output.fill(&mut bytes, 4, 2);
        let samples: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_le_bytes(b))
            .collect();
        assert_eq!(samples, [0.25, -0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);

        let mut mono = [0u8; 4];
        queue.push([0.25, -0.5]).unwrap();
        engine.output.fill(&mut mono, 1, 1);
        assert_eq!(f32::from_le_bytes(mono), -0.125);
    }
}
