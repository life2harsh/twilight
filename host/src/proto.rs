use bytes::{Buf, BufMut, BytesMut};
use thiserror::Error;

pub const MAGIC: u16 = 0x544C;
pub const VERSION: u8 = 1;
pub const CONTROL_PORT: u16 = 48100;
pub const VIDEO_PORT: u16 = 48101;
pub const INPUT_PORT: u16 = 48102;
pub const CHUNK_HEADER: usize = 20;
pub const CHUNK_PAYLOAD: usize = 1200;
pub const MAX_CONTROL: usize = 1024;
pub const ATTACH_LEN: usize = 22;
pub const PROBE_LEN: usize = 32;
pub const INPUT_HEADER: usize = 20;
pub const EVENT_LEN: usize = 16;
pub const MAX_INPUT: usize = INPUT_HEADER + 3 * EVENT_LEN;

pub const FLAG_KEYFRAME: u8 = 0x01;
pub const FLAG_PARITY: u8 = 0x02;
pub const FLAG_LAST: u8 = 0x04;
pub const FLAG_PROBE: u8 = 0x80;
pub const FLAG_PROBE_ECHO: u8 = 0x81;
pub const STATS_UDP_SILENT: u8 = 0x01;

pub type Token = [u8; 16];

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("bad magic or version")]
    BadMagic,
    #[error("truncated or oversized packet")]
    BadLength,
    #[error("unknown message type {0}")]
    BadType(u8),
    #[error("invalid field value")]
    BadValue,
}

type Result<T> = std::result::Result<T, ProtoError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Udp = 0,
    Tcp = 1,
}

impl Transport {
    fn parse(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Udp),
            1 => Ok(Self::Tcp),
            _ => Err(ProtoError::BadValue),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Auto = 0,
    UdpOnly = 1,
    TcpOnly = 2,
}

