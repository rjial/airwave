//! Stream packets and the optional compression.
//!
//! Every stream (Wi-Fi and USB) is a series of self-contained packets:
//!   "AWP2" | u32 rate | u16 channels | u8 codec | u8 0 | u32 frames | u32 len | payload
//! (all little-endian, 20-byte header, whole packet at most 16 KB so a phone
//! in USB accessory mode can read each one in a single go). A receiver that
//! loses its place simply searches for the next "AWP2".
//!
//! Codecs: 0 = 16-bit PCM (no compression), 1 = µ-law (half the data),
//! 2 = IMA ADPCM (a quarter of the data).

pub const MAGIC: &[u8; 4] = b"AWP2";
pub const HEADER: usize = 20;
pub const MAX_PACKET: usize = 16384;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Codec {
    Pcm,
    MuLaw,
    Adpcm,
}

impl Codec {
    pub fn id(self) -> u8 {
        match self {
            Codec::Pcm => 0,
            Codec::MuLaw => 1,
            Codec::Adpcm => 2,
        }
    }
    pub fn from_id(id: u8) -> Option<Codec> {
        match id {
            0 => Some(Codec::Pcm),
            1 => Some(Codec::MuLaw),
            2 => Some(Codec::Adpcm),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Codec::Pcm => "Off",
            Codec::MuLaw => "Light",
            Codec::Adpcm => "Strong",
        }
    }
    pub fn describe(self) -> &'static str {
        match self {
            Codec::Pcm => "no compression",
            Codec::MuLaw => "light compression",
            Codec::Adpcm => "strong compression",
        }
    }
    /// Payload bytes for `frames` frames of `ch` channels.
    fn payload_len(self, frames: usize, ch: usize) -> usize {
        match self {
            Codec::Pcm => frames * ch * 2,
            Codec::MuLaw => frames * ch,
            Codec::Adpcm => 4 * ch + (frames * ch).div_ceil(2),
        }
    }
    fn max_frames(self, ch: usize) -> usize {
        let room = MAX_PACKET - HEADER;
        match self {
            Codec::Pcm => room / (ch * 2),
            Codec::MuLaw => room / ch,
            Codec::Adpcm => (room - 4 * ch) * 2 / ch,
        }
    }
}

// ------------------------------------------------------------------ µ-law

const ULAW_BIAS: i32 = 0x84;
const ULAW_CLIP: i32 = 32635;

pub fn ulaw_encode(s: i16) -> u8 {
    let mut x = s as i32;
    let sign = if x < 0 {
        x = -x;
        0x80
    } else {
        0
    };
    if x > ULAW_CLIP {
        x = ULAW_CLIP;
    }
    x += ULAW_BIAS;
    let mut exp = 7;
    let mut mask = 0x4000;
    while exp > 0 && x & mask == 0 {
        exp -= 1;
        mask >>= 1;
    }
    let mant = (x >> (exp + 3)) & 0x0F;
    !((sign | (exp << 4) | mant) as u8)
}

pub fn ulaw_decode(u: u8) -> i16 {
    let u = !u;
    let exp = ((u >> 4) & 7) as i32;
    let mant = (u & 0x0F) as i32;
    let x = (((mant << 3) + ULAW_BIAS) << exp) - ULAW_BIAS;
    if u & 0x80 != 0 {
        -x as i16
    } else {
        x as i16
    }
}

// ------------------------------------------------------------------ ADPCM

const INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

#[derive(Clone, Copy, Default, Debug)]
pub struct AdpcmChannel {
    pred: i32,
    index: i32,
}

impl AdpcmChannel {
    fn step(&mut self, nib: u8) -> i16 {
        let step = STEP_TABLE[self.index as usize];
        let mut delta = step >> 3;
        if nib & 4 != 0 {
            delta += step;
        }
        if nib & 2 != 0 {
            delta += step >> 1;
        }
        if nib & 1 != 0 {
            delta += step >> 2;
        }
        self.pred = if nib & 8 != 0 {
            self.pred - delta
        } else {
            self.pred + delta
        }
        .clamp(-32768, 32767);
        self.index = (self.index + INDEX_TABLE[nib as usize]).clamp(0, 88);
        self.pred as i16
    }

