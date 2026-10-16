//! The Linux backend, a PipeWire virtual sink. It runs its own main loop on
//! a dedicated thread and filters every buffer it receives into cone
//! excursion. Optionally it also plays the treble of the input plus the
//! simulated subwoofer through a second, playback stream.
//!
//! Both streams run at 44.1 or 48 kHz, whichever the graph rate is a multiple
//! of, so PipeWire converts between them by an integer ratio.

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::param::audio::{AudioFormat, AudioInfoRaw, MAX_CHANNELS};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;

use super::engine::{
    DEFAULT_RATE, Engine, FIR_CHECK_INTERVAL, OutputReader, Processor, processing_rate,
};
use super::{AudioDevices, Shared};

const NODE_NAME: &str = "woofersim";
const NODE_DESCRIPTION: &str = "WooferSim";
const OUTPUT_NODE_NAME: &str = "woofersim.output";
const OUTPUT_NODE_DESCRIPTION: &str = "WooferSim output";

/// How often the main loop checks whether the graph rate moved to the other
/// rate family.
const RATE_CHECK_INTERVAL: Duration = Duration::from_millis(250);
const CHANNELS: u32 = 2;
const FRAME_BYTES: usize = CHANNELS as usize * size_of::<f32>();

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
    /// PipeWire has no device choice to make, apps pick the sink themselves.
    pub fn spawn(shared: Arc<Shared>, output: bool, _: &AudioDevices) -> anyhow::Result<Self> {
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
    processor: Processor,
    format: AudioInfoRaw,
    /// Graph rate seen by the last processed buffer, 0 until then.
    graph_rate: Arc<AtomicU32>,
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

    let Engine {
        processor,
        fir_builder,
        output: output_reader,
        controls,
    } = Engine::new(shared, output, false);
    let graph_rate = Arc::new(AtomicU32::new(0));
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
        processor,
        format: AudioInfoRaw::new(),
        graph_rate: graph_rate.clone(),
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
        .add_local_listener_with_user_data(output_reader)
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
                controls.set_enabled(enabled);
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
        let fir_builder = RefCell::new(fir_builder);
        move |_| fir_builder.borrow_mut().tick()
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
    let arrival = Instant::now();
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
    let start = data.chunk().offset() as usize;
    let end = start + data.chunk().size() as usize;
    let Some(bytes) = data.data().and_then(|bytes| bytes.get(start..end)) else {
        return;
    };
    let channels = state.format.channels() as usize;
    state
        .processor
        .process(bytes, channels, state.format.rate(), arrival);
}

fn process_output(stream: &pw::stream::Stream, output: &mut OutputReader) {
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
    output.fill(bytes, CHANNELS as usize, frames);

    let chunk = data.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = FRAME_BYTES as i32;
    *chunk.size_mut() = (frames * FRAME_BYTES) as u32;
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
