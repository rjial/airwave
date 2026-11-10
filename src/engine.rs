//! Audio engine: sends this computer's sound to listeners on the network
//! ("server") and plays sound received from another computer ("player").
//!
//! Wire format (our own, simple and open): see codec.rs for the packets
//! used on Wi-Fi and USB alike.
//!   Discovery: UDP probe "AIRWAVE?" to port DISCOVERY_PORT,
//!   servers answer "AIRWAVE!<tcp port>|<name>".

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use crate::codec::{Codec, Encoder, PacketReader};

pub const STREAM_PORT: u16 = 59200;
pub const DISCOVERY_PORT: u16 = 59201;
const PROBE: &[u8] = b"AIRWAVE?";
const REPLY_PREFIX: &str = "AIRWAVE!";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Quality {
    Stereo,
    Mono,
}

#[derive(Clone, PartialEq, Debug)]
pub enum ServerState {
    Stopped,
    Starting,
    Live,
    Error(String),
}

#[derive(Clone, PartialEq, Debug)]
pub enum PlayerState {
    Idle,
    Connecting(String),
    Playing(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub struct Found {
    pub name: String,
    pub addr: SocketAddr,
    pub last_seen: Instant,
}

#[derive(Clone, Debug)]
pub struct ListenerInfo {
    pub label: String,
    pub since: Instant,
}

/// Another way out for the sender's audio (for example a USB phone).
/// It receives ready-made packets, each at most 16 KB.
pub struct Tap {
    pub name: String,
    pub tx: SyncSender<Vec<u8>>,
    pub since: Instant,
}

#[derive(Default)]
pub struct Devices {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub default_input: Option<String>,
    pub default_output: Option<String>,
    pub loaded: bool,
}

pub struct Status {
    pub server: ServerState,
    pub server_format: String,
    pub listeners: Vec<ListenerInfo>,
    pub player: PlayerState,
    pub player_format: String,
    pub found: Vec<Found>,
    pub devices: Devices,
    pub discovery_note: Option<String>,
}

fn f32_load(a: &AtomicU32) -> f32 {
    f32::from_bits(a.load(Ordering::Relaxed))
}
fn f32_store(a: &AtomicU32, v: f32) {
    a.store(v.to_bits(), Ordering::Relaxed)
}

/// Shared numbers the audio threads update many times a second.
pub struct Meters {
    pub server_level: AtomicU32,
    pub player_level: AtomicU32,
    pub player_buffer_ms: AtomicU32,
    pub volume: AtomicU32,
    pub muted: AtomicBool,
    pub latency_ms: AtomicU32,
    pub bytes_sent: AtomicU32,
}

impl Meters {
    pub fn server_level(&self) -> f32 {
        f32_load(&self.server_level)
    }
    pub fn player_level(&self) -> f32 {
        f32_load(&self.player_level)
    }
    pub fn set_volume(&self, v: f32) {
        f32_store(&self.volume, v.clamp(0.0, 1.0))
    }
}

pub struct Engine {
    pub status: Arc<Mutex<Status>>,
    pub meters: Arc<Meters>,
    pub server_name: String,
    server_stop: Option<Arc<AtomicBool>>,
    player_stop: Option<Arc<AtomicBool>>,
    pub scan_now: Arc<AtomicBool>,
    pub auto_scan: Arc<AtomicBool>,
    pub taps: Arc<Mutex<Vec<Tap>>>,
}

impl Engine {
    pub fn new(volume: f32, buffer_ms: u32, auto_scan: bool) -> Self {
        let status = Arc::new(Mutex::new(Status {
            server: ServerState::Stopped,
            server_format: String::new(),
            listeners: Vec::new(),
            player: PlayerState::Idle,
            player_format: String::new(),
            found: Vec::new(),
            devices: Devices::default(),
            discovery_note: None,
        }));
        let meters = Arc::new(Meters {
            server_level: AtomicU32::new(0),
            player_level: AtomicU32::new(0),
            player_buffer_ms: AtomicU32::new(0),
            volume: AtomicU32::new(volume.to_bits()),
            muted: AtomicBool::new(false),
            latency_ms: AtomicU32::new(buffer_ms),
            bytes_sent: AtomicU32::new(0),
        });
        let engine = Engine {
            status,
            meters,
            server_name: computer_name(),
            server_stop: None,
            player_stop: None,
            scan_now: Arc::new(AtomicBool::new(true)),
            auto_scan: Arc::new(AtomicBool::new(auto_scan)),
            taps: Arc::new(Mutex::new(Vec::new())),
        };
        engine.refresh_devices();
        engine.start_scanner();
        engine
    }

    pub fn refresh_devices(&self) {
        let status = self.status.clone();
        thread::spawn(move || {
            let devices = list_devices();
            status.lock().unwrap().devices = devices;
        });
    }

    // ---------------------------------------------------------------- server

    pub fn start_server(&mut self, device: Option<String>, quality: Quality, codec: Codec, port: u16) {
        self.stop_server();
        let stop = Arc::new(AtomicBool::new(false));
        self.server_stop = Some(stop.clone());
        {
            let mut st = self.status.lock().unwrap();
            st.server = ServerState::Starting;
            st.listeners.clear();
        }
        let status = self.status.clone();
        let meters = self.meters.clone();
        let name = self.server_name.clone();
        let taps = self.taps.clone();
        thread::Builder::new()
            .name("airwave-server".into())
            .spawn(move || {
                let res = run_server(device, quality, codec, port, name, &stop, &status, &meters, &taps);
                let mut st = status.lock().unwrap();
                st.listeners.clear();
                st.server = match res {
                    Ok(()) => ServerState::Stopped,
                    Err(e) => ServerState::Error(format!("{e:#}")),
                };
                f32_store(&meters.server_level, 0.0);
            })
            .ok();
    }

    pub fn stop_server(&mut self) {
        if let Some(stop) = self.server_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
    }

    pub fn server_running(&self) -> bool {
        matches!(
            self.status.lock().unwrap().server,
            ServerState::Live | ServerState::Starting
        )
    }

    // ---------------------------------------------------------------- player

    /// Plays `address` through every device in `devices` (empty = system default).
    pub fn connect(&mut self, address: String, devices: Vec<String>) {
        self.disconnect();
        let stop = Arc::new(AtomicBool::new(false));
        self.player_stop = Some(stop.clone());
        self.status.lock().unwrap().player = PlayerState::Connecting(address.clone());
        let status = self.status.clone();
        let meters = self.meters.clone();
        thread::Builder::new()
            .name("airwave-player".into())
            .spawn(move || {
                let res = run_player(&address, &devices, &stop, &status, &meters);
                let mut st = status.lock().unwrap();
                st.player = match res {
                    Ok(()) => PlayerState::Idle,
                    Err(e) => PlayerState::Error(format!("{e:#}")),
                };
                f32_store(&meters.player_level, 0.0);
                meters.player_buffer_ms.store(0, Ordering::Relaxed);
            })
            .ok();
    }

    pub fn disconnect(&mut self) {
        if let Some(stop) = self.player_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        let mut st = self.status.lock().unwrap();
        if matches!(st.player, PlayerState::Error(_)) {
            st.player = PlayerState::Idle;
        }
    }

    // ------------------------------------------------------------- discovery

    fn start_scanner(&self) {
        let status = self.status.clone();
        let scan_now = self.scan_now.clone();
        let auto = self.auto_scan.clone();
        thread::Builder::new()
            .name("airwave-scan".into())
            .spawn(move || {
                let mut last = Instant::now() - Duration::from_secs(60);
                loop {
                    let due = auto.load(Ordering::Relaxed) && last.elapsed() > Duration::from_secs(4);
                    if scan_now.swap(false, Ordering::Relaxed) || due {
                        last = Instant::now();
                        let replies = probe_once(Duration::from_millis(900));
                        let mut st = status.lock().unwrap();
                        for (name, addr) in replies {
                            merge_found(&mut st.found, name, addr);
                        }
                        st.found
                            .retain(|f| f.last_seen.elapsed() < Duration::from_secs(12));
                    }
                    thread::sleep(Duration::from_millis(150));
                }
            })
            .ok();
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop_server();
        self.disconnect();
    }
}

fn merge_found(list: &mut Vec<Found>, name: String, addr: SocketAddr) {
    let now = Instant::now();
    if let Some(f) = list
        .iter_mut()
        .find(|f| f.name == name && f.addr.port() == addr.port())
    {
        // Prefer a network address over the loopback one for the same server.
        if f.addr.ip().is_loopback() || f.addr == addr {
            f.addr = addr;
        }
        f.last_seen = now;
    } else {
        list.push(Found {
            name,
            addr,
            last_seen: now,
        });
    }
}

fn probe_once(wait: Duration) -> Vec<(String, SocketAddr)> {
    let mut out = Vec::new();
    let Ok(sock) = UdpSocket::bind(("0.0.0.0", 0)) else {
        return out;
    };
    let _ = sock.set_broadcast(true);
    let _ = sock.set_read_timeout(Some(Duration::from_millis(100)));
    let _ = sock.send_to(PROBE, ("255.255.255.255", DISCOVERY_PORT));
    let _ = sock.send_to(PROBE, ("127.0.0.1", DISCOVERY_PORT));
    let end = Instant::now() + wait;
    let mut buf = [0u8; 512];
    while Instant::now() < end {
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            let text = String::from_utf8_lossy(&buf[..n]);
            if let Some(rest) = text.strip_prefix(REPLY_PREFIX) {
                let mut parts = rest.splitn(2, '|');
                let port = parts.next().and_then(|p| p.parse::<u16>().ok());
                let name = parts.next().unwrap_or("Unnamed").to_string();
                if let Some(port) = port {
                    out.push((name, SocketAddr::new(from.ip(), port)));
                }
            }
        }
    }
    out
}

fn run_responder(name: String, port: u16, stop: Arc<AtomicBool>, status: Arc<Mutex<Status>>) {
    let sock = match UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)) {
        Ok(s) => s,
        Err(e) => {
            status.lock().unwrap().discovery_note =
                Some(format!("Other devices can't find this computer automatically ({e})."));
            return;
        }
    };
    status.lock().unwrap().discovery_note = None;
    let _ = sock.set_read_timeout(Some(Duration::from_millis(250)));
    let reply = format!("{REPLY_PREFIX}{port}|{name}");
    let mut buf = [0u8; 64];
    while !stop.load(Ordering::Relaxed) {
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            if &buf[..n] == PROBE {
                let _ = sock.send_to(reply.as_bytes(), from);
            }
        }
    }
}

