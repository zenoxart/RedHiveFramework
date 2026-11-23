//! Real-time audio and MIDI devices: the output stream that drives the [`Renderer`], the input stream for
//! monitoring and recording, the recorder that writes takes to disk on a worker thread, and MIDI input.
//!
//! Threads:
//! * **audio thread** (device callback) – applies commands, renders, monitors input, hands input blocks to
//!   the recorder; never touches the disk.
//! * **recorder thread** – writes armed inputs to WAV files.
//! * **MIDI thread** (midir callback) – forwards messages to the UI with the exact transport beat.

use crate::model::{Id, InputSource};
use crate::render::{LiveEvent, Meters, PlayMode, Renderer, Song, TransportState};
use crate::{EngineError, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DeviceConfig {
    /// Audio host / driver (`ALSA`, `JACK`, `WASAPI`, `ASIO`, `CoreAudio`); `None` = default.
    pub host: Option<String>,
    pub output: Option<String>,
    pub input: Option<String>,
    pub sample_rate: Option<u32>,
    /// Buffer size in frames; `None` = driver default.
    pub buffer: Option<u32>,
    /// Extra recording offset in milliseconds (positive = recordings are moved earlier).
    pub record_offset_ms: f32,
    pub midi_inputs: Vec<String>,
}

/// Whether this build can use ASIO drivers (Windows builds made with the `asio` feature).
pub const ASIO: bool = cfg!(all(windows, feature = "asio"));

pub fn hosts() -> Vec<String> {
    cpal::available_hosts().into_iter().map(|h| h.name().to_string()).collect()
}

fn host(name: Option<&str>) -> cpal::Host {
    name.and_then(|n| cpal::available_hosts().into_iter().find(|h| h.name() == n)).and_then(|id| cpal::host_from_id(id).ok()).unwrap_or_else(cpal::default_host)
}

pub fn output_devices(host_name: Option<&str>) -> Vec<String> {
    host(host_name).output_devices().map(|d| d.filter_map(|x| x.name().ok()).collect()).unwrap_or_default()
}

pub fn input_devices(host_name: Option<&str>) -> Vec<String> {
    host(host_name).input_devices().map(|d| d.filter_map(|x| x.name().ok()).collect()).unwrap_or_default()
}

/// Commands from the UI to the audio thread.
pub enum Command {
    SetSong(Arc<Song>),
    Play(PlayMode),
    Pause,
    Stop,
    Seek(f64),
    Record { count_in_bars: u32 },
    Live(LiveEvent),
}

/// A finished recording of one track.
#[derive(Debug, Clone)]
pub struct RecordedTake {
    pub track: Id,
    pub path: PathBuf,
    /// Beat where the recording starts on the timeline (after count-in).
    pub start_beat: f64,
    pub frames: usize,
    pub rate: u32,
    /// Seconds to skip at the start of the file to compensate the round-trip latency.
    pub latency: f64,
}

/// Where and what to record.
#[derive(Debug, Clone)]
pub struct RecordTarget {
    pub track: Id,
    pub source: InputSource,
    pub path: PathBuf,
}

enum RecMsg {
    Start { targets: Vec<RecordTarget>, rate: u32 },
    Block { first_beat: f64, channels: Vec<Vec<f32>> },
    Finish { latency: f64, reply: Sender<Vec<RecordedTake>> },
}

fn recorder_thread(rx: Receiver<RecMsg>) {
    struct Open {
        target: RecordTarget,
        writer: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
        frames: usize,
    }
    let mut open: Vec<Open> = Vec::new();
    let mut start_beat: Option<f64> = None;
    let mut rate = 48000;
    for msg in rx {
        match msg {
            RecMsg::Start { targets, rate: r } => {
                rate = r;
                start_beat = None;
                open.clear();
                for t in targets {
                    if let Some(dir) = t.path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let channels = if t.source.right.is_some() { 2 } else { 1 };
                    let spec = hound::WavSpec { channels, sample_rate: r, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
                    match hound::WavWriter::create(&t.path, spec) {
                        Ok(writer) => open.push(Open { target: t, writer, frames: 0 }),
                        Err(e) => tracing::error!("cannot create {}: {e}", t.path.display()),
                    }
                }
            }
            RecMsg::Block { first_beat, channels } => {
                if start_beat.is_none() {
                    start_beat = Some(first_beat);
                }
                for o in &mut open {
                    let l = channels.get(o.target.source.left as usize);
                    let r = o.target.source.right.and_then(|r| channels.get(r as usize));
                    let Some(l) = l else { continue };
                    for i in 0..l.len() {
                        let _ = o.writer.write_sample(l[i]);
                        if let Some(r) = r {
                            let _ = o.writer.write_sample(r.get(i).copied().unwrap_or(0.0));
                        }
                    }
                    o.frames += l.len();
                }
            }
            RecMsg::Finish { latency, reply } => {
                let mut out = Vec::new();
                for o in open.drain(..) {
                    let frames = o.frames;
                    let _ = o.writer.finalize();
                    if frames > 0 {
                        out.push(RecordedTake { track: o.target.track, path: o.target.path, start_beat: start_beat.unwrap_or(0.0), frames, rate, latency });
                    } else {
                        let _ = std::fs::remove_file(&o.target.path);
                    }
                }
                let _ = reply.send(out);
            }
        }
    }
}

/// Shared view of the transport, updated by the audio thread.
#[derive(Debug, Clone, Copy)]
pub struct SharedTransport {
    pub state: TransportState,
    pub at: Instant,
}

impl SharedTransport {
    /// Beat estimated for now (between audio callbacks).
    pub fn beat_now(&self) -> f64 {
        if self.state.playing {
            self.state.beat + self.at.elapsed().as_secs_f64() * self.state.bpm / 60.0
        } else {
            self.state.beat
        }
    }
}

/// Liveness of the output stream: the callback counts its calls, the error callback flags a failure.
#[derive(Default)]
pub struct StreamHealth {
    pub callbacks: std::sync::atomic::AtomicU64,
    pub failed: std::sync::atomic::AtomicBool,
    /// Frames of the last callback: the buffer the driver really runs (ASIO may override the request).
    pub frames: std::sync::atomic::AtomicU32,
    /// Output latency the driver reports (callback → speaker), µs; 0 = not reported.
    pub out_lat_us: std::sync::atomic::AtomicU32,
    /// Input latency the driver reports (microphone → callback), µs; 0 = not reported.
    pub in_lat_us: std::sync::atomic::AtomicU32,
    /// Input frames still waiting in the FIFO after each output callback took its block.
    pub fifo: std::sync::atomic::AtomicU32,
}

fn note_out_latency(h: &StreamHealth, info: &cpal::OutputCallbackInfo) {
    let t = info.timestamp();
    if let Some(d) = t.playback.duration_since(&t.callback) {
        h.out_lat_us.store(d.as_micros().min(1_000_000) as u32, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The running audio engine.
/// Joins `threads`, giving up after `limit` in total (a plugin hanging in its destructor must not
/// hang the app on close).
pub fn join_bounded(threads: Vec<std::thread::JoinHandle<()>>, limit: Duration) {
    let until = Instant::now() + limit;
    for t in threads {
        while !t.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
        if t.is_finished() {
            let _ = t.join();
        } else {
            tracing::warn!("engine thread {:?} did not stop in time", t.thread().name());
        }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub struct AudioEngine {
    tx: Sender<Command>,
    rec_tx: Sender<RecMsg>,
    recording: Arc<Mutex<bool>>,
    pub meters: Meters,
    pub transport: Arc<Mutex<SharedTransport>>,
    pub rate: u32,
    pub buffer: u32,
    pub output_name: String,
    pub input_name: Option<String>,
    pub input_channels: u16,
    /// Why no input is open (device missing or refused), for the Options and the recording panel.
    pub input_error: Option<String>,
    pub latency_frames: u32,
    record_offset: f64,
    pub health: Arc<StreamHealth>,
    out: Option<cpal::Stream>,
    input: Option<cpal::Stream>,
    /// The recorder and song-collector threads, joined on shutdown (they may hold plugins).
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl AudioEngine {
    pub fn start(cfg: &DeviceConfig, song: Arc<Song>) -> Result<AudioEngine> {
        let h = host(cfg.host.as_deref());
        let out_dev = match &cfg.output {
            Some(n) => h.output_devices().map_err(|e| EngineError::Device(e.to_string()))?.find(|d| d.name().ok().as_deref() == Some(n)).or_else(|| h.default_output_device()),
            None => h.default_output_device(),
        }
        .ok_or_else(|| EngineError::Device("no audio output device".into()))?;
        let def = out_dev.default_output_config().map_err(|e| EngineError::Device(e.to_string()))?;
        let rate = cfg.sample_rate.unwrap_or(def.sample_rate().0);
        let channels = def.channels().max(1);
        // A fixed buffer the driver can't do (ASIO drivers report their range) is clamped to its range.
        let buffer = match (cfg.buffer, def.buffer_size()) {
            (Some(b), cpal::SupportedBufferSize::Range { min, max }) => cpal::BufferSize::Fixed(b.clamp(*min, (*max).max(*min))),
            (Some(b), cpal::SupportedBufferSize::Unknown) => cpal::BufferSize::Fixed(b),
            (None, _) => cpal::BufferSize::Default,
        };
        let config = cpal::StreamConfig { channels, sample_rate: cpal::SampleRate(rate), buffer_size: buffer };

        // Input (optional).
        let in_dev = match &cfg.input {
            Some(n) => h.input_devices().ok().and_then(|mut d| d.find(|x| x.name().ok().as_deref() == Some(n))),
            None => h.default_input_device(),
        };
        let health = Arc::new(StreamHealth::default());
        let input_buf: Arc<Mutex<Vec<VecDeque<f32>>>> = Arc::new(Mutex::new(Vec::new()));
        let mut input_channels = 0;
        let mut in_stream = None;
        let mut input_name = None;
        let mut input_error = None;
        match in_dev {
            Some(dev) => match open_input(&dev, rate, buffer, &input_buf, health.clone()) {
                Ok((stream, ich)) => {
                    in_stream = Some(stream);
                    input_channels = ich;
                    input_name = dev.name().ok();
                }
                Err(e) => {
                    tracing::warn!("no input stream: {e}");
                    input_error = Some(format!("{}: {e}", dev.name().unwrap_or_else(|_| "input".into())));
                }
            },
            None => {
                input_error = Some(match &cfg.input {
                    Some(n) => format!("the input \"{n}\" was not found"),
                    None => "no input device found".into(),
                })
            }
        }

        let (tx, rx) = unbounded::<Command>();
        let (rec_tx, rec_rx) = unbounded::<RecMsg>();
        let recorder = std::thread::Builder::new().name("dragon-recorder".into()).spawn(move || recorder_thread(rec_rx)).map_err(|e| EngineError::Device(e.to_string()))?;
        let recording = Arc::new(Mutex::new(false));
        let mut renderer = Renderer::new(song);
        // Old songs are dropped here, not in the audio callback (freeing can block on the allocator).
        let (gc_tx, gc_rx) = crossbeam_channel::bounded::<Arc<Song>>(64);
        let collector = std::thread::Builder::new().name("dragon-gc".into()).spawn(move || for _ in gc_rx {}).map_err(|e| EngineError::Device(e.to_string()))?;
        // Health: the callback counts its calls; the error callback flags a dead stream.
        let hb = health.clone();
        let meters = renderer.meters.clone();
        let transport = Arc::new(Mutex::new(SharedTransport { state: renderer.transport, at: Instant::now() }));
        let tr = transport.clone();
        let rec_flag = recording.clone();
        let rtx = rec_tx.clone();
        let ib = input_buf.clone();
        let mut l = vec![0.0f32; 8192];
        let mut r = vec![0.0f32; 8192];
        let mut inputs: Vec<Vec<f32>> = Vec::new();
        let ch = channels as usize;
        let mut process = move |n: usize| -> (*const f32, *const f32) {
            while let Ok(c) = rx.try_recv() {
                match c {
                    Command::SetSong(s) => {
                        let old = renderer.set_song(s);
                        // If the collector is behind, the song is dropped here after all (rare).
                        let _ = gc_tx.try_send(old);
                    }
                    Command::Play(m) => renderer.play(m),
                    Command::Pause => renderer.pause(),
                    Command::Stop => renderer.stop(),
                    Command::Seek(b) => renderer.seek(b),
                    Command::Record { count_in_bars } => renderer.record(count_in_bars),
                    Command::Live(e) => renderer.push_live(e),
                }
            }
            if l.len() < n {
                l.resize(n, 0.0);
                r.resize(n, 0.0);
            }
            // Pull exactly n frames of input per channel (zeros when the input is late).
            if let Ok(mut q) = ib.try_lock() {
                inputs.resize(q.len(), Vec::new());
                for (k, chq) in q.iter_mut().enumerate() {
                    inputs[k].clear();
                    for _ in 0..n {
                        inputs[k].push(chq.pop_front().unwrap_or(0.0));
                    }
                    // Keep latency bounded if the input runs ahead.
                    while chq.len() > n * 4 {
                        chq.pop_front();
                    }
                }
                if let Some(c) = q.first() {
                    hb.fifo.store(c.len() as u32, std::sync::atomic::Ordering::Relaxed);
                }
            } else {
                for v in &mut inputs {
                    v.clear();
                    v.resize(n, 0.0);
                }
            }
            // Input channel slices without allocating (up to 32 channels).
            let mut refs: [&[f32]; 32] = [&[]; 32];
            let k = inputs.len().min(refs.len());
            for (slot, v) in refs.iter_mut().zip(inputs.iter()) {
                *slot = v.as_slice();
            }
            // A panic inside the render (a bug in an effect or instrument) must not take the app down:
            // play silence, report which insert failed and let the app rebuild the engine.
            if hb.failed.load(std::sync::atomic::Ordering::Relaxed) {
                l[..n].fill(0.0);
                r[..n].fill(0.0);
            } else {
                crate::fault::set_in_audio(true);
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| renderer.render(&mut l[..n], &mut r[..n], &refs[..k])));
                crate::fault::set_in_audio(false);
                if let Err(p) = res {
                    crate::fault::report(crate::fault::current(), crate::fault::message(&*p));
                    hb.failed.store(true, std::sync::atomic::Ordering::Relaxed);
                    l[..n].fill(0.0);
                    r[..n].fill(0.0);
                }
            }
            hb.callbacks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            hb.frames.store(n as u32, std::sync::atomic::Ordering::Relaxed);
            // Recording: hand input to the recorder once the count-in is over.
            let st = renderer.transport;
            let recording_now = st.recording && !renderer.counting_in();
            if recording_now && !inputs.is_empty() {
                let _ = rtx.send(RecMsg::Block { first_beat: renderer.last_block_beat.0, channels: inputs.clone() });
            }
            if let Ok(mut f) = rec_flag.try_lock() {
                *f = st.recording;
            }
            if let Ok(mut t) = tr.try_lock() {
                *t = SharedTransport { state: st, at: Instant::now() };
            }
            (l.as_ptr(), r.as_ptr())
        };
        let dead = health.clone();
        let err = move |e: cpal::StreamError| {
            tracing::error!("output stream: {e}");
            dead.failed.store(true, std::sync::atomic::Ordering::Relaxed);
        };
        let lat = health.clone();
        let stream = match def.sample_format() {
            cpal::SampleFormat::F32 => out_dev.build_output_stream(
                &config,
                move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                    note_out_latency(&lat, info);
                    let n = data.len() / ch;
                    let (pl, pr) = process(n);
                    // SAFETY: `process` returns pointers into its own buffers of at least n frames that stay
                    // alive (owned by the closure) until the next call.
                    let (l, r) = unsafe { (std::slice::from_raw_parts(pl, n), std::slice::from_raw_parts(pr, n)) };
                    for i in 0..n {
                        data[i * ch] = l[i];
                        if ch > 1 {
                            data[i * ch + 1] = r[i];
                        }
                        for k in 2..ch {
                            data[i * ch + k] = 0.0;
                        }
                    }
                },
                err,
                None,
            ),
            cpal::SampleFormat::I16 => out_dev.build_output_stream(
                &config,
                move |data: &mut [i16], info: &cpal::OutputCallbackInfo| {
                    note_out_latency(&lat, info);
                    let n = data.len() / ch;
                    let (pl, pr) = process(n);
                    // SAFETY: see above.
                    let (l, r) = unsafe { (std::slice::from_raw_parts(pl, n), std::slice::from_raw_parts(pr, n)) };
                    for i in 0..n {
                        data[i * ch] = (l[i].clamp(-1.0, 1.0) * 32767.0) as i16;
                        if ch > 1 {
                            data[i * ch + 1] = (r[i].clamp(-1.0, 1.0) * 32767.0) as i16;
                        }
                        for k in 2..ch {
                            data[i * ch + k] = 0;
                        }
                    }
                },
                err,
                None,
            ),
            cpal::SampleFormat::I32 => out_dev.build_output_stream(
                &config,
                move |data: &mut [i32], info: &cpal::OutputCallbackInfo| {
                    note_out_latency(&lat, info);
                    let n = data.len() / ch;
                    let (pl, pr) = process(n);
                    // SAFETY: see above.
                    let (l, r) = unsafe { (std::slice::from_raw_parts(pl, n), std::slice::from_raw_parts(pr, n)) };
                    for i in 0..n {
                        data[i * ch] = (l[i].clamp(-1.0, 1.0) as f64 * 2_147_483_647.0) as i32;
                        if ch > 1 {
                            data[i * ch + 1] = (r[i].clamp(-1.0, 1.0) as f64 * 2_147_483_647.0) as i32;
                        }
                        for k in 2..ch {
                            data[i * ch + k] = 0;
                        }
                    }
                },
                err,
                None,
            ),
            f => return Err(EngineError::Device(format!("unsupported sample format {f:?}"))),
        }
        .map_err(|e| EngineError::Device(e.to_string()))?;
        stream.play().map_err(|e| EngineError::Device(e.to_string()))?;
        let buffer_frames = cfg.buffer.unwrap_or(match def.buffer_size() {
            cpal::SupportedBufferSize::Range { min, max } => (*min).max(256).min(*max),
            cpal::SupportedBufferSize::Unknown => 512,
        });
        // Round trip ≈ one output buffer + one input buffer.
        let latency_frames = buffer_frames * if in_stream.is_some() { 2 } else { 1 };
        Ok(AudioEngine {
            tx,
            rec_tx,
            recording,
            meters,
            transport,
            rate,
            buffer: buffer_frames,
            output_name: out_dev.name().unwrap_or_default(),
            input_name,
            input_channels,
            input_error,
            latency_frames,
            record_offset: cfg.record_offset_ms as f64 / 1000.0,
            health,
            out: Some(stream),
            input: in_stream,
            threads: vec![recorder, collector],
        })
    }

    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }

    /// The buffer the driver actually runs (frames of the last callback; the requested size until audio
    /// has run).
    pub fn buffer_frames(&self) -> u32 {
        match self.health.frames.load(std::sync::atomic::Ordering::Relaxed) {
            0 => self.buffer,
            f => f,
        }
    }

    /// The round trip a recording is shifted back by, in seconds: the latencies the driver reports for
    /// output and input (one real buffer each when it reports none – ASIO may run another buffer than
    /// the one requested) plus the input frames waiting in the FIFO.
    pub fn record_latency(&self) -> f64 {
        use std::sync::atomic::Ordering::Relaxed;
        let rate = self.rate.max(1) as f64;
        let buf = self.buffer_frames() as f64 / rate;
        let out = match self.health.out_lat_us.load(Relaxed) {
            0 => buf,
            us => us as f64 * 1e-6,
        };
        let inp = match self.health.in_lat_us.load(Relaxed) {
            0 => buf,
            us => us as f64 * 1e-6,
        };
        out + inp + self.health.fifo.load(Relaxed) as f64 / rate
    }

    /// Output latency of one buffer, in milliseconds (what DAWs show for the buffer size: 128 frames at
    /// 44.1 kHz ≈ 2.9 ms).
    pub fn buffer_ms(&self) -> f64 {
        self.buffer_frames() as f64 * 1000.0 / self.rate.max(1) as f64
    }

    /// Stops the device in a fixed order and waits for the engine's threads: the input stream, then the
    /// output stream (its callback owns the renderer, the song and the plugins, which are freed here, on
    /// the calling thread), then the recorder (finishes its files) and the song collector. Called on
    /// close and by `Drop`; idempotent. Afterwards commands are ignored.
    ///
    /// Without this the streams and threads were torn down by process exit, racing the driver (ASIO)
    /// and plugin libraries still running on them – an intermittent crash when closing the app.
    pub fn shutdown(&mut self) {
        if let Some(s) = self.input.take() {
            let _ = s.pause();
            drop(s);
        }
        if let Some(s) = self.out.take() {
            let _ = s.pause();
            drop(s);
        }
        // Disconnect the channels so both threads leave their loops.
        self.tx = unbounded().0;
        self.rec_tx = unbounded().0;
        join_bounded(std::mem::take(&mut self.threads), Duration::from_secs(5));
    }

    pub fn set_song(&self, s: Arc<Song>) {
        self.send(Command::SetSong(s));
    }

    pub fn transport(&self) -> SharedTransport {
        *self.transport.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn is_recording(&self) -> bool {
        *self.recording.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Starts recording the given targets (armed tracks).
    pub fn start_recording(&self, targets: Vec<RecordTarget>, count_in_bars: u32) {
        let _ = self.rec_tx.send(RecMsg::Start { targets, rate: self.rate });
        self.send(Command::Record { count_in_bars });
    }

    /// Stops the transport and returns the recorded takes (blocks until the files are closed).
    pub fn stop_recording(&self) -> Vec<RecordedTake> {
        self.send(Command::Pause);
        // Give the audio thread one buffer to apply the pause before closing the files.
        std::thread::sleep(std::time::Duration::from_millis((self.buffer as u64 * 2000 / self.rate as u64).clamp(5, 200)));
        let (tx, rx) = crossbeam_channel::bounded(1);
        let latency = self.record_latency() + self.record_offset;
        let _ = self.rec_tx.send(RecMsg::Finish { latency, reply: tx });
        rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap_or_default()
    }
}

type InputBuf = Arc<Mutex<Vec<VecDeque<f32>>>>;

/// Opens `dev` for recording. Tries the engine's rate and buffer first, then the driver's buffer, then
/// the device's own rate (resampled to the engine's) – WASAPI shared mode and many interfaces refuse a
/// rate or a fixed buffer that differs from their own. Returns the stream and its channel count.
fn open_input(dev: &cpal::Device, rate: u32, buffer: cpal::BufferSize, buf: &InputBuf, health: Arc<StreamHealth>) -> std::result::Result<(cpal::Stream, u16), String> {
    let def = dev.default_input_config().map_err(|e| e.to_string())?;
    let ich = def.channels().max(1);
    let format = def.sample_format();
    let own = def.sample_rate().0;
    let mut tries = vec![(rate, buffer), (rate, cpal::BufferSize::Default)];
    if own != rate {
        tries.push((own, cpal::BufferSize::Default));
    }
    let mut last = String::from("no usable configuration");
    for (in_rate, bsize) in tries {
        let cfg = cpal::StreamConfig { channels: ich, sample_rate: cpal::SampleRate(in_rate), buffer_size: bsize };
        *buf.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = vec![VecDeque::with_capacity(rate as usize); ich as usize];
        macro_rules! build {
            ($t:ty, $conv:expr) => {{
                let ib = buf.clone();
                let mut rs = Resample::new(ich as usize, in_rate, rate);
                let h = health.clone();
                dev.build_input_stream(
                    &cfg,
                    move |d: &[$t], info: &cpal::InputCallbackInfo| {
                        let t = info.timestamp();
                        if let Some(l) = t.callback.duration_since(&t.capture) {
                            h.in_lat_us.store(l.as_micros().min(1_000_000) as u32, std::sync::atomic::Ordering::Relaxed);
                        }
                        push_input(&ib, d, ich as usize, $conv, &mut rs)
                    },
                    |e| tracing::warn!("input stream: {e}"),
                    None,
                )
            }};
        }
        let stream = match format {
            cpal::SampleFormat::F32 => build!(f32, |x: f32| x),
            cpal::SampleFormat::F64 => build!(f64, |x: f64| x as f32),
            cpal::SampleFormat::I16 => build!(i16, |x: i16| x as f32 / 32768.0),
            cpal::SampleFormat::I32 => build!(i32, |x: i32| x as f32 / 2_147_483_648.0),
            cpal::SampleFormat::I8 => build!(i8, |x: i8| x as f32 / 128.0),
            cpal::SampleFormat::U16 => build!(u16, |x: u16| (x as f32 - 32768.0) / 32768.0),
            cpal::SampleFormat::U8 => build!(u8, |x: u8| (x as f32 - 128.0) / 128.0),
            f => return Err(format!("sample format {f:?} is not supported")),
        };
        match stream {
            Ok(s) => match s.play() {
                Ok(()) => return Ok((s, ich)),
                Err(e) => last = e.to_string(),
            },
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// Linear resampling of the input to the engine's rate (when the device runs at its own rate).
struct Resample {
    step: f64,
    pos: f64,
    last: Vec<f32>,
    frame: Vec<f32>,
}

impl Resample {
    fn new(ch: usize, from: u32, to: u32) -> Self {
        Resample { step: from as f64 / to as f64, pos: 0.0, last: vec![0.0; ch], frame: vec![0.0; ch] }
    }
}

fn push_input<T: Copy>(buf: &InputBuf, data: &[T], ch: usize, conv: impl Fn(T) -> f32, rs: &mut Resample) {
    if let Ok(mut q) = buf.lock() {
        if q.len() != ch {
            q.resize(ch, VecDeque::new());
        }
        let same = (rs.step - 1.0).abs() < 1e-9;
        for frame in data.chunks_exact(ch) {
            if same {
                for (k, s) in frame.iter().enumerate() {
                    q[k].push_back(conv(*s));
                }
                continue;
            }
            for (k, s) in frame.iter().enumerate() {
                rs.frame[k] = conv(*s);
            }
            // Output samples that fall between the previous frame and this one.
            while rs.pos < 1.0 {
                let f = rs.pos as f32;
                for k in 0..ch {
                    q[k].push_back(rs.last[k] + (rs.frame[k] - rs.last[k]) * f);
                }
                rs.pos += rs.step;
            }
            rs.pos -= 1.0;
            rs.last.copy_from_slice(&rs.frame);
        }
        // Never grow without bound when nobody consumes (output stopped).
        for c in q.iter_mut() {
            let len = c.len();
            if len > 96_000 {
                c.drain(..len - 48_000);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------- MIDI

/// A MIDI message with the transport beat it arrived at.
#[derive(Debug, Clone, Copy)]
pub struct TimedMidi {
    pub message: crate::midi::MidiMessage,
    pub beat: f64,
    pub seconds: f64,
    pub port: usize,
}

pub fn midi_outputs() -> Vec<String> {
    let Ok(m) = midir::MidiOutput::new("Dragon DAW") else { return Vec::new() };
    m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect()
}

/// An open MIDI output (control-surface feedback: motor faders, LEDs).
pub struct MidiOutput {
    conn: midir::MidiOutputConnection,
    pub name: String,
}

impl MidiOutput {
    pub fn open(name: &str) -> Result<MidiOutput> {
        let out = midir::MidiOutput::new("Dragon DAW").map_err(|e| EngineError::Midi(e.to_string()))?;
        let port = out.ports().into_iter().find(|p| out.port_name(p).ok().as_deref() == Some(name)).ok_or_else(|| EngineError::Midi(format!("no MIDI output named {name}")))?;
        let conn = out.connect(&port, "dragon-out").map_err(|e| EngineError::Midi(e.to_string()))?;
        Ok(MidiOutput { conn, name: name.to_string() })
    }

    pub fn send(&mut self, bytes: &[u8]) {
        let _ = self.conn.send(bytes);
    }
}

pub fn midi_inputs() -> Vec<String> {
    let Ok(m) = midir::MidiInput::new("Dragon DAW") else { return Vec::new() };
    m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect()
}

/// Open MIDI input connections; messages arrive on `rx`.
pub struct MidiInputs {
    _conns: Vec<midir::MidiInputConnection<()>>,
    pub rx: Receiver<TimedMidi>,
    pub names: Vec<String>,
    /// Every port present when opening; [`TimedMidi::port`] indexes this list.
    pub all_ports: Vec<String>,
}

impl MidiInputs {
    /// Opens the named ports (all ports when `names` is empty). `wake` is called after every message (the
    /// UI redraws then, so a key pressed on the controller lights up at once).
    pub fn open(names: &[String], transport: Option<Arc<Mutex<SharedTransport>>>, wake: Option<Arc<dyn Fn() + Send + Sync>>) -> MidiInputs {
        let (tx, rx) = unbounded();
        let mut conns = Vec::new();
        let mut opened = Vec::new();
        let start = Instant::now();
        let probe = midir::MidiInput::new("Dragon DAW probe");
        let ports: Vec<String> = probe.as_ref().map(|m| m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect()).unwrap_or_default();
        for (idx, name) in ports.iter().enumerate() {
            if !names.is_empty() && !names.contains(name) {
                continue;
            }
            let Ok(mut input) = midir::MidiInput::new(&format!("Dragon DAW in {idx}")) else { continue };
            input.ignore(midir::Ignore::None);
            let Some(port) = input.ports().into_iter().find(|p| input.port_name(p).ok().as_deref() == Some(name.as_str())) else { continue };
            let tx = tx.clone();
            let tr = transport.clone();
            let wake = wake.clone();
            let conn = input.connect(
                &port,
                "dragon-daw",
                move |_, bytes, _| {
                    let beat = tr.as_ref().and_then(|t| t.lock().ok().map(|s| s.beat_now())).unwrap_or(0.0);
                    let _ = tx.send(TimedMidi { message: crate::midi::parse(bytes), beat, seconds: start.elapsed().as_secs_f64(), port: idx });
                    if let Some(w) = &wake {
                        w();
                    }
                },
                (),
            );
            if let Ok(c) = conn {
                conns.push(c);
                opened.push(name.clone());
            }
        }
        MidiInputs { _conns: conns, rx, names: opened, all_ports: ports }
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;

    /// An input running at its own rate is resampled to the engine's: the right number of samples, and the
    /// tone keeps its pitch.
    #[test]
    fn input_at_another_rate_is_resampled_to_the_engines() {
        let buf: InputBuf = Arc::new(Mutex::new(vec![VecDeque::new(); 2]));
        let mut rs = Resample::new(2, 48_000, 44_100);
        // One second of a 1 kHz tone at 48 kHz, stereo interleaved, in 480-frame callbacks.
        let x: Vec<f32> = (0..48_000)
            .flat_map(|i| {
                let v = (std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin();
                [v, v]
            })
            .collect();
        for chunk in x.chunks(960) {
            push_input(&buf, chunk, 2, |v: f32| v, &mut rs);
        }
        let q = buf.lock().unwrap();
        let n = q[0].len();
        assert!((n as i64 - 44_100).abs() <= 2, "{n} samples");
        // Zero crossings of 1 kHz over one second: ~2000.
        let y: Vec<f32> = q[0].iter().copied().collect();
        let zc = y.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        assert!((zc as i64 - 1000).abs() <= 2, "{zc} rising crossings");
        // Smooth: no jumps bigger than a 1 kHz sine's step at 44.1 kHz.
        let max_step = y.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        assert!(max_step < 0.16, "{max_step}");
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    /// Finished threads are joined; one that hangs is given up on after the limit (closing never hangs).
    #[test]
    fn join_bounded_joins_and_gives_up_in_time() {
        let done = std::thread::spawn(|| std::thread::sleep(Duration::from_millis(20)));
        let (_keep, rx) = unbounded::<()>();
        let stuck = std::thread::spawn(move || {
            let _ = rx.recv();
        });
        let t0 = Instant::now();
        join_bounded(vec![done, stuck], Duration::from_millis(300));
        let took = t0.elapsed();
        assert!(took >= Duration::from_millis(250) && took < Duration::from_secs(3), "{took:?}");
    }
}