impl Mode {
    fn parse(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Auto),
            1 => Ok(Self::UdpOnly),
            2 => Ok(Self::TcpOnly),
            _ => Err(ProtoError::BadValue),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bye {
    Normal = 0,
    Version = 1,
    Auth = 2,
    Protocol = 3,
    Timeout = 4,
    Full = 5,
    NoPath = 6,
}

impl Bye {
    fn parse(v: u8) -> Result<Self> {
        Ok(match v {
            0 => Self::Normal,
            1 => Self::Version,
            2 => Self::Auth,
            3 => Self::Protocol,
            4 => Self::Timeout,
            5 => Self::Full,
            6 => Self::NoPath,
            _ => return Err(ProtoError::BadValue),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub width: u16,
    pub height: u16,
    pub fps: u8,
    pub quality: u8,
    pub stripes: u8,
    pub mode: Mode,
    pub fec: bool,
}

impl Config {
    pub fn clamped(self) -> Self {
        Self {
            fps: self.fps.clamp(1, 120),
            quality: self.quality.clamp(1, 100),
            stripes: self.stripes.clamp(1, 8),
            ..self
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub window_ms: u16,
    pub loss_permille: u16,
    pub completion_permille: u16,
    pub jitter_us: u32,
    pub fps_x10: u16,
    pub latency_us: u32,
    pub frames_skipped: u32,
    pub udp_clean_ms: u32,
    pub flags: u8,
}

impl Stats {
    pub fn udp_silent(&self) -> bool {
        self.flags & STATS_UDP_SILENT != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    Hello { magic: u16, version: u8 },
    HelloOk { version: u8, host_nonce: [u8; 16] },
    Pair { client_nonce: [u8; 16], mac: [u8; 32] },
    PairOk { token: Token },
    PairFail { attempts_left: u8 },
    Config(Config),
    ConfigOk(Config),
    ProbeResult { sent: u8, echoed: u8 },
    Start { transport: Transport, first_frame_id: u32 },
    Stats(Stats),
    Migrate { transport: Transport, next_frame_id: u32 },
    Ping { seq: u32, t1: u64 },
    Pong { seq: u32, t1: u64, t2: u64 },
    KeyframeReq,
    Bye(Bye),
}

fn need(b: &[u8], n: usize) -> Result<()> {
    if b.len() == n { Ok(()) } else { Err(ProtoError::BadLength) }
}

fn array<const N: usize>(b: &mut &[u8]) -> [u8; N] {
    let mut out = [0; N];
    b.copy_to_slice(&mut out);
    out
}

fn put_config(o: &mut BytesMut, c: &Config) {
    o.put_u16(c.width);
    o.put_u16(c.height);
    o.put_u8(c.fps);
    o.put_u8(c.quality);
    o.put_u8(c.stripes);
    o.put_u8(c.mode as u8);
    o.put_u8(c.fec as u8);
}

fn get_config(b: &mut &[u8]) -> Result<Config> {
    need(b, 9)?;
    Ok(Config {
        width: b.get_u16(),
        height: b.get_u16(),
        fps: b.get_u8(),
        quality: b.get_u8(),
        stripes: b.get_u8(),
        mode: Mode::parse(b.get_u8())?,
        fec: match b.get_u8() {
            0 => false,
            1 => true,
            _ => return Err(ProtoError::BadValue),
        },
    })
}

impl Msg {
    pub fn type_code(&self) -> u8 {
        match self {
            Self::Hello { .. } => 1,
            Self::HelloOk { .. } => 2,
            Self::Pair { .. } => 3,
            Self::PairOk { .. } => 4,
            Self::PairFail { .. } => 5,
            Self::Config(_) => 6,
            Self::ConfigOk(_) => 7,
            Self::ProbeResult { .. } => 8,
            Self::Start { .. } => 9,
            Self::Stats(_) => 10,
            Self::Migrate { .. } => 11,
            Self::Ping { .. } => 12,
            Self::Pong { .. } => 13,
            Self::KeyframeReq => 14,
            Self::Bye(_) => 15,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut o = BytesMut::with_capacity(64);
        o.put_u32(0);
        o.put_u8(self.type_code());
        match self {
            Self::Hello { magic, version } => {
                o.put_u16(*magic);
                o.put_u8(*version);
            }
            Self::HelloOk { version, host_nonce } => {
                o.put_u8(*version);
                o.put_slice(host_nonce);
            }
            Self::Pair { client_nonce, mac } => {
                o.put_slice(client_nonce);
                o.put_slice(mac);
            }
            Self::PairOk { token } => o.put_slice(token),
            Self::PairFail { attempts_left } => o.put_u8(*attempts_left),
            Self::Config(c) | Self::ConfigOk(c) => put_config(&mut o, c),
            Self::ProbeResult { sent, echoed } => {
                o.put_u8(*sent);
                o.put_u8(*echoed);
            }
            Self::Start { transport, first_frame_id: id }
            | Self::Migrate { transport, next_frame_id: id } => {
                o.put_u8(*transport as u8);
                o.put_u32(*id);
            }
            Self::Stats(s) => {
                o.put_u16(s.window_ms);
                o.put_u16(s.loss_permille);
                o.put_u16(s.completion_permille);
                o.put_u32(s.jitter_us);
                o.put_u16(s.fps_x10);
                o.put_u32(s.latency_us);
                o.put_u32(s.frames_skipped);
                o.put_u32(s.udp_clean_ms);
                o.put_u8(s.flags);
            }
            Self::Ping { seq, t1 } => {
                o.put_u32(*seq);
                o.put_u64(*t1);
            }
            Self::Pong { seq, t1, t2 } => {
                o.put_u32(*seq);
                o.put_u64(*t1);
                o.put_u64(*t2);
            }
            Self::KeyframeReq => {}
            Self::Bye(r) => o.put_u8(*r as u8),
        }
        let len = (o.len() - 5) as u32;
        o[..4].copy_from_slice(&len.to_be_bytes());
        o.to_vec()
    }

