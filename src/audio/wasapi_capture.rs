//! The Windows backend. Windows has no virtual sink without a driver, so
//! WooferSim captures audio instead, from one of two sources:
//!
//! - Everything other apps play, through process loopback with WooferSim's
//!   own process excluded, so its output never loops back in. The original
//!   audio still reaches the speakers directly, so the output only adds the
//!   simulated bass.
//! - A capture device, such as the output end of a virtual cable that apps
//!   play into. The output then carries the treble and the simulated bass,
//!   like the PipeWire sink on Linux.
//!
//! Capture, playback and FIR crossover building each get a thread. Both
//! streams run at 44.1 or 48 kHz, whichever the device rate is a multiple
//! of, and Windows converts to and from the device format.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use wasapi::{
    AudioClient, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat, initialize_mta,
};

use super::engine::{
    Engine, FIR_CHECK_INTERVAL, OutputControls, OutputReader, Processor, processing_rate,
};
use super::{AudioDevices, AvailableDevices, Shared};

const CHANNELS: usize = 2;
/// Longest a thread waits for its device before checking whether to stop.
const WAIT: Duration = Duration::from_millis(100);
/// Shared mode buffer length in 100 ns units, 20 ms.
const BUFFER_DURATION: i64 = 200_000;

/// Handle to the audio threads. Dropping it stops them.
pub struct Sink {
    quit: Arc<AtomicBool>,
    controls: OutputControls,
    threads: Vec<JoinHandle<()>>,
}

impl Sink {
    /// Starts capturing from `devices.input`, or everything other apps play
    /// when it is `None`, and waits until the capture runs. `output` decides
    /// whether playback on `devices.output` starts out active.
    pub fn spawn(
        shared: Arc<Shared>,
        output: bool,
        devices: &AudioDevices,
    ) -> anyhow::Result<Self> {
        let bass_only = devices.output_is_bass_only();
        let Engine {
            processor,
            mut fir_builder,
            output: reader,
            controls,
        } = Engine::new(shared, output, bass_only);
        let quit = Arc::new(AtomicBool::new(false));
        let mut sink = Self {
            quit: quit.clone(),
            controls: controls.clone(),
            threads: Vec::new(),
        };

        let (ready_tx, ready_rx) = mpsc::channel();
        let input = devices.input.clone();
        sink.threads.push(spawn("wasapi-capture", {
            let quit = quit.clone();
            move || {
                if let Err(error) = capture(processor, input.as_deref(), &quit, &ready_tx)
                    // Sending fails once startup is over, the error then
                    // goes to the log instead.
                    && let Err(mpsc::SendError(Err(error))) = ready_tx.send(Err(error))
                {
                    tracing::error!("Audio capture stopped: {error:#}");
                }
            }
        })?);
        // Dropping `sink` on an error stops the threads started so far.
        let rate = ready_rx
            .recv()
            .context("audio capture thread exited during startup")??;

        let output_device = devices.output.clone();
        sink.threads.push(spawn("wasapi-render", {
            let quit = quit.clone();
            move || {
                if let Err(error) = render(reader, rate, output_device.as_deref(), &controls, &quit)
                {
                    tracing::error!("Audio output unavailable: {error:#}");
                }
            }
        })?);
        sink.threads.push(spawn("wasapi-fir", move || {
            while !quit.load(Ordering::Relaxed) {
                fir_builder.tick();
                std::thread::sleep(FIR_CHECK_INTERVAL);
            }
        })?);
        Ok(sink)
    }

