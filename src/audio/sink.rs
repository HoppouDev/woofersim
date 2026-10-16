//! The PipeWire virtual sink. It runs its own main loop on a dedicated thread
//! and filters every buffer it receives into cone excursion. Optionally it
//! also plays the treble of the input plus the simulated subwoofer through a
//! second, playback stream.
//!
//! Both streams run at 44.1 or 48 kHz, whichever the graph rate is a multiple
//! of, so PipeWire converts between them by an integer ratio instead of
//! resampling 44.1 kHz material to 48 kHz or the other way around.
//!
//! FIR crossovers are built on the main loop thread whenever the crossover
//! frequency or sample rate changes, handed to the audio thread through a
//! queue, and handed back the same way to be freed, because building and
//! freeing them allocates.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use rtrb::{Consumer, Producer, RingBuffer};
use spa::param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;

use super::Shared;
use super::dsp::WooferModel;
use super::fir::FirCrossover;
use super::history::HistoryWriter;

const NODE_NAME: &str = "woofersim";
const NODE_DESCRIPTION: &str = "WooferSim";
const OUTPUT_NODE_NAME: &str = "woofersim.output";
const OUTPUT_NODE_DESCRIPTION: &str = "WooferSim output";
/// Processing rate used until the graph rate is known, and for graph rates
/// that are a multiple of neither family.
const DEFAULT_RATE: u32 = 48_000;
const CD_RATE: u32 = 44_100;

/// How often the main loop checks whether the graph rate moved to the other
/// rate family.
const RATE_CHECK_INTERVAL: Duration = Duration::from_millis(250);
/// How often the main loop checks whether the FIR crossover needs rebuilding.
const FIR_CHECK_INTERVAL: Duration = Duration::from_millis(100);
/// Crossovers in flight between the threads in either direction.
const FIR_QUEUE: usize = 4;
/// Frames decoded from a buffer at a time.
const DECODE_FRAMES: usize = 4096;
const CHANNELS: u32 = 2;
const FRAME_BYTES: usize = CHANNELS as usize * size_of::<f32>();

/// Frames passed from the sink to the playback stream, 170 ms at 48 kHz.
/// Only a couple of buffers are ever in flight, the rest is headroom.
const OUTPUT_QUEUE_FRAMES: usize = 8192;

/// Most buffers' worth of audio the playback stream lets queue up before it
/// drops the oldest frames. Bounds the latency after the output is switched
/// back on, when the queue still holds audio from before.
const MAX_QUEUED_BUFFERS: usize = 2;

/// Handle to the sink thread. Dropping it removes the sink from the graph.
pub struct Sink {
    commands: pw::channel::Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

enum Command {
    SetOutput(bool),
    Quit,
}

impl Sink {
    /// Starts the sink and waits until it is registered with PipeWire.
    /// `output` decides whether the playback stream starts out active.
    pub fn spawn(shared: Arc<Shared>, output: bool) -> anyhow::Result<Self> {
        let (commands, commands_rx) = pw::channel::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("pipewire-sink".into())
            .spawn(move || {
                if let Err(error) = run(shared, output, commands_rx, &ready_tx) {
                    let _ = ready_tx.send(Err(error));
                }
            })?;
        ready_rx
            .recv()
            .context("PipeWire sink thread exited during startup")??;
        Ok(Self {
            commands,
            thread: Some(thread),
        })
    }