// -------------------------------------------------------------------- server

#[allow(clippy::too_many_arguments)]
fn run_server(
    device_name: Option<String>,
    quality: Quality,
    codec: Codec,
    port: u16,
    name: String,
    stop: &Arc<AtomicBool>,
    status: &Arc<Mutex<Status>>,
    meters: &Arc<Meters>,
    taps: &Arc<Mutex<Vec<Tap>>>,
) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))
        .with_context(|| format!("Port {port} is already in use"))?;
    listener.set_nonblocking(true)?;

    let host = cpal::default_host();
    let (device, loopback) = find_source(&host, device_name.as_deref())?;
    let config = if loopback {
        device.default_output_config()?
    } else {
        device.default_input_config()?
    };
    let rate = config.sample_rate().0;
    let in_ch = config.channels() as usize;
    let out_ch: usize = match quality {
        Quality::Mono => 1,
        Quality::Stereo => in_ch.min(2),
    };
    let (tx, rx) = sync_channel::<Vec<f32>>(128);
    let stream_cfg: cpal::StreamConfig = config.clone().into();
    let m = meters.clone();
    let stream = match config.sample_format() {
        SampleFormat::F32 => build_input::<f32>(&device, &stream_cfg, in_ch, out_ch, tx, m)?,
        SampleFormat::I16 => build_input::<i16>(&device, &stream_cfg, in_ch, out_ch, tx, m)?,
        SampleFormat::U16 => build_input::<u16>(&device, &stream_cfg, in_ch, out_ch, tx, m)?,
        SampleFormat::I32 => build_input::<i32>(&device, &stream_cfg, in_ch, out_ch, tx, m)?,
        SampleFormat::U8 => build_input::<u8>(&device, &stream_cfg, in_ch, out_ch, tx, m)?,
        SampleFormat::F64 => build_input::<f64>(&device, &stream_cfg, in_ch, out_ch, tx, m)?,
        other => bail!("This sound source uses an unsupported format ({other})"),
    };
    stream.play().context("Could not start recording from the sound source")?;

    {
        let mut st = status.lock().unwrap();
        st.server = ServerState::Live;
        st.server_format = format!(
            "{:.1} kHz · {} · {}",
            rate as f32 / 1000.0,
            if out_ch == 1 { "mono" } else { "stereo" },
            codec.describe()
        );
    }

    {
        let stop = stop.clone();
        let status = status.clone();
        thread::spawn(move || run_responder(name, port, stop, status));
    }

    let mut clients: Vec<(TcpStream, ListenerInfo)> = Vec::new();
    let mut encoder = Encoder::default();
    let mut samples: Vec<i16> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let before = clients.len();
        loop {
            match listener.accept() {
                Ok((s, addr)) => {
                    let _ = s.set_nonblocking(false);
                    let _ = s.set_nodelay(true);
                    let _ = s.set_write_timeout(Some(Duration::from_millis(300)));
                    clients.push((
                        s,
                        ListenerInfo {
                            label: addr.ip().to_string(),
                            since: Instant::now(),
                        },
                    ));
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }

        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(chunk) => {
                samples.clear();
                samples.extend(chunk.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16));
                let packets = encoder.packets(codec, rate, out_ch, &samples);
                let mut taps = taps.lock().unwrap();
                if !clients.is_empty() || !taps.is_empty() {
                    let n: usize = packets.iter().map(|p| p.len()).sum();
                    meters.bytes_sent.fetch_add(
                        (n * (clients.len() + taps.len())) as u32,
                        Ordering::Relaxed,
                    );
                }
                for packet in &packets {
                    clients.retain_mut(|(s, _)| s.write_all(packet).is_ok());
                    taps.retain(|t| {
                        !matches!(t.tx.try_send(packet.clone()), Err(TrySendError::Disconnected(_)))
                    });
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("The sound source stopped"),
        }

        if clients.len() != before || status.lock().unwrap().listeners.len() != clients.len() {
            status.lock().unwrap().listeners = clients.iter().map(|(_, i)| i.clone()).collect();
        }
    }
    drop(stream);
    Ok(())
}