    /// Starts or stops playback. While stopped it does not keep the output
    /// device awake.
    pub fn set_output(&self, enabled: bool) {
        self.controls.set_enabled(enabled);
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> anyhow::Result<JoinHandle<()>> {
    Ok(std::thread::Builder::new().name(name.into()).spawn(f)?)
}

/// Names of the active capture and playback devices.
pub fn list_devices() -> anyhow::Result<AvailableDevices> {
    // COM has to be set up on the calling thread, and the main thread's
    // apartment belongs to the windowing library, so this gets its own.
    std::thread::spawn(|| {
        initialize_mta().ok()?;
        let enumerator = DeviceEnumerator::new()?;
        let names = |direction| -> anyhow::Result<Vec<String>> {
            let mut names = Vec::new();
            for device in &enumerator.get_device_collection(&direction)? {
                names.push(device?.get_friendlyname()?);
            }
            Ok(names)
        };
        Ok(AvailableDevices {
            inputs: names(Direction::Capture)?,
            outputs: names(Direction::Render)?,
        })
    })
    .join()
    .map_err(|_| anyhow!("the device listing thread panicked"))?
}

fn stream_format(rate: u32) -> WaveFormat {
    WaveFormat::new(32, 32, &SampleType::Float, rate as usize, CHANNELS, None)
}

fn shared_mode() -> StreamMode {
    StreamMode::EventsShared {
        // Windows converts between this format and the device's.
        autoconvert: true,
        buffer_duration_hns: BUFFER_DURATION,
    }
}

fn capture(
    mut processor: Processor,
    input: Option<&str>,
    quit: &AtomicBool,
    ready: &mpsc::Sender<anyhow::Result<u32>>,
) -> anyhow::Result<()> {
    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let (mut client, rate) = match input {
        None => {
            // A process loopback client has no format of its own. The apps
            // it captures play to the default output, so its rate decides.
            let device_rate = enumerator
                .get_default_device(&Direction::Render)?
                .get_iaudioclient()?
                .get_mixformat()?
                .get_samplespersec();
            // Captures every process but WooferSim's own process tree,
            // instead of only that tree.
            let include_tree = false;
            let client =
                AudioClient::new_application_loopback_client(std::process::id(), include_tree)
                    .context(
                        "could not capture the system audio, this needs Windows 10 2004 or newer",
                    )?;
            (client, processing_rate(device_rate))
        }
        Some(name) => {
            let client = enumerator
                .get_device_collection(&Direction::Capture)?
                .get_device_with_name(name)?
                .get_iaudioclient()?;
            let rate = processing_rate(client.get_mixformat()?.get_samplespersec());
            (client, rate)
        }
    };
    let format = stream_format(rate);
    client.initialize_client(&format, &Direction::Capture, &shared_mode())?;
    let event = client.set_get_eventhandle()?;
    let capture_client = client.get_audiocaptureclient()?;
    let frame_bytes = format.get_blockalign() as usize;
    // A tenth of a second, far more than a packet holds. It only grows if a
    // packet is larger after all.
    let mut buffer = vec![0u8; rate as usize / 10 * frame_bytes];
    client.start_stream()?;
    tracing::info!(
        "Capturing {} at {rate} Hz",
        input.unwrap_or("the system audio")
    );
    let _ = ready.send(Ok(rate));

    while !quit.load(Ordering::Relaxed) {
        // Times out while nothing plays, which only means there is nothing
        // to read yet.
        let _ = event.wait_for_event(WAIT.as_millis() as u32);
        loop {
            let frames = capture_client.get_next_packet_size()?.unwrap_or(0) as usize;
            if frames == 0 {
                break;
            }
            let arrival = Instant::now();
            if buffer.len() < frames * frame_bytes {
                buffer.resize(frames * frame_bytes, 0);
            }
            let (read, info) = capture_client.read_from_device(&mut buffer)?;
            let bytes = &mut buffer[..read as usize * frame_bytes];
            // Silent packets may hold anything.
            if info.flags.silent {
                bytes.fill(0);
            }
            processor.process(bytes, CHANNELS, rate, arrival);
        }
    }
    client.stop_stream()?;
    Ok(())
}

fn render(
    mut reader: OutputReader,
    rate: u32,
    output: Option<&str>,
    controls: &OutputControls,
    quit: &AtomicBool,
) -> anyhow::Result<()> {
    initialize_mta().ok()?;
    let enumerator = DeviceEnumerator::new()?;
    let device = match output {
        None => enumerator.get_default_device(&Direction::Render)?,
        Some(name) => enumerator
            .get_device_collection(&Direction::Render)?
            .get_device_with_name(name)?,
    };
    let mut client = device.get_iaudioclient()?;
    let format = stream_format(rate);
    client.initialize_client(&format, &Direction::Render, &shared_mode())?;
    let event = client.set_get_eventhandle()?;
    let render_client = client.get_audiorenderclient()?;
    let frame_bytes = format.get_blockalign() as usize;
    let mut buffer = vec![0u8; client.get_buffer_size()? as usize * frame_bytes];

    let mut running = false;
    while !quit.load(Ordering::Relaxed) {
        let enabled = controls.enabled();
        if enabled != running {
            if enabled {
                client.start_stream()?;
            } else {
                client.stop_stream()?;
                // Drops what is still buffered, so it does not play when
                // the output is switched back on.
                client.reset_stream()?;
            }
            running = enabled;
        }
        if !running {
            std::thread::sleep(WAIT);
            continue;
        }
        let _ = event.wait_for_event(WAIT.as_millis() as u32);
        let frames = client.get_available_space_in_frames()? as usize;
        if frames == 0 {
            continue;
        }
        let bytes = &mut buffer[..frames * frame_bytes];
        reader.fill(bytes, CHANNELS, frames);
        render_client.write_to_device(frames, bytes, None)?;
    }
    if running {
        client.stop_stream()?;
    }
    Ok(())
}
