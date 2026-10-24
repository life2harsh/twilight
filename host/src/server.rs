use crate::fec::{Frame, packetize};
use crate::input::{Cmd, Dedup};
use crate::proto::*;
use crate::rate::Rate;
use crate::selector::{self, Selector};
use crate::session::{Machine, Step};
use crate::source::{Encoder, Raw};
use crate::stripes::Stripes;
use anyhow::{Context, Result};
use socket2::{Domain, Protocol, SockRef, Socket, Type};
use std::collections::HashMap;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc as amsc, watch};
use tokio::task::JoinSet;
use tokio::time::{sleep, sleep_until, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

const STEP_TIMEOUT: Duration = Duration::from_secs(10);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(2);
const PONG_TIMEOUT: Duration = Duration::from_secs(3);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const RESEND_AFTER: Duration = Duration::from_millis(200);
const LOCKOUT: Duration = Duration::from_secs(60);
const LOCKOUT_FAILS: u32 = 5;
const PACE_BATCH: usize = 8;
const UDP_BUFFER: usize = 4 << 20;

#[derive(Clone)]
pub struct Options {
    pub bind: IpAddr,
    pub base_port: u16,
    pub pin: String,
    pub max_sessions: usize,
    pub tcp_sndbuf: usize,
    pub metrics: Option<std::path::PathBuf>,
}

pub struct Ports {
    pub control: u16,
    pub video: u16,
    pub input: u16,
}

struct Session {
    id: u64,
    token: Token,
    config: Config,
    stop: CancellationToken,
    udp_addr: Mutex<Option<SocketAddr>>,
    stripes: Stripes,
    transport: AtomicU8,
    rate: Mutex<Rate>,
    next_frame: AtomicU32,
    frames_sent: AtomicU64,
    bytes_sent: AtomicU64,
    dedup: Mutex<Dedup>,
    input_tx: mpsc::Sender<Cmd>,
    input_busy: AtomicBool,
}

impl Session {
    fn transport(&self) -> Transport {
        if self.transport.load(Relaxed) == Transport::Udp as u8 { Transport::Udp } else { Transport::Tcp }
    }

    fn input(&self, events: &[InputEvent]) {
        for kind in self.dedup.lock().unwrap().accept(events) {
            let _ = self.input_tx.send(Cmd::Event(self.id, kind));
        }
    }
}

#[derive(Default)]
struct Lockouts(Mutex<HashMap<IpAddr, (u32, Instant)>>);

impl Lockouts {
    fn blocked(&self, ip: IpAddr) -> bool {
        let mut map = self.0.lock().unwrap();
        map.retain(|_, (_, at)| at.elapsed() < LOCKOUT);
        map.get(&ip).is_some_and(|(fails, _)| *fails >= LOCKOUT_FAILS)
    }

    fn fail(&self, ip: IpAddr) {
        let mut map = self.0.lock().unwrap();
        let entry = map.entry(ip).or_insert((0, Instant::now()));
        *entry = (entry.0 + 1, Instant::now());
    }

    fn clear(&self, ip: IpAddr) {
        self.0.lock().unwrap().remove(&ip);
    }
}

struct Metrics {
    file: Option<Mutex<std::fs::File>>,
}

impl Metrics {
    fn open(path: Option<&std::path::Path>) -> Result<Self> {
        let Some(path) = path else { return Ok(Self { file: None }) };
        let mut file = std::fs::File::create(path).context("creating metrics file")?;
        writeln!(file, "t_us,session,event,transport,quality,fps,rtt_us,loss_pm,completion_pm,jitter_us,rx_fps_x10,latency_us,skipped_rx,skipped_tx,frames,bytes")?;
        Ok(Self { file: Some(Mutex::new(file)) })
    }

    fn line(&self, row: std::fmt::Arguments) {
        if let Some(file) = &self.file {
            let _ = writeln!(file.lock().unwrap(), "{row}");
        }
    }
}

struct Hub {
    opts: Options,
    epoch: Instant,
    sessions: Mutex<HashMap<Token, Arc<Session>>>,
    frames: watch::Sender<Option<Arc<Raw>>>,
    video_udp: Arc<UdpSocket>,
    input_tx: mpsc::Sender<Cmd>,
    lockouts: Lockouts,
    metrics: Metrics,
    next_id: AtomicU64,
    stop: CancellationToken,
}

impl Hub {
    fn now_us(&self) -> u64 {
        self.epoch.elapsed().as_micros() as u64
    }

    fn session(&self, token: &Token) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(token).cloned()
    }
}

