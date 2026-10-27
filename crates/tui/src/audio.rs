//! Built-in audio. Devices live on a dedicated thread; decoding is streamed into a
//! short PCM queue. Neither the UI nor an audio callback waits for disk/network I/O.
use pulseaudio::protocol::{self, ChannelMap, SampleFormat, SampleSpec};
use pulseaudio::{Client, PlaybackStream, RecordStream};
use std::{
    collections::VecDeque,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{CODEC_TYPE_NULL, Decoder as Codec, DecoderOptions},
    formats::{FormatOptions, FormatReader},
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};

const RATE: u32 = 48_000;
const RECORD_LIMIT: usize = RATE as usize * 600;

pub fn supported(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()).is_some_and(|e| {
        [
            "ogg", "opus", "mp3", "m4a", "aac", "flac", "wav", "wave", "aif", "aiff", "alac", "mka",
        ]
        .contains(&e.to_ascii_lowercase().as_str())
    })
}
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub seconds: f64,
    pub peaks: Vec<u8>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playback {
    pub path: PathBuf,
    pub seconds: f64,
    pub playing: bool,
    pub stopped: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Toggle(PathBuf),
    Stop,
    Seek(PathBuf, f64),
    Record(PathBuf),
    Finish,
    Cancel(PathBuf),
    Quit,
}
#[derive(Clone, Debug)]
pub enum Event {
    Playback(Playback),
    Recorded {
        path: PathBuf,
        seconds: f64,
        level: u8,
        done: bool,
    },
    Error(String),
}
#[derive(Clone)]
pub struct Engine(mpsc::Sender<Command>);
impl Engine {
    pub fn new(report: impl Fn(Event) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("omtalk-audio".into())
            .spawn(move || worker(rx, report))
            .expect("audio worker");
        Self(tx)
    }
    pub fn send(&self, command: Command) {
        let _ = self.0.send(command);
    }
}

struct Reader {
    source: Source,
    pub rate: u32,
    pub channels: usize,
}
enum Source {
    Standard {
        format: Box<dyn FormatReader>,
        decoder: Box<dyn Codec>,
        track: u32,
    },
    Opus {
        packets: ogg::PacketReader<File>,
        decoder: opus::Decoder,
        skip: usize,
        gain: f32,
        end: Option<u64>,
        decoded: u64,
    },
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
impl Reader {
    fn open(path: &Path) -> Result<Self, String> {
        // Opus-in-Ogg is decoded with the bundled libopus; Symphonia covers other
        // common formats. The content, rather than the extension, selects Opus.
        let mut packets = ogg::PacketReader::new(File::open(path).map_err(err)?);
        if let Ok(Some(head)) = packets.read_packet()
            && head.data.starts_with(b"OpusHead")
        {
            if head.data.len() < 19 || head.data[8] > 15 || head.data[18] != 0 {
                return Err("unsupported Opus channel mapping".into());
            }
            let channels = head.data[9] as usize;
            let mode = match channels {
                1 => opus::Channels::Mono,
                2 => opus::Channels::Stereo,
                _ => return Err("unsupported Opus channel count".into()),
            };
            let skip = u16::from_le_bytes([head.data[10], head.data[11]]) as usize;
            let gain = 10f32.powf(i16::from_le_bytes([head.data[16], head.data[17]]) as f32 / (256.0 * 20.0));
            let tags = packets.read_packet().map_err(err)?.ok_or("missing Opus tags")?;
            if !tags.data.starts_with(b"OpusTags") {
                return Err("missing Opus tags".into());
            }
            return Ok(Self {
                source: Source::Opus {
                    packets,
                    decoder: opus::Decoder::new(RATE, mode).map_err(err)?,
                    skip,
                    gain,
                    end: None,
                    decoded: 0,
                },
                rate: RATE,
                channels,
            });
        }
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }
        let stream = MediaSourceStream::new(Box::new(File::open(path).map_err(err)?), Default::default());
        let format = symphonia::default::get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(err)?
            .format;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or("no audio track")?;
        let rate = track
            .codec_params
            .sample_rate
            .ok_or("unknown audio sample rate")?;
        let channels = track
            .codec_params
            .channels
            .ok_or("unknown audio channels")?
            .count();
        if rate == 0 || rate > 384_000 || channels == 0 || channels > 2 {
            return Err("unsupported audio layout".into());
        }
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(err)?;
        let id = track.id;
        Ok(Self {
            source: Source::Standard {
                format,
                decoder,
                track: id,
            },
            rate,
            channels,
        })
    }
    fn next(&mut self) -> Result<Option<Vec<f32>>, String> {
        match &mut self.source {
            Source::Opus {
                packets,
                decoder,
                skip,
                gain,
                end,
                decoded,
            } => {
                let Some(packet) = packets.read_packet().map_err(err)? else {
                    return Ok(None);
                };
                if packet.last_in_stream() {
                    *end = Some(packet.absgp_page());
                }
                let mut pcm = vec![0f32; 5760 * self.channels];
                let frames = decoder.decode_float(&packet.data, &mut pcm, false).map_err(err)?;
                let trim_end = end.map_or(frames, |n| n.saturating_sub(*decoded).min(frames as u64) as usize);
                *decoded += frames as u64;
                let trim_start = (*skip).min(trim_end);
                *skip = skip.saturating_sub(frames);
                pcm.truncate(trim_end * self.channels);
                pcm.drain(..trim_start * self.channels);
                for sample in &mut pcm {
                    *sample = (*sample * *gain).clamp(-1.0, 1.0);
                }
                Ok(Some(pcm))
            }
            Source::Standard {
                format,
                decoder,
                track,
            } => loop {
                let packet = match format.next_packet() {
                    Ok(p) => p,
                    Err(symphonia::core::errors::Error::IoError(e))
                        if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        return Ok(None);
                    }
                    Err(e) => return Err(err(e)),
                };
                if packet.track_id() != *track {
                    continue;
                }
                let decoded = decoder.decode(&packet).map_err(err)?;
                if decoded.spec().rate != self.rate || decoded.spec().channels.count() != self.channels {
                    return Err("audio layout changes within the file".into());
                }
                let mut samples = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
                samples.copy_interleaved_ref(decoded);
                return Ok(Some(
                    samples
                        .samples()
                        .iter()
                        .map(|v| if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 })
                        .collect(),
                ));
            },
        }
    }
}

