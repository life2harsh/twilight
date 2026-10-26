use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use twilight_host::fec::repair;
use twilight_host::input::Cmd;
use twilight_host::proto::*;
use twilight_host::server::{Options, Server};
use twilight_host::session::pair_tag;
use twilight_host::source::{Raw, Source, spawn_capture};

const PIN: &str = "424242";
const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

struct Rig {
    control: u16,
    video: u16,
    input: u16,
    stop: CancellationToken,
    input_rx: mpsc::Receiver<Cmd>,
    frames: watch::Sender<Option<Arc<Raw>>>,
    done: tokio::task::JoinHandle<()>,
}

async fn rig_with(max_sessions: usize, capture: bool) -> Rig {
    let stop = CancellationToken::new();
    let (frames, _) = watch::channel(None);
    if capture {
        let open = || Ok(Source::Synthetic { width: 640, height: 360, tick: 0 });
        spawn_capture(open, 30, frames.clone(), stop.clone()).unwrap();
    }
    let (input_tx, input_rx) = mpsc::channel();
    let options = Options {
        bind: LOCAL,
        base_port: 0,
        pin: PIN.into(),
        max_sessions,
        tcp_sndbuf: 256 * 1024,
        metrics: None,
    };
    let server = Server::bind(options, frames.clone(), input_tx, stop.clone()).await.unwrap();
    let ports = server.ports().unwrap();
    let done = tokio::spawn(server.run());
    Rig { control: ports.control, video: ports.video, input: ports.input, stop, input_rx, frames, done }
}

async fn rig() -> Rig {
    rig_with(8, true).await
}

struct Control(TcpStream);

impl Control {
    async fn connect(port: u16) -> Self {
        let s = TcpStream::connect((LOCAL, port)).await.unwrap();
        s.set_nodelay(true).unwrap();
        Self(s)
    }

    async fn send(&mut self, m: Msg) {
        self.0.write_all(&m.encode()).await.unwrap();
    }

    async fn recv(&mut self) -> Option<Msg> {
        loop {
            let len = self.0.read_u32().await.ok()? as usize;
            let kind = self.0.read_u8().await.ok()?;
            let mut body = vec![0; len];
            self.0.read_exact(&mut body).await.ok()?;
            let msg = Msg::decode(kind, &body).unwrap();
            if let Msg::Ping { seq, t1 } = msg {
                self.send(Msg::Pong { seq, t1, t2: 0 }).await;
                continue;
            }
            return Some(msg);
        }
    }

    async fn expect(&mut self) -> Msg {
        timeout(Duration::from_secs(5), self.recv()).await.expect("timed out").expect("closed")
    }

    async fn hello(&mut self) -> [u8; 16] {
        self.send(Msg::Hello { magic: MAGIC, version: VERSION }).await;
        let Msg::HelloOk { host_nonce, .. } = self.expect().await else { panic!() };
        host_nonce
    }

    async fn pair(&mut self, nonce: [u8; 16], pin: &str) -> Msg {
        let client_nonce = [5; 16];
        self.send(Msg::Pair { client_nonce, mac: pair_tag(pin, &nonce, &client_nonce) }).await;
        self.expect().await
    }

    async fn login(&mut self) -> Token {
        let nonce = self.hello().await;
        let Msg::PairOk { token } = self.pair(nonce, PIN).await else { panic!() };
        token
    }

    async fn configure(&mut self, mode: Mode, stripes: u8) -> Token {
        let token = self.login().await;
        let config = Config { width: 0, height: 0, fps: 30, quality: 60, stripes, mode, fec: true };
        self.send(Msg::Config(config)).await;
        assert_eq!(self.expect().await, Msg::ConfigOk(config));
        token
    }
}

async fn udp_client(rig: &Rig, token: Token, count: u8) -> (UdpSocket, u8) {
    let socket = UdpSocket::bind((LOCAL, 0)).await.unwrap();
    socket.connect((LOCAL, rig.video)).await.unwrap();
    for seq in 0..count {
        let p = Probe { echo: false, seq: seq as u32, t_us: 9, token };
        socket.send(&p.encode()).await.unwrap();
    }
    let mut buf = [0; 2048];
    let mut echoed = 0;
    while echoed < count {
        let Ok(Ok(n)) = timeout(Duration::from_millis(300), socket.recv(&mut buf)).await else { break };
        if let Ok(p) = Probe::parse(&buf[..n]) {
            assert!(p.echo);
            assert_eq!(p.token, token);
            echoed += 1;
        }
    }
    (socket, echoed)
}

async fn attach(rig: &Rig, token: Token, stripe: u8) -> TcpStream {
    let mut s = TcpStream::connect((LOCAL, rig.video)).await.unwrap();
    s.write_all(&Attach { kind: 0, stripe, token }.encode()).await.unwrap();
    s
}