pub struct Server {
    hub: Arc<Hub>,
    control: TcpListener,
    video: TcpListener,
    input: TcpListener,
    input_udp: UdpSocket,
}

fn udp_socket(addr: SocketAddr) -> Result<UdpSocket> {
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_nonblocking(true)?;
    let _ = socket.set_send_buffer_size(UDP_BUFFER);
    let _ = socket.set_recv_buffer_size(UDP_BUFFER);
    socket.bind(&addr.into())?;
    Ok(UdpSocket::from_std(socket.into())?)
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut out = [0; N];
    getrandom::fill(&mut out).map_err(|e| anyhow::anyhow!("random: {e}"))?;
    Ok(out)
}

impl Server {
    pub async fn bind(
        opts: Options,
        frames: watch::Sender<Option<Arc<Raw>>>,
        input_tx: mpsc::Sender<Cmd>,
        stop: CancellationToken,
    ) -> Result<Self> {
        let port = |offset: u16| if opts.base_port == 0 { 0 } else { opts.base_port + offset };
        let control = TcpListener::bind((opts.bind, port(0))).await.context("binding control port")?;
        let video = TcpListener::bind((opts.bind, port(1))).await.context("binding video port")?;
        let input = TcpListener::bind((opts.bind, port(2))).await.context("binding input port")?;
        let video_udp = udp_socket(SocketAddr::new(opts.bind, video.local_addr()?.port()))?;
        let input_udp = udp_socket(SocketAddr::new(opts.bind, input.local_addr()?.port()))?;
        let metrics = Metrics::open(opts.metrics.as_deref())?;
        let hub = Arc::new(Hub {
            opts,
            epoch: Instant::now(),
            sessions: Mutex::default(),
            frames,
            video_udp: Arc::new(video_udp),
            input_tx,
            lockouts: Lockouts::default(),
            metrics,
            next_id: AtomicU64::new(1),
            stop,
        });
        Ok(Self { hub, control, video, input, input_udp })
    }

    pub fn ports(&self) -> Result<Ports> {
        Ok(Ports {
            control: self.control.local_addr()?.port(),
            video: self.video.local_addr()?.port(),
            input: self.input.local_addr()?.port(),
        })
    }

    pub async fn run(self) {
        let Self { hub, control, video, input, input_udp } = self;
        let stop = hub.stop.clone();
        let mut sessions = JoinSet::new();
        tokio::spawn(probe_loop(hub.clone()));
        tokio::spawn(input_udp_loop(hub.clone(), input_udp));
        tokio::spawn(attach_loop(hub.clone(), video, 0));
        tokio::spawn(attach_loop(hub.clone(), input, 1));
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                accepted = control.accept() => match accepted {
                    Ok((stream, peer)) => {
                        sessions.spawn(serve_control(hub.clone(), stream, peer));
                    }
                    Err(e) => warn!("accept failed: {e}"),
                },
                Some(_) = sessions.join_next() => {}
            }
        }
        while sessions.join_next().await.is_some() {}
    }
}

async fn probe_loop(hub: Arc<Hub>) {
    let mut buf = [0; 64];
    loop {
        tokio::select! {
            _ = hub.stop.cancelled() => return,
            received = hub.video_udp.recv_from(&mut buf) => {
                let Ok((n, peer)) = received else { continue };
                let Ok(probe) = Probe::parse(&buf[..n]) else { continue };
                if probe.echo {
                    continue;
                }
                let Some(session) = hub.session(&probe.token) else { continue };
                *session.udp_addr.lock().unwrap() = Some(peer);
                let echo = Probe { echo: true, ..probe }.encode();
                let _ = hub.video_udp.send_to(&echo, peer).await;
            }
        }
    }
}

async fn input_udp_loop(hub: Arc<Hub>, socket: UdpSocket) {
    let mut buf = [0; MAX_INPUT + 1];
    loop {
        tokio::select! {
            _ = hub.stop.cancelled() => return,
            received = socket.recv_from(&mut buf) => {
                let Ok((n, _)) = received else { continue };
                let Ok(packet) = InputPacket::parse(&buf[..n]) else { continue };
                if let Some(session) = hub.session(&packet.token) {
                    session.input(&packet.events);
                }
            }
        }
    }
}