/// A bounded, streaming peak envelope. Compacting pairs retains the whole file's
/// shape without keeping decoded PCM in memory.
#[derive(Default)]
struct Envelope {
    peaks: Vec<f32>,
    stride: usize,
    count: usize,
    peak: f32,
}
impl Envelope {
    fn push(&mut self, sample: f32) {
        self.peak = self.peak.max(sample.abs());
        self.count += 1;
        if self.count >= self.stride.max(256) {
            self.peaks.push(self.peak);
            self.peak = 0.0;
            self.count = 0;
            if self.peaks.len() == 1024 {
                self.peaks = self.peaks.chunks_exact(2).map(|v| v[0].max(v[1])).collect();
                self.stride = self.stride.max(256) * 2;
            }
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.peaks.push(self.peak);
        }
        let max = self.peaks.iter().copied().fold(0f32, f32::max).max(0.01);
        self.peaks
            .iter()
            .map(|p| ((p / max).sqrt() * 7.0).round() as u8)
            .collect()
    }
}
pub fn inspect(path: &Path) -> Result<Info, String> {
    let mut reader = Reader::open(path)?;
    let mut frames = 0u64;
    let mut envelope = Envelope::default();
    while let Some(pcm) = reader.next()? {
        frames += (pcm.len() / reader.channels) as u64;
        if frames > reader.rate as u64 * 6 * 3600 {
            return Err("audio exceeds six hours".into());
        }
        for frame in pcm.chunks_exact(reader.channels) {
            envelope.push(frame.iter().copied().map(f32::abs).fold(0.0, f32::max));
        }
    }
    if frames == 0 {
        return Err("empty audio file".into());
    }
    Ok(Info {
        seconds: frames as f64 / reader.rate as f64,
        peaks: envelope.finish(),
    })
}
pub fn clock(seconds: f64) -> String {
    let n = seconds.max(0.0) as u64;
    format!("{}:{:02}", n / 60, n % 60)
}