    fn encode(&mut self, sample: i16) -> u8 {
        let step = STEP_TABLE[self.index as usize];
        let mut diff = sample as i32 - self.pred;
        let mut nib = 0u8;
        if diff < 0 {
            nib = 8;
            diff = -diff;
        }
        if diff >= step {
            nib |= 4;
            diff -= step;
        }
        if diff >= step >> 1 {
            nib |= 2;
            diff -= step >> 1;
        }
        if diff >= step >> 2 {
            nib |= 1;
        }
        // Track exactly what the decoder will reconstruct.
        self.step(nib);
        nib
    }
}

/// Running encoder state, kept between packets for smooth sound.
#[derive(Default)]
pub struct Encoder {
    adpcm: Vec<AdpcmChannel>,
}

impl Encoder {
    /// Cuts interleaved samples into packets.
    pub fn packets(&mut self, codec: Codec, rate: u32, ch: usize, samples: &[i16]) -> Vec<Vec<u8>> {
        let ch = ch.max(1);
        if self.adpcm.len() != ch {
            self.adpcm = vec![AdpcmChannel::default(); ch];
        }
        let frames_total = samples.len() / ch;
        let max_frames = codec.max_frames(ch);
        let mut out = Vec::new();
        let mut f0 = 0;
        while f0 < frames_total {
            let frames = (frames_total - f0).min(max_frames);
            let part = &samples[f0 * ch..(f0 + frames) * ch];
            let len = codec.payload_len(frames, ch);
            let mut p = Vec::with_capacity(HEADER + len);
            p.extend_from_slice(MAGIC);
            p.extend_from_slice(&rate.to_le_bytes());
            p.extend_from_slice(&(ch as u16).to_le_bytes());
            p.push(codec.id());
            p.push(0);
            p.extend_from_slice(&(frames as u32).to_le_bytes());
            p.extend_from_slice(&(len as u32).to_le_bytes());
            match codec {
                Codec::Pcm => {
                    for s in part {
                        p.extend_from_slice(&s.to_le_bytes());
                    }
                }
                Codec::MuLaw => p.extend(part.iter().map(|&s| ulaw_encode(s))),
                Codec::Adpcm => {
                    for c in &self.adpcm {
                        p.extend_from_slice(&(c.pred as i16).to_le_bytes());
                        p.push(c.index as u8);
                        p.push(0);
                    }
                    let mut byte = 0u8;
                    for (i, &s) in part.iter().enumerate() {
                        let nib = self.adpcm[i % ch].encode(s);
                        if i % 2 == 0 {
                            byte = nib;
                        } else {
                            p.push(byte | (nib << 4));
                        }
                    }
                    if part.len() % 2 == 1 {
                        p.push(byte);
                    }
                }
            }
            debug_assert_eq!(p.len(), HEADER + len);
            out.push(p);
            f0 += frames;
        }
        out
    }
}

/// One decoded packet.
pub struct Packet {
    pub rate: u32,
    pub channels: usize,
    pub codec: Codec,
    pub samples: Vec<i16>,
}

fn decode_payload(codec: Codec, ch: usize, frames: usize, data: &[u8]) -> Option<Vec<i16>> {
    if data.len() != codec.payload_len(frames, ch) {
        return None;
    }
    let n = frames * ch;
    Some(match codec {
        Codec::Pcm => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| i16::from_le_bytes(b))
            .collect(),
        Codec::MuLaw => data.iter().map(|&u| ulaw_decode(u)).collect(),
        Codec::Adpcm => {
            let mut st: Vec<AdpcmChannel> = (0..ch)
                .map(|c| AdpcmChannel {
                    pred: i16::from_le_bytes([data[c * 4], data[c * 4 + 1]]) as i32,
                    index: (data[c * 4 + 2] as i32).min(88),
                })
                .collect();
            let body = &data[4 * ch..];
            (0..n)
                .map(|i| {
                    let b = body[i / 2];
                    let nib = if i % 2 == 0 { b & 0x0F } else { b >> 4 };
                    st[i % ch].step(nib)
                })
                .collect()
        }
    })
}