    /// Starts or pauses the playback stream. While paused it does not keep
    /// the output device awake.
    pub fn set_output(&self, enabled: bool) {
        // Fails only when the loop has already stopped.
        let _ = self.commands.send(Command::SetOutput(enabled));
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Quit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct SinkState {
    shared: Arc<Shared>,
    format: AudioInfoRaw,
    model: WooferModel,
    history: HistoryWriter,
    output: Producer<[f32; 2]>,
    /// Whether the playback stream is on. While it is off nothing is queued
    /// for it, so it never starts with old audio.
    output_enabled: Arc<AtomicBool>,
    /// Graph rate seen by the last processed buffer, 0 until then.
    graph_rate: Arc<AtomicU32>,
    /// Negotiated sample rate, for building FIR crossovers.
    stream_rate: Arc<AtomicU32>,
    new_firs: Consumer<Box<FirCrossover>>,
    old_firs: Producer<Box<FirCrossover>>,
    frames: Vec<[f32; 2]>,
}

/// State of the playback stream's process callback.
struct OutputState {
    queue: Consumer<[f32; 2]>,
    /// Set when the stream is switched on, to drop whatever is still queued
    /// from before it was switched off.
    flush: Arc<AtomicBool>,
}

fn run(
    shared: Arc<Shared>,
    output: bool,
    commands: pw::channel::Receiver<Command>,
    ready: &mpsc::Sender<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context
        .connect_rc(None)
        .context("could not connect to the PipeWire daemon")?;

    let (producer, consumer) = RingBuffer::new(OUTPUT_QUEUE_FRAMES);
    let (new_firs_tx, new_firs) = RingBuffer::new(FIR_QUEUE);
    // Room for every crossover the audio thread can hold besides the one in
    // use: the whole incoming queue, plus incoming, pending and retired.
    let (old_firs, old_firs_rx) = RingBuffer::new(FIR_QUEUE + 3);
    let graph_rate = Arc::new(AtomicU32::new(0));
    let stream_rate = Arc::new(AtomicU32::new(0));
    let output_enabled = Arc::new(AtomicBool::new(output));
    let flush_output = Arc::new(AtomicBool::new(false));
    let fir_shared = shared.clone();
    let format_bytes = enum_format(DEFAULT_RATE)?;
    let format = || Pod::from_bytes(&format_bytes).ok_or_else(|| anyhow!("invalid format pod"));

    // Both streams form one group, like module-loopback: the group runs on
    // one clock, so the queue between them neither fills up nor runs dry,
    // and the session manager never links the playback stream back into
    // the sink, even when the sink is the default output.
    let sink = pw::stream::StreamRc::new(
        core.clone(),
        NODE_NAME,
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CLASS => "Audio/Sink",
            *pw::keys::NODE_NAME => NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => NODE_DESCRIPTION,
            *pw::keys::NODE_GROUP => NODE_NAME,
            *pw::keys::NODE_LINK_GROUP => NODE_NAME,
        },
    )?;
    let state = SinkState {
        shared,
        format: AudioInfoRaw::new(),
        model: WooferModel::default(),
        history: HistoryWriter::default(),
        output: producer,
        output_enabled: output_enabled.clone(),
        graph_rate: graph_rate.clone(),
        stream_rate: stream_rate.clone(),
        new_firs,
        old_firs,
        frames: vec![[0.0; 2]; DECODE_FRAMES],
    };
    let _sink_listener = sink
        .add_local_listener_with_user_data(state)
        .param_changed(|_, state, id, param| {
            // None clears the format, process() is not called until a new one arrives.
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            match format_utils::parse_format(param) {
                Ok((MediaType::Audio, MediaSubtype::Raw)) => {}
                _ => return,
            }
            if let Err(error) = state.format.parse(param) {
                tracing::error!("Unsupported PipeWire audio format: {error}");
            }
        })
        .process(process_input)
        .register()?;
    sink.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
        &mut [format()?],
    )?;

    let playback = pw::stream::StreamRc::new(
        core.clone(),
        OUTPUT_NODE_NAME,
        properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Music",
            *pw::keys::NODE_NAME => OUTPUT_NODE_NAME,
            *pw::keys::NODE_DESCRIPTION => OUTPUT_NODE_DESCRIPTION,
            *pw::keys::NODE_GROUP => NODE_NAME,
            *pw::keys::NODE_LINK_GROUP => NODE_NAME,
        },
    )?;
    let _playback_listener = playback
        .add_local_listener_with_user_data(OutputState {
            queue: consumer,
            flush: flush_output.clone(),
        })
        .process(process_output)
        .register()?;
    let mut flags = pw::stream::StreamFlags::AUTOCONNECT
        | pw::stream::StreamFlags::MAP_BUFFERS
        | pw::stream::StreamFlags::RT_PROCESS;
    if !output {
        flags |= pw::stream::StreamFlags::INACTIVE;
    }
    playback.connect(spa::utils::Direction::Output, None, flags, &mut [format()?])?;