fn build_input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    in_ch: usize,
    out_ch: usize,
    tx: SyncSender<Vec<f32>>,
    meters: Arc<Meters>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mut out = Vec::with_capacity(data.len() / in_ch.max(1) * out_ch);
            let mut peak = 0f32;
            for frame in data.chunks(in_ch.max(1)) {
                if out_ch == 1 {
                    let sum: f32 = frame.iter().map(|x| x.to_sample::<f32>()).sum();
                    let v = sum / frame.len() as f32;
                    peak = peak.max(v.abs());
                    out.push(v);
                } else {
                    let l = frame[0].to_sample::<f32>();
                    let r = if frame.len() > 1 {
                        frame[1].to_sample::<f32>()
                    } else {
                        l
                    };
                    peak = peak.max(l.abs()).max(r.abs());
                    out.push(l);
                    out.push(r);
                }
            }
            f32_store(&meters.server_level, peak);
            let _ = tx.try_send(out);
        },
        |e| eprintln!("input stream error: {e}"),
        None,
    )?;
    Ok(stream)
}

// -------------------------------------------------------------------- player

fn run_player(
    address: &str,
    device_names: &[String],
    stop: &Arc<AtomicBool>,
    status: &Arc<Mutex<Status>>,
    meters: &Arc<Meters>,
) -> Result<()> {
    let address = address.trim();
    let with_port = if address.contains(':') {
        address.to_string()
    } else {
        format!("{address}:{STREAM_PORT}")
    };
    let sa = with_port
        .to_socket_addrs()
        .with_context(|| format!("\"{address}\" is not a valid address"))?
        .next()
        .ok_or_else(|| anyhow!("\"{address}\" is not a valid address"))?;
    let mut sock = TcpStream::connect_timeout(&sa, Duration::from_secs(3))
        .with_context(|| format!("Could not reach {sa}"))?;
    sock.set_read_timeout(Some(Duration::from_millis(200)))?;

    let mut reader = PacketReader::default();
    let mut outputs: Vec<Output> = Vec::new();
    let mut format: Option<(u32, usize, Codec)> = None;
    let mut buf = vec![0u8; 16384];
    let mut silent_since = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match sock.read(&mut buf) {
            Ok(0) => bail!("The server stopped streaming"),
            Ok(n) => reader.push(&buf[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e).context("Connection lost"),
        }
        if format.is_none() && silent_since.elapsed() > Duration::from_secs(4) {
            bail!("The other side is not an Airwave sender");
        }
        while let Some(packet) = reader.next() {
            silent_since = Instant::now();
            let this = (packet.rate, packet.channels, packet.codec);
            if format.map(|(r, c, _)| (r, c)) != Some((packet.rate, packet.channels)) {
                // First packet, or the sender changed format: (re)open the speakers.
                outputs.clear();
                outputs = open_outputs(device_names, packet.rate, packet.channels, meters)?;
                let mut st = status.lock().unwrap();
                st.player = PlayerState::Playing(sa.to_string());
            }
            if format != Some(this) {
                status.lock().unwrap().player_format = format!(
                    "{:.1} kHz · {} · {} · {} speaker{}",
                    packet.rate as f32 / 1000.0,
                    if packet.channels == 1 { "mono" } else { "stereo" },
                    packet.codec.describe(),
                    outputs.len(),
                    if outputs.len() == 1 { "" } else { "s" }
                );
                format = Some(this);
            }
            for o in &outputs {
                let mut q = o.queue.lock().unwrap();
                q.extend(packet.samples.iter().map(|&s| s as f32 / 32768.0));
            }
        }
        if let (Some((rate, ch, _)), Some(o)) = (format, outputs.first()) {
            let q = o.queue.lock().unwrap().len();
            let ms = (q / ch) as u64 * 1000 / rate as u64;
            meters.player_buffer_ms.store(ms as u32, Ordering::Relaxed);
        }
    }
    drop(outputs);
    Ok(())
}