/// Pulls packets out of a byte stream, searching again after damage.
#[derive(Default)]
pub struct PacketReader {
    buf: Vec<u8>,
    start: usize,
}

impl PacketReader {
    pub fn push(&mut self, bytes: &[u8]) {
        if self.start > 0 && self.start * 2 > self.buf.len() {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    pub fn next(&mut self) -> Option<Packet> {
        loop {
            let avail = &self.buf[self.start..];
            if avail.len() < HEADER {
                return None;
            }
            if &avail[..4] != MAGIC {
                // Lost our place: jump to the next possible start.
                match avail[1..].windows(4).position(|w| w == MAGIC) {
                    Some(p) => self.start += p + 1,
                    None => self.start = self.buf.len() - 3,
                }
                continue;
            }
            let rate = u32::from_le_bytes(avail[4..8].try_into().unwrap());
            let ch = u16::from_le_bytes(avail[8..10].try_into().unwrap()) as usize;
            let codec = Codec::from_id(avail[10]);
            let frames = u32::from_le_bytes(avail[12..16].try_into().unwrap()) as usize;
            let len = u32::from_le_bytes(avail[16..20].try_into().unwrap()) as usize;
            let sane = (8000..=192_000).contains(&rate)
                && (1..=8).contains(&ch)
                && codec.is_some()
                && len <= MAX_PACKET * 4;
            if !sane {
                self.start += 1;
                continue;
            }
            if avail.len() < HEADER + len {
                return None;
            }
            let codec = codec.unwrap();
            let decoded = decode_payload(codec, ch, frames, &avail[HEADER..HEADER + len]);
            self.start += HEADER + len;
            match decoded {
                Some(samples) => {
                    return Some(Packet {
                        rate,
                        channels: ch,
                        codec,
                        samples,
                    })
                }
                None => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(n: usize, ch: usize) -> Vec<i16> {
        (0..n * ch)
            .map(|i| ((i / ch) as f32 * 0.05).sin() * 12000.0)
            .map(|v| v as i16)
            .collect()
    }

    #[test]
    fn every_codec_round_trips_and_fits_usb() {
        for codec in [Codec::Pcm, Codec::MuLaw, Codec::Adpcm] {
            for ch in [1, 2] {
                let input = tone(30_000, ch);
                let mut enc = Encoder::default();
                let packets = enc.packets(codec, 48_000, ch, &input);
                let mut reader = PacketReader::default();
                let mut out = Vec::new();
                for p in &packets {
                    assert!(p.len() <= MAX_PACKET, "{codec:?} packet too big");
                    reader.push(p);
                    while let Some(pk) = reader.next() {
                        assert_eq!(pk.channels, ch);
                        out.extend(pk.samples);
                    }
                }
                assert_eq!(out.len(), input.len(), "{codec:?}");
                let err: f64 = input
                    .iter()
                    .zip(&out)
                    .map(|(a, b)| (*a as f64 - *b as f64).abs())
                    .sum::<f64>()
                    / input.len() as f64;
                let limit = match codec {
                    Codec::Pcm => 0.0,
                    Codec::MuLaw => 200.0,
                    Codec::Adpcm => 600.0,
                };
                assert!(err <= limit, "{codec:?} ch{ch}: average error {err}");
            }
        }
    }

    #[test]
    fn reader_recovers_from_garbage() {
        let mut enc = Encoder::default();
        let packets = enc.packets(Codec::Adpcm, 48_000, 2, &tone(100_000, 2));
        assert!(packets.len() >= 4);
        let mut reader = PacketReader::default();
        reader.push(b"junk AWP noise");
        reader.push(&packets[0][..50]); // cut short, then a whole one follows
        for p in &packets[1..] {
            reader.push(p);
        }
        let mut got = 0;
        while let Some(p) = reader.next() {
            got += p.samples.len();
        }
        // At most the damaged packet and the one it swallowed are lost.
        let last = packets.len() - 1;
        assert!(
            got >= (last - 1) * 2 * Codec::Adpcm.max_frames(2) / 2,
            "only {got} samples"
        );
    }
}