#[derive(Default)]
struct Buffer {
    samples: VecDeque<f32>,
    played: u64,
    paused: bool,
    ended: bool,
    waker: Option<std::task::Waker>,
}
struct Playing {
    path: PathBuf,
    reader: Reader,
    buffer: Arc<Mutex<Buffer>>,
    device: PlaybackStream,
    _client: Client,
    skip: u64,
    base: f64,
    checked: Instant,
}
impl Playing {
    fn new(path: PathBuf, seconds: f64) -> Result<Self, String> {
        let reader = Reader::open(&path)?;
        let buffer = Arc::new(Mutex::new(Buffer::default()));
        let client = connect_audio()?;
        let spec = SampleSpec {
            format: SampleFormat::Float32Le,
            channels: reader.channels as u8,
            sample_rate: reader.rate,
        };
        let mut params = protocol::PlaybackStreamParams {
            sample_spec: spec,
            sink_name: std::env::var("PULSE_SINK")
                .ok()
                .and_then(|s| std::ffi::CString::new(s).ok()),
            channel_map: channel_map(reader.channels),
            ..Default::default()
        };
        params.buffer_attr.target_length = reader.rate * reader.channels as u32 * 4 / 10;
        params.buffer_attr.pre_buffering = 0;
        let device = wait(client.create_playback_stream(params, AudioSource(buffer.clone())))?;
        let skip = (seconds.max(0.0) * reader.rate as f64) as u64;
        Ok(Self {
            path,
            reader,
            buffer,
            device,
            _client: client,
            skip,
            base: seconds.max(0.0),
            checked: Instant::now(),
        })
    }
    fn fill(&mut self) -> Result<(), String> {
        if self.checked.elapsed() >= Duration::from_secs(2) {
            wait(self.device.timing_info())?;
            self.checked = Instant::now();
        }
        {
            let b = self.buffer.lock().unwrap();
            if b.ended || b.samples.len() >= self.reader.rate as usize * self.reader.channels / 2 {
                return Ok(());
            }
        }
        match self.reader.next()? {
            None => {
                let mut b = self.buffer.lock().unwrap();
                b.ended = true;
                if let Some(w) = b.waker.take() {
                    w.wake();
                }
            }
            Some(mut pcm) => {
                let skip = self.skip.min((pcm.len() / self.reader.channels) as u64) as usize;
                self.skip -= skip as u64;
                pcm.drain(..skip * self.reader.channels);
                let mut buffer = self.buffer.lock().unwrap();
                buffer.samples.extend(pcm);
                if let Some(waker) = buffer.waker.take() {
                    waker.wake();
                }
            }
        }
        Ok(())
    }
    fn paused(&self) -> bool {
        self.buffer.lock().unwrap().paused
    }
    fn snapshot(&self) -> Playback {
        let b = self.buffer.lock().unwrap();
        Playback {
            path: self.path.clone(),
            seconds: self.base + b.played as f64 / (self.reader.rate as f64 * self.reader.channels as f64),
            playing: !b.paused && !(b.ended && b.samples.is_empty()),
            stopped: b.ended && b.samples.is_empty(),
        }
    }
}
fn connect_audio() -> Result<Client, String> {
    let path = pulseaudio::socket_path_from_env().ok_or("no PulseAudio/PipeWire sound server")?;
    let socket = std::os::unix::net::UnixStream::connect(path).map_err(err)?;
    socket
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(err)?;
    socket
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(err)?;
    let cookie = pulseaudio::cookie_path_from_env().and_then(|p| std::fs::read(p).ok());
    Client::new_unix(c"omtalk", socket, cookie).map_err(err)
}
fn wait<T>(future: impl std::future::Future<Output = pulseaudio::Result<T>>) -> Result<T, String> {
    // This runtime is confined to the audio worker; the protocol has its own reactor.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(err)?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(3), future)
            .await
            .map_err(|_| "sound server timed out".to_owned())?
            .map_err(err)
    })
}
fn channel_map(channels: usize) -> ChannelMap {
    if channels == 1 {
        ChannelMap::mono()
    } else {
        ChannelMap::stereo()
    }
}
struct AudioSource(Arc<Mutex<Buffer>>);
impl pulseaudio::PlaybackSource for AudioSource {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        out: &mut [u8],
    ) -> std::task::Poll<usize> {
        let mut state = self.0.lock().unwrap();
        if state.paused || (state.samples.is_empty() && !state.ended) {
            state.waker = Some(cx.waker().clone());
            return std::task::Poll::Pending;
        }
        let count = (out.len() / 4).min(state.samples.len());
        for bytes in out[..count * 4].chunks_exact_mut(4) {
            bytes.copy_from_slice(&state.samples.pop_front().unwrap().to_le_bytes());
        }
        state.played += count as u64;
        std::task::Poll::Ready(count * 4)
    }
}