/// One speaker playing the stream, with its own waiting sound.
struct Output {
    queue: Arc<Mutex<VecDeque<f32>>>,
    _stream: cpal::Stream,
}

fn open_outputs(
    names: &[String],
    src_rate: u32,
    src_ch: usize,
    meters: &Arc<Meters>,
) -> Result<Vec<Output>> {
    let host = cpal::default_host();
    let mut devices = Vec::new();
    if names.is_empty() {
        devices.push(find_output(&host, None)?);
    } else {
        for n in names {
            match find_output_exact(&host, n) {
                Some(d) => devices.push(d),
                None => eprintln!("speaker \"{n}\" is not available"),
            }
        }
        if devices.is_empty() {
            devices.push(find_output(&host, None)?);
        }
    }
    let mut outputs = Vec::new();
    let mut last_err = None;
    for device in devices {
        let res = (|| -> Result<Output> {
            let config = device.default_output_config()?;
            let stream_cfg: cpal::StreamConfig = config.clone().into();
            let queue: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
            let p = PlayParams {
                queue: queue.clone(),
                meters: meters.clone(),
                src_rate,
                src_ch,
                out_rate: config.sample_rate().0,
                out_ch: config.channels() as usize,
            };
            let stream = match config.sample_format() {
                SampleFormat::F32 => build_output::<f32>(&device, &stream_cfg, p)?,
                SampleFormat::I16 => build_output::<i16>(&device, &stream_cfg, p)?,
                SampleFormat::U16 => build_output::<u16>(&device, &stream_cfg, p)?,
                SampleFormat::I32 => build_output::<i32>(&device, &stream_cfg, p)?,
                SampleFormat::U8 => build_output::<u8>(&device, &stream_cfg, p)?,
                SampleFormat::F64 => build_output::<f64>(&device, &stream_cfg, p)?,
                other => bail!("This speaker uses an unsupported format ({other})"),
            };
            stream.play().context("Could not start the speaker")?;
            Ok(Output {
                queue,
                _stream: stream,
            })
        })();
        match res {
            Ok(o) => outputs.push(o),
            Err(e) => last_err = Some(e),
        }
    }
    if outputs.is_empty() {
        return Err(last_err.unwrap_or_else(|| anyhow!("No speaker could be opened")));
    }
    Ok(outputs)
}

