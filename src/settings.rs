//! Remembers the person's choices between runs in a small text file.

use std::path::PathBuf;

use crate::codec::Codec;
use crate::engine::{Quality, STREAM_PORT};

pub const BUFFER_MIN_MS: u32 = 20;
pub const BUFFER_MAX_MS: u32 = 2000;

#[derive(Clone, Debug)]
pub struct Settings {
    pub source_device: Option<String>,
    /// Speakers to play through at the same time (empty = system default).
    pub output_devices: Vec<String>,
    pub quality: Quality,
    pub codec: Codec,
    /// How much sound the listener keeps waiting, in milliseconds.
    pub buffer_ms: u32,
    pub volume: f32,
    pub auto_start_server: bool,
    pub auto_scan: bool,
    pub port: u16,
    pub last_address: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            source_device: None,
            output_devices: Vec::new(),
            quality: Quality::Stereo,
            codec: Codec::Pcm,
            buffer_ms: 100,
            volume: 0.8,
            auto_start_server: false,
            auto_scan: true,
            port: STREAM_PORT,
            last_address: String::new(),
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                if cfg!(target_os = "macos") {
                    p.push("Library/Application Support");
                } else {
                    p.push(".config");
                }
                p
            })
        })?;
    Some(base.join("airwave").join("settings.conf"))
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Settings::default();
        let Some(p) = path() else { return s };
        let Ok(text) = std::fs::read_to_string(p) else {
            return s;
        };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "source_device" if !v.is_empty() => s.source_device = Some(v.to_string()),
                "output_device" if !v.is_empty() && s.output_devices.is_empty() => {
                    s.output_devices = vec![v.to_string()]
                }
                "output_devices" => {
                    s.output_devices = v
                        .split('|')
                        .filter(|x| !x.is_empty())
                        .map(|x| x.to_string())
                        .collect()
                }
                "codec" => {
                    s.codec = match v {
                        "light" => Codec::MuLaw,
                        "strong" => Codec::Adpcm,
                        _ => Codec::Pcm,
                    }
                }
                "buffer_ms" => {
                    s.buffer_ms = v
                        .parse::<u32>()
                        .unwrap_or(100)
                        .clamp(BUFFER_MIN_MS, BUFFER_MAX_MS)
                }
                "quality" => {
                    s.quality = if v == "mono" {
                        Quality::Mono
                    } else {
                        Quality::Stereo
                    }
                }
                "latency" => {
                    s.buffer_ms = match v {
                        "low" => 40,
                        "stable" => 250,
                        _ => 100,
                    }
                }
                "volume" => s.volume = v.parse::<f32>().unwrap_or(0.8).clamp(0.0, 1.0),
                "auto_start_server" => s.auto_start_server = v == "true",
                "auto_scan" => s.auto_scan = v != "false",
                "port" => s.port = v.parse().unwrap_or(STREAM_PORT),
                "last_address" => s.last_address = v.to_string(),
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = format!(
            "source_device={}\noutput_devices={}\nquality={}\ncodec={}\nbuffer_ms={}\nvolume={:.2}\nauto_start_server={}\nauto_scan={}\nport={}\nlast_address={}\n",
            self.source_device.clone().unwrap_or_default(),
            self.output_devices.join("|"),
            if self.quality == Quality::Mono { "mono" } else { "stereo" },
            match self.codec {
                Codec::Pcm => "off",
                Codec::MuLaw => "light",
                Codec::Adpcm => "strong",
            },
            self.buffer_ms,
            self.volume,
            self.auto_start_server,
            self.auto_scan,
            self.port,
            self.last_address,
        );
        let _ = std::fs::write(p, text);
    }
}