struct Recording {
    path: PathBuf,
    device: RecordStream,
    _client: Client,
    chunks: mpsc::Receiver<Vec<f32>>,
    overflow: Arc<std::sync::atomic::AtomicBool>,
    writer: VoiceWriter,
    level: u8,
    heard: Instant,
    count: usize,
}
impl Recording {
    fn new(path: PathBuf) -> Result<Self, String> {
        let writer = VoiceWriter::new(&path)?;
        let (tx, chunks) = mpsc::sync_channel(128);
        let overflow = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = overflow.clone();
        let client = connect_audio()?;
        let spec = SampleSpec {
            format: SampleFormat::Float32Le,
            channels: 1,
            sample_rate: RATE,
        };
        let mut params = protocol::RecordStreamParams {
            sample_spec: spec,
            source_name: std::env::var("PULSE_SOURCE")
                .ok()
                .and_then(|s| std::ffi::CString::new(s).ok()),
            channel_map: ChannelMap::mono(),
            ..Default::default()
        };
        params.buffer_attr.fragment_size = RATE * 4 / 50;
        let device = wait(client.create_record_stream(params, move |input: &[u8]| {
            let samples = input
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .map(|v| if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 })
                .collect();
            if tx.try_send(samples).is_err() {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }))?;
        Ok(Self {
            path,
            device,
            _client: client,
            chunks,
            overflow,
            writer,
            level: 0,
            heard: Instant::now(),
            count: 0,
        })
    }
    fn drain(&mut self) -> Result<(), String> {
        if self.writer.samples != self.count {
            self.count = self.writer.samples;
            self.heard = Instant::now();
        }
        if self.heard.elapsed() > Duration::from_secs(3) {
            return Err("the microphone stopped returning audio".into());
        }
        if self.overflow.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("microphone buffer overflow; recording cancelled".into());
        }
        self.level = 0;
        while let Ok(chunk) = self.chunks.try_recv() {
            self.level = self
                .level
                .max((chunk.iter().copied().map(f32::abs).fold(0.0, f32::max) * 8.0).min(8.0) as u8);
            self.writer.push(&chunk)?;
        }
        Ok(())
    }
    fn seconds(&self) -> f64 {
        self.writer.samples as f64 / RATE as f64
    }
}
/// Ogg Opus, mono 48 kHz, 32 kbit/s. The encoder's lookahead is included in the
/// header and end granule so other XMPP clients trim the recording correctly.
struct VoiceWriter {
    file: ogg::PacketWriter<'static, File>,
    encoder: opus::Encoder,
    pending: Vec<f32>,
    samples: usize,
    encoded: u64,
    preskip: u16,
}
impl VoiceWriter {
    fn new(path: &Path) -> Result<Self, String> {
        let mut encoder =
            opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).map_err(err)?;
        encoder.set_bitrate(opus::Bitrate::Bits(32_000)).map_err(err)?;
        let preskip = encoder.get_lookahead().map_err(err)? as u16;
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(err)?;
        let mut file = ogg::PacketWriter::new(file);
        let mut head = b"OpusHead".to_vec();
        head.extend([1, 1]);
        head.extend(preskip.to_le_bytes());
        head.extend(RATE.to_le_bytes());
        head.extend([0, 0, 0]);
        file.write_packet(head, 1, ogg::PacketWriteEndInfo::EndPage, 0)
            .map_err(err)?;
        let vendor = b"omtalk";
        let mut tags = b"OpusTags".to_vec();
        tags.extend((vendor.len() as u32).to_le_bytes());
        tags.extend(vendor);
        tags.extend(0u32.to_le_bytes());
        file.write_packet(tags, 1, ogg::PacketWriteEndInfo::EndPage, 0)
            .map_err(err)?;
        Ok(Self {
            file,
            encoder,
            pending: Vec::new(),
            samples: 0,
            encoded: 0,
            preskip,
        })
    }
    fn push(&mut self, samples: &[f32]) -> Result<(), String> {
        if self.samples + samples.len() > RECORD_LIMIT {
            return Err("recording limit: 10 minutes".into());
        }
        self.samples += samples.len();
        self.pending.extend_from_slice(samples);
        while self.pending.len() >= 960 {
            let data = self
                .encoder
                .encode_vec_float(&self.pending[..960], 4000)
                .map_err(err)?;
            self.pending.drain(..960);
            self.encoded += 960;
            self.file
                .write_packet(data, 1, ogg::PacketWriteEndInfo::EndPage, self.encoded)
                .map_err(err)?;
        }
        Ok(())
    }
    fn finish(&mut self) -> Result<(), String> {
        if self.samples == 0 {
            return Err("the microphone returned no audio".into());
        }
        let target = self.samples as u64 + self.preskip as u64;
        while self.encoded < target {
            self.pending.resize(960, 0.0);
            let data = self.encoder.encode_vec_float(&self.pending, 4000).map_err(err)?;
            self.pending.clear();
            self.encoded += 960;
            let end = if self.encoded >= target {
                ogg::PacketWriteEndInfo::EndStream
            } else {
                ogg::PacketWriteEndInfo::EndPage
            };
            self.file
                .write_packet(data, 1, end, self.encoded.min(target))
                .map_err(err)?;
        }
        Ok(())
    }
}