    let _commands = commands.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        let playback = playback.clone();
        move |command| match command {
            Command::SetOutput(enabled) => {
                if enabled {
                    flush_output.store(true, Ordering::Release);
                }
                output_enabled.store(enabled, Ordering::Release);
                if let Err(error) = playback.set_active(enabled) {
                    tracing::error!("Could not switch the audio output: {error}");
                }
            }
            Command::Quit => mainloop.quit(),
        }
    });

    let rate_check = mainloop.loop_().add_timer({
        let streams = [sink.clone(), playback.clone()];
        let current = Cell::new(DEFAULT_RATE);
        move |_| {
            let graph = graph_rate.load(Ordering::Relaxed);
            let wanted = processing_rate(graph);
            if graph == 0 || wanted == current.get() {
                return;
            }
            let Some(bytes) = enum_format(wanted).ok() else {
                return;
            };
            let Some(pod) = Pod::from_bytes(&bytes) else {
                return;
            };
            // Remembered only once both streams took it, so a failed switch
            // is tried again on the next tick.
            let mut switched = true;
            for stream in &streams {
                if let Err(error) = stream.update_params(&mut [pod]) {
                    tracing::error!("Could not switch to {wanted} Hz: {error}");
                    switched = false;
                }
            }
            if switched {
                tracing::info!("Graph runs at {graph} Hz, processing at {wanted} Hz");
                current.set(wanted);
            }
        }
    });
    rate_check.update_timer(Some(RATE_CHECK_INTERVAL), Some(RATE_CHECK_INTERVAL));

    let fir_check = mainloop.loop_().add_timer({
        let new_firs_tx = std::cell::RefCell::new(new_firs_tx);
        let old_firs_rx = std::cell::RefCell::new(old_firs_rx);
        let built = Cell::new(None::<(f32, u32)>);
        move |_| {
            // Dropping them here frees them off the audio thread.
            while old_firs_rx.borrow_mut().pop().is_ok() {}

            let rate = stream_rate.load(Ordering::Relaxed);
            let wanted = (fir_shared.lowpass_hz.load(), rate);
            // Built in IIR mode too, because the FIR crossover keeps running
            // there and must have the right cutoff when switched back to.
            // Nothing is built while the queue is full, for example while no
            // audio flows, since the result could not be handed over.
            if rate == 0 || built.get() == Some(wanted) || new_firs_tx.borrow().is_full() {
                return;
            }
            let crossover = Box::new(FirCrossover::new(wanted.0, rate as f32));
            if new_firs_tx.borrow_mut().push(crossover).is_ok() {
                built.set(Some(wanted));
            }
        }
    });
    fir_check.update_timer(Some(FIR_CHECK_INTERVAL), Some(FIR_CHECK_INTERVAL));
    let _ = ready.send(Ok(()));
    tracing::info!("PipeWire sink \"{NODE_DESCRIPTION}\" ready");
    mainloop.run();
    // Stops the realtime callbacks before the listeners holding their state
    // are dropped at the end of this function.
    let _ = playback.disconnect();
    let _ = sink.disconnect();
    Ok(())
}