struct PlayParams {
    queue: Arc<Mutex<VecDeque<f32>>>,
    meters: Arc<Meters>,
    src_rate: u32,
    src_ch: usize,
    out_rate: u32,
    out_ch: usize,
}

fn build_output<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    p: PlayParams,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let PlayParams {
        queue,
        meters,
        src_rate,
        src_ch,
        out_rate,
        out_ch,
    } = p;
    let step = src_rate as f64 / out_rate as f64;
    let mut pos = 0f64;
    let mut prebuffering = true;
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            let silence = T::from_sample(0.0f32);
            let mut q = queue.lock().unwrap();
            let target_ms = meters.latency_ms.load(Ordering::Relaxed).max(10) as usize;
            let target = target_ms * src_rate as usize / 1000 * src_ch;
            if prebuffering && q.len() < target {
                data.fill(silence);
                f32_store(&meters.player_level, 0.0);
                return;
            }
            prebuffering = false;
            // Too much waiting audio: skip ahead so the delay stays small.
            if q.len() > target * 3 + src_ch * 2048 {
                let drop_n = (q.len() - target) / src_ch * src_ch;
                q.drain(..drop_n);
                pos = 0.0;
            }
            let gain = if meters.muted.load(Ordering::Relaxed) {
                0.0
            } else {
                let v = f32_load(&meters.volume);
                v * v
            };
            let mut peak = 0f32;
            let frames = data.len() / out_ch;
            let mut written = 0;
            for f in 0..frames {
                let i0 = pos as usize;
                if (i0 + 2) * src_ch > q.len() {
                    prebuffering = true;
                    break;
                }
                let frac = (pos - i0 as f64) as f32;
                for c in 0..out_ch {
                    let sc = c.min(src_ch - 1);
                    let a = q[i0 * src_ch + sc];
                    let b = q[(i0 + 1) * src_ch + sc];
                    let v = (a + (b - a) * frac) * gain;
                    peak = peak.max(v.abs());
                    data[f * out_ch + c] = T::from_sample(v);
                }
                pos += step;
                written = f + 1;
            }
            for s in &mut data[written * out_ch..] {
                *s = silence;
            }
            let consumed = (pos as usize).min(q.len() / src_ch);
            q.drain(..consumed * src_ch);
            pos -= consumed as f64;
            f32_store(&meters.player_level, peak);
        },
        |e| eprintln!("output stream error: {e}"),
        None,
    )?;
    Ok(stream)
}