fn worker(rx: mpsc::Receiver<Command>, report: impl Fn(Event)) {
    let mut playing: Option<Playing> = None;
    let mut recording: Option<Recording> = None;
    let mut last = Instant::now();
    let mut shown: Option<Playback> = None;
    loop {
        // Nothing to feed or drain (no sound, or it is paused): the state is told, a
        // finished stream let go, and the worker sleeps until the next command, so an
        // idle omtalk does not wake hundreds of times a second.
        let idle = |playing: &Option<Playing>, recording: &Option<Recording>| {
            recording.is_none() && playing.as_ref().is_none_or(Playing::paused)
        };
        if idle(&playing, &recording) || last.elapsed() >= Duration::from_millis(100) {
            last = Instant::now();
            if let Some(p) = &playing {
                let snapshot = p.snapshot();
                if shown.as_ref() != Some(&snapshot) {
                    report(Event::Playback(snapshot.clone()));
                    shown = Some(snapshot.clone());
                }
                if snapshot.stopped {
                    playing = None;
                }
            }
            if let Some(r) = &recording {
                report(Event::Recorded {
                    path: r.path.clone(),
                    seconds: r.seconds(),
                    level: r.level,
                    done: false,
                });
            }
        }
        let command = if idle(&playing, &recording) {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            }
        } else {
            match rx.recv_timeout(Duration::from_millis(2)) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => break,
            }
        };
        let result: Result<(), String> = (|| {
            match command {
                Some(Command::Quit) => return Err("__quit".into()),
                Some(Command::Stop) => {
                    if let Some(p) = playing.take() {
                        report(Event::Playback(Playback {
                            path: p.path.clone(),
                            stopped: true,
                            ..Default::default()
                        }));
                    }
                }
                Some(Command::Toggle(path))
                    if playing
                        .as_ref()
                        .is_some_and(|p| p.path == path && !p.snapshot().stopped) =>
                {
                    let p = playing.as_ref().unwrap();
                    let mut b = p.buffer.lock().unwrap();
                    b.paused = !b.paused;
                    let paused = b.paused;
                    if let Some(w) = b.waker.take() {
                        w.wake();
                    }
                    drop(b);
                    if paused {
                        wait(p.device.cork())?;
                    } else {
                        wait(p.device.uncork())?;
                    }
                }
                Some(Command::Toggle(path)) => {
                    playing = None;
                    playing = Some(Playing::new(path, 0.0)?);
                }
                Some(Command::Seek(path, seconds)) => {
                    playing = None;
                    playing = Some(Playing::new(path, seconds)?);
                }
                Some(Command::Record(path)) => {
                    playing = None;
                    if recording.is_some() {
                        return Err("already recording".into());
                    }
                    match Recording::new(path.clone()) {
                        Ok(r) => recording = Some(r),
                        Err(e) => {
                            let _ = std::fs::remove_file(path);
                            return Err(e);
                        }
                    }
                }
                Some(Command::Finish) => {
                    if let Some(mut r) = recording.take() {
                        let result = wait(r.device.cork())
                            .and_then(|_| r.drain())
                            .and_then(|_| r.writer.finish());
                        if let Err(e) = result {
                            let _ = std::fs::remove_file(&r.path);
                            return Err(e);
                        }
                        report(Event::Recorded {
                            path: r.path.clone(),
                            seconds: r.seconds(),
                            level: 0,
                            done: true,
                        });
                    }
                }
                Some(Command::Cancel(path)) => {
                    playing = None;
                    if let Some(r) = recording.take() {
                        let old = r.path.clone();
                        drop(r);
                        let _ = std::fs::remove_file(old);
                    }
                    let _ = std::fs::remove_file(path);
                }
                None => {}
            }
            if let Some(p) = playing.as_mut() {
                for _ in 0..32 {
                    p.fill()?;
                }
            }
            if let Some(r) = recording.as_mut() {
                r.drain()?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            if e == "__quit" {
                break;
            }
            if let Some(p) = playing.take() {
                report(Event::Playback(Playback {
                    path: p.path.clone(),
                    stopped: true,
                    ..Default::default()
                }));
            }
            if let Some(r) = recording.take() {
                let path = r.path.clone();
                drop(r);
                let _ = std::fs::remove_file(path);
            }
            report(Event::Error(e));
        }
    }
    drop(playing);
    if let Some(r) = recording {
        let path = r.path.clone();
        drop(r);
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opus_round_trip_keeps_duration_and_real_waveform_without_devices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voice.ogg");
        let mut writer = VoiceWriter::new(&path).unwrap();
        let samples: Vec<_> = (0..48000)
            .map(|n| {
                if n < 24000 {
                    0.0
                } else {
                    (n as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.5
                }
            })
            .collect();
        for chunk in samples.chunks(333) {
            writer.push(chunk).unwrap();
        }
        writer.finish().unwrap();
        drop(writer);
        let info = inspect(&path).unwrap();
        assert!((info.seconds - 1.0).abs() < 0.001, "{}", info.seconds);
        assert!(info.peaks.iter().take(info.peaks.len() / 3).all(|v| *v <= 1));
        assert!(info.peaks.iter().any(|v| *v >= 6));
    }
    #[test]
    fn waveform_memory_is_bounded_and_silent_audio_is_finite() {
        let mut e = Envelope::default();
        for _ in 0..2_000_000 {
            e.push(0.0);
        }
        let peaks = e.finish();
        assert!(peaks.len() <= 1024);
        assert!(peaks.iter().all(|v| *v == 0));
    }
}

#[cfg(test)]
mod device_tests {
    use super::*;
    /// Run explicitly with an isolated PulseAudio/PipeWire null sink and its
    /// monitor as PULSE_SINK/PULSE_SOURCE; never records the user's microphone.
    #[test]
    #[ignore = "requires a dedicated virtual sound device"]
    fn virtual_device_play_pause_seek_stop_and_record() {
        assert!(
            std::env::var("PULSE_SOURCE")
                .unwrap()
                .starts_with("omtalk_audio_test")
        );
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.ogg");
        let recording = dir.path().join("recorded.ogg");
        let mut writer = VoiceWriter::new(&source).unwrap();
        let wave: Vec<_> = (0..RATE * 3)
            .map(|n| (n as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.1)
            .collect();
        writer.push(&wave).unwrap();
        writer.finish().unwrap();
        drop(writer);
        let (tx, rx) = mpsc::channel();
        let engine = Engine::new(move |e| {
            let _ = tx.send(e);
        });
        let until = |accept: &dyn Fn(&Event) -> bool| {
            let start = Instant::now();
            loop {
                let e = rx.recv_timeout(Duration::from_secs(5)).unwrap();
                if let Event::Error(error) = &e {
                    panic!("audio backend: {error}");
                }
                if accept(&e) {
                    return e;
                }
                assert!(start.elapsed() < Duration::from_secs(8), "audio event deadline");
            }
        };
        engine.send(Command::Record(recording.clone()));
        until(&|e| matches!(e,Event::Recorded{seconds,done:false,..}if *seconds>0.05));
        engine.send(Command::Toggle(source.clone()));
        until(&|e| matches!(e,Event::Playback(p)if p.playing&&p.seconds>0.3));
        engine.send(Command::Toggle(source.clone()));
        until(&|e| matches!(e,Event::Playback(p)if !p.playing&&!p.stopped));
        engine.send(Command::Seek(source.clone(), 1.5));
        until(&|e| matches!(e,Event::Playback(p)if p.playing&&p.seconds>=1.5));
        engine.send(Command::Stop);
        until(&|e| matches!(e,Event::Playback(p)if p.stopped&&p.seconds==0.0));
        engine.send(Command::Finish);
        until(&|e| matches!(e, Event::Recorded { done: true, .. }));
        engine.send(Command::Quit);
        let info = inspect(&recording).unwrap();
        assert!(info.seconds > 0.3);
        assert!(
            info.peaks.iter().any(|p| *p > 2),
            "virtual monitor must capture the played sine"
        );
    }
}