fn process_input(stream: &pw::stream::Stream, state: &mut SinkState) {
    let arrival = state.shared.excursion.seconds(Instant::now());
    if let Ok(time) = stream.time()
        && let Some(graph_rate) = time.rate().denom.checked_div(time.rate().num)
    {
        state.graph_rate.store(graph_rate, Ordering::Relaxed);
    }
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let channels = state.format.channels() as usize;
    if channels == 0 {
        return;
    }
    let start = data.chunk().offset() as usize;
    let end = start + data.chunk().size() as usize;
    let Some(bytes) = data.data().and_then(|bytes| bytes.get(start..end)) else {
        return;
    };

    let rate = state.format.rate();
    state.stream_rate.store(rate, Ordering::Relaxed);
    state
        .model
        .configure(state.shared.woofer_params(rate as f32));
    // A replaced crossover goes back to the main loop to be freed, so only
    // take new ones while there is room to return one.
    while state.old_firs.slots() > 1
        && let Ok(crossover) = state.new_firs.pop()
    {
        if let Some(superseded) = state.model.install_fir(crossover) {
            let _ = state.old_firs.push(superseded);
        }
    }

    let history = &state.shared.excursion;
    let frame_bytes = channels * size_of::<f32>();
    let buffer_len = (bytes.len() / frame_bytes) as u32;
    for encoded in bytes.chunks(DECODE_FRAMES * frame_bytes) {
        let frames = &mut state.frames[..encoded.len() / frame_bytes];
        for (frame, encoded) in frames.iter_mut().zip(encoded.chunks_exact(frame_bytes)) {
            let (samples, _) = encoded.as_chunks::<4>();
            let left = f32::from_le_bytes(samples[0]);
            let right = samples
                .get(1)
                .map_or(left, |&sample| f32::from_le_bytes(sample));
            *frame = [left, right];
        }
        let (writer, output) = (&mut state.history, &mut state.output);
        let playing = state.output_enabled.load(Ordering::Acquire);
        state.model.process_block(frames, |frame| {
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
    if state.old_firs.slots() > 0
        && let Some(retired) = state.model.take_retired_fir()
    {
        let _ = state.old_firs.push(retired);
    }
    state
        .history
        .finish_buffer(history, arrival, rate, buffer_len);
}

fn process_output(stream: &pw::stream::Stream, state: &mut OutputState) {
    let queue = &mut state.queue;
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let requested = buffer.requested() as usize;
    let Some(data) = buffer.datas_mut().first_mut() else {
        return;
    };
    let Some(bytes) = data.data() else {
        return;
    };
    let capacity = bytes.len() / FRAME_BYTES;
    let frames = if requested == 0 {
        capacity
    } else {
        requested.min(capacity)
    };

    let keep = if state.flush.swap(false, Ordering::AcqRel) {
        0
    } else {
        frames * MAX_QUEUED_BUFFERS
    };
    let stale = queue.slots().saturating_sub(keep);
    if let Ok(chunk) = queue.read_chunk(stale) {
        chunk.commit_all();
    }
    let (out_frames, _) = bytes[..frames * FRAME_BYTES].as_chunks_mut::<FRAME_BYTES>();
    for out in out_frames {
        // Silence when the sink has nothing new, for example when nothing
        // plays into it.
        let [left, right] = queue.pop().unwrap_or_default();
        out[..4].copy_from_slice(&left.to_le_bytes());
        out[4..].copy_from_slice(&right.to_le_bytes());
    }

    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = FRAME_BYTES as i32;
    *chunk.size_mut() = (frames * FRAME_BYTES) as u32;
}

/// The processing rate for a graph rate: 44.1 kHz when the graph runs at a
/// multiple of it, 48 kHz otherwise.
fn processing_rate(graph_rate: u32) -> u32 {
    if graph_rate.is_multiple_of(CD_RATE) && !graph_rate.is_multiple_of(DEFAULT_RATE) {
        CD_RATE
    } else {
        DEFAULT_RATE
    }
}

/// Stereo float audio at `rate`. PipeWire converts whatever the client plays
/// into this on the way in, and converts it to the device format on the way
/// out.
fn enum_format(rate: u32) -> anyhow::Result<Vec<u8>> {
    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(rate);
    info.set_channels(CHANNELS);
    let mut position = [0; MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);

    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    let (cursor, _) = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processing_rate_follows_the_graph_rate_family() {
        for (graph, expected) in [
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
            assert_eq!(processing_rate(graph), expected, "graph rate {graph}");
        }
    }
}