// ------------------------------------------------------------------- devices

#[cfg(target_os = "windows")]
const LOOPBACK_PREFIX: &str = "Speaker sound: ";

fn list_devices() -> Devices {
    let host = cpal::default_host();
    let mut d = Devices {
        loaded: true,
        ..Default::default()
    };
    if let Ok(it) = host.input_devices() {
        d.inputs = it.filter_map(|x| x.name().ok()).collect();
    }
    if let Ok(it) = host.output_devices() {
        d.outputs = it.filter_map(|x| x.name().ok()).collect();
    }
    d.default_input = host.default_input_device().and_then(|x| x.name().ok());
    d.default_output = host.default_output_device().and_then(|x| x.name().ok());
    // On Windows, any speaker can be recorded directly ("loopback").
    #[cfg(target_os = "windows")]
    {
        let mut loop_names: Vec<String> = d
            .outputs
            .iter()
            .map(|n| format!("{LOOPBACK_PREFIX}{n}"))
            .collect();
        loop_names.append(&mut d.inputs);
        d.inputs = loop_names;
    }
    d.inputs.dedup();
    d.outputs.dedup();
    d
}

/// Returns the device to record from and whether it is a speaker (loopback).
fn find_source(host: &cpal::Host, name: Option<&str>) -> Result<(cpal::Device, bool)> {
    #[cfg(target_os = "windows")]
    if let Some(n) = name.and_then(|n| n.strip_prefix(LOOPBACK_PREFIX)) {
        let dev = host
            .output_devices()?
            .find(|d| d.name().map(|x| x == n).unwrap_or(false))
            .ok_or_else(|| anyhow!("The speaker \"{n}\" is no longer available"))?;
        return Ok((dev, true));
    }
    if let Some(n) = name {
        if let Some(dev) = host
            .input_devices()?
            .find(|d| d.name().map(|x| x == n).unwrap_or(false))
        {
            return Ok((dev, false));
        }
    }
    #[cfg(target_os = "windows")]
    if name.is_none() {
        if let Some(dev) = host.default_output_device() {
            return Ok((dev, true));
        }
    }
    let dev = host
        .default_input_device()
        .ok_or_else(|| anyhow!("No sound source was found on this computer"))?;
    Ok((dev, false))
}

fn find_output_exact(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    host.output_devices()
        .ok()?
        .find(|d| d.name().map(|x| x == name).unwrap_or(false))
}