async fn read_chunk(s: &mut TcpStream) -> Option<(ChunkHeader, Vec<u8>)> {
    let len = s.read_u16().await.ok()? as usize;
    let mut body = vec![0; len];
    s.read_exact(&mut body).await.ok()?;
    Some((ChunkHeader::parse(&body).unwrap(), body[CHUNK_HEADER..].to_vec()))
}

#[derive(Default)]
struct Assembly {
    total: usize,
    data: HashMap<u16, Vec<u8>>,
    parity: HashMap<u16, Vec<u8>>,
}

impl Assembly {
    fn add(&mut self, h: ChunkHeader, body: &[u8]) {
        self.total = h.chunk_total as usize;
        let side = if h.flags & FLAG_PARITY != 0 { &mut self.parity } else { &mut self.data };
        side.insert(h.chunk_idx, body.to_vec());
    }

    fn image(&self) -> Option<Vec<u8>> {
        (self.data.len() == self.total)
            .then(|| (0..self.total as u16).flat_map(|i| self.data[&i].clone()).collect())
    }

    fn drop_chunk(&mut self, idx: u16) -> Vec<u8> {
        self.data.remove(&idx).unwrap()
    }

    fn fill_from_parity(&mut self, idx: u16) {
        let groups = self.total.div_ceil(4);
        let group = idx as usize % groups;
        let known: Vec<&[u8]> = (group..self.total)
            .step_by(groups)
            .filter(|&i| i != idx as usize)
            .map(|i| self.data[&(i as u16)].as_slice())
            .collect();
        let fixed = repair(&self.parity[&(group as u16)], &known).unwrap();
        self.data.insert(idx, fixed);
    }
}

fn assert_jpeg(image: &[u8], width: usize, height: usize) {
    let header = turbojpeg::read_header(image).unwrap();
    assert_eq!((header.width, header.height), (width, height));
}

async fn collect_udp_frame(socket: &UdpSocket, oldest: u32) -> (u32, Assembly) {
    let mut buf = [0; 2048];
    let mut frames: HashMap<u32, Assembly> = HashMap::new();
    loop {
        let n = timeout(Duration::from_secs(5), socket.recv(&mut buf)).await.expect("no udp").unwrap();
        let Ok(h) = ChunkHeader::parse(&buf[..n]) else { continue };
        if h.flags & FLAG_PROBE != 0 || h.frame_id < oldest {
            continue;
        }
        assert!(n <= 1222);
        let entry = frames.entry(h.frame_id).or_default();
        entry.add(h, &buf[CHUNK_HEADER..n]);
        if entry.image().is_some() && entry.parity.len() == entry.total.div_ceil(4) {
            return (h.frame_id, frames.remove(&h.frame_id).unwrap());
        }
    }
}

async fn start(c: &mut Control, echoed: u8) -> Transport {
    c.send(Msg::ProbeResult { sent: 10, echoed }).await;
    let Msg::Start { transport, first_frame_id } = c.expect().await else { panic!() };
    assert_eq!(first_frame_id, 1);
    transport
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_over_udp_with_working_parity() {
    let rig = rig().await;
    let mut c = Control::connect(rig.control).await;
    let token = c.configure(Mode::Auto, 2).await;
    let (socket, echoed) = udp_client(&rig, token, 10).await;
    assert_eq!(echoed, 10);
    assert_eq!(start(&mut c, echoed).await, Transport::Udp);
    let (_, mut frame) = collect_udp_frame(&socket, 0).await;
    let whole = frame.image().unwrap();
    assert_jpeg(&whole, 640, 360);
    for idx in [0, frame.total as u16 - 1] {
        let original = frame.drop_chunk(idx);
        frame.fill_from_parity(idx);
        assert_eq!(frame.data[&idx], original);
    }
    assert_eq!(frame.image().unwrap(), whole);
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_over_striped_tcp_when_udp_is_blocked() {
    let rig = rig().await;
    let mut c = Control::connect(rig.control).await;
    let token = c.configure(Mode::Auto, 4).await;
    let mut stripes = Vec::new();
    for i in 0..4 {
        stripes.push(attach(&rig, token, i).await);
    }
    assert_eq!(start(&mut c, 0).await, Transport::Tcp);
    let (tx, mut rx) = tokio::sync::mpsc::channel(4096);
    for mut s in stripes {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(c) = read_chunk(&mut s).await {
                if tx.send(c).await.is_err() {
                    break;
                }
            }
        });
    }
    let mut frames: HashMap<u32, Assembly> = HashMap::new();
    let mut complete = 0;
    while complete < 10 {
        let (h, body) = timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert_eq!(h.flags & FLAG_PARITY, 0);
        let entry = frames.entry(h.frame_id).or_default();
        entry.add(h, &body);
        if let Some(image) = entry.image() {
            assert_jpeg(&image, 640, 360);
            complete += 1;
        }
    }
}