fn find_output(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device> {
    if let Some(n) = name {
        if let Some(dev) = host
            .output_devices()?
            .find(|d| d.name().map(|x| x == n).unwrap_or(false))
        {
            return Ok(dev);
        }
    }
    host.default_output_device()
        .ok_or_else(|| anyhow!("No speaker was found on this computer"))
}

// --------------------------------------------------------------------- misc

pub fn computer_name() -> String {
    if let Ok(n) = std::env::var("COMPUTERNAME") {
        if !n.trim().is_empty() {
            return n.trim().to_string();
        }
    }
    if let Ok(n) = std::fs::read_to_string("/etc/hostname") {
        if !n.trim().is_empty() {
            return n.trim().to_string();
        }
    }
    if let Ok(n) = std::env::var("HOSTNAME") {
        if !n.trim().is_empty() {
            return n.trim().to_string();
        }
    }
    "This computer".to_string()
}

/// The address other devices on the same network use to reach this computer.
/// (Connecting a UDP socket only picks a route; nothing is sent.)
pub fn local_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect(("192.168.0.1", 9)).ok()?;
    let ip = s.local_addr().ok()?.ip();
    if ip.is_unspecified() {
        None
    } else {
        Some(ip)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for(secs: u64, mut f: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            if f() {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }
        false
    }

    /// Sender and player in one process: sound must flow end to end.
    #[test]
    #[ignore = "needs sound devices"]
    fn loopback_stream() {
        let mut engine = Engine::new(0.5, 100, false);
        engine.start_server(None, Quality::Stereo, Codec::Adpcm, 59300);
        assert!(
            wait_for(10, || engine.status.lock().unwrap().server == ServerState::Live),
            "server state: {:?}",
            engine.status.lock().unwrap().server
        );
        engine.connect("127.0.0.1:59300".into(), Vec::new());
        assert!(
            wait_for(10, || matches!(engine.status.lock().unwrap().player, PlayerState::Playing(_))),
            "player state: {:?}",
            engine.status.lock().unwrap().player
        );
        assert!(wait_for(5, || !engine.status.lock().unwrap().listeners.is_empty()));
        assert!(
            wait_for(5, || engine.meters.bytes_sent.load(Ordering::Relaxed) > 48_000),
            "no audio bytes were sent"
        );
        let found = probe_once(Duration::from_millis(900));
        println!("discovery found: {found:?}");
        // Another Airwave already running on this computer owns the discovery
        // port; then it is the one that answers.
        assert!(!found.is_empty(), "discovery did not answer");
        engine.disconnect();
        engine.stop_server();
    }

    /// Plays from AIRWAVE_TEST_ADDR (for example a phone) for AIRWAVE_TEST_SECS.
    #[test]
    #[ignore = "manual"]
    fn listen_for_a_while() {
        let addr = std::env::var("AIRWAVE_TEST_ADDR").expect("set AIRWAVE_TEST_ADDR");
        let secs: u64 = std::env::var("AIRWAVE_TEST_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        let mut engine = Engine::new(0.3, 100, false);
        engine.connect(addr, Vec::new());
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            println!(
                "player={:?} format={} buffer_ms={} level={:.3}",
                engine.status.lock().unwrap().player,
                engine.status.lock().unwrap().player_format,
                engine.meters.player_buffer_ms.load(Ordering::Relaxed),
                engine.meters.player_level()
            );
            thread::sleep(Duration::from_secs(1));
        }
        engine.disconnect();
    }

    /// Keeps a sender running so another device can be tested against it.
    /// AIRWAVE_TEST_SECS sets how long (default 30).
    #[test]
    #[ignore = "manual"]
    fn serve_for_a_while() {
        let secs: u64 = std::env::var("AIRWAVE_TEST_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);
        let mut engine = Engine::new(0.5, 100, false);
        engine.start_server(None, Quality::Stereo, Codec::Pcm, STREAM_PORT);
        let end = Instant::now() + Duration::from_secs(secs);
        while Instant::now() < end {
            {
                let st = engine.status.lock().unwrap();
                println!(
                    "server={:?} listeners={:?} bytes={} level={:.3}",
                    st.server,
                    st.listeners.iter().map(|l| l.label.clone()).collect::<Vec<_>>(),
                    engine.meters.bytes_sent.load(Ordering::Relaxed),
                    engine.meters.server_level()
                );
            }
            thread::sleep(Duration::from_secs(2));
        }
        engine.stop_server();
    }
}
