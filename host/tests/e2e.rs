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

#[tokio::test(flavor = "multi_thread")]
async fn migrates_both_ways_with_continuous_frame_ids() {
    let rig = rig().await;
    let mut c = Control::connect(rig.control).await;
    let token = c.configure(Mode::Auto, 2).await;
    let mut tcp = attach(&rig, token, 0).await;
    let (socket, echoed) = udp_client(&rig, token, 10).await;
    assert_eq!(start(&mut c, echoed).await, Transport::Udp);
    let (before, _) = collect_udp_frame(&socket, 0).await;
    let started = Instant::now();
    c.send(Msg::Stats(Stats { completion_permille: 1000, flags: STATS_UDP_SILENT, ..Default::default() })).await;
    let Msg::Migrate { transport: Transport::Tcp, next_frame_id } = c.expect().await else { panic!() };
    assert!(next_frame_id > before);
    let (h, _) = timeout(Duration::from_secs(1), read_chunk(&mut tcp)).await.unwrap().unwrap();
    assert!(started.elapsed() < Duration::from_millis(500), "stall {:?}", started.elapsed());
    assert!(h.frame_id >= next_frame_id && h.frame_id < next_frame_id + 5);
    tokio::spawn(async move { while read_chunk(&mut tcp).await.is_some() {} });
    let clean = Stats { completion_permille: 1000, udp_clean_ms: 3000, ..Default::default() };
    c.send(Msg::Stats(clean)).await;
    assert!(timeout(Duration::from_millis(10_200), c.recv()).await.is_err());
    c.send(Msg::Stats(clean)).await;
    let Msg::Migrate { transport: Transport::Udp, next_frame_id: back } = c.expect().await else { panic!() };
    assert!(back > next_frame_id);
    collect_udp_frame(&socket, back).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_pin_three_times_ends_and_five_lock_out() {
    let rig = rig().await;
    let mut c = Control::connect(rig.control).await;
    let n = c.hello().await;
    assert_eq!(c.pair(n, "000000").await, Msg::PairFail { attempts_left: 2 });
    assert_eq!(c.pair(n, "000000").await, Msg::PairFail { attempts_left: 1 });
    assert_eq!(c.pair(n, "000000").await, Msg::Bye(Bye::Auth));
    let mut c = Control::connect(rig.control).await;
    let n = c.hello().await;
    assert_eq!(c.pair(n, "000000").await, Msg::PairFail { attempts_left: 2 });
    assert_eq!(c.pair(n, "000000").await, Msg::PairFail { attempts_left: 1 });
    let mut c = Control::connect(rig.control).await;
    c.send(Msg::Hello { magic: MAGIC, version: VERSION }).await;
    assert_eq!(c.expect().await, Msg::Bye(Bye::Auth));
}

#[tokio::test(flavor = "multi_thread")]
async fn version_mismatch_and_garbage_get_bye() {
    let rig = rig().await;
    let mut c = Control::connect(rig.control).await;
    c.send(Msg::Hello { magic: MAGIC, version: 9 }).await;
    assert_eq!(c.expect().await, Msg::Bye(Bye::Version));
    let mut c = Control::connect(rig.control).await;
    c.0.write_all(&[0, 0, 0, 1, 200, 0]).await.unwrap();
    assert_eq!(c.expect().await, Msg::Bye(Bye::Protocol));
    let mut c = Control::connect(rig.control).await;
    c.0.write_all(&[0xff, 0xff, 0xff, 0xff, 1]).await.unwrap();
    assert_eq!(c.expect().await, Msg::Bye(Bye::Protocol));
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_tokens_get_nothing_and_host_survives_garbage() {
    let rig = rig().await;
    let mut c = Control::connect(rig.control).await;
    let token = c.configure(Mode::Auto, 1).await;
    let (_, echoed) = udp_client(&rig, [0; 16], 3).await;
    assert_eq!(echoed, 0);
    let mut rogue = attach(&rig, [0; 16], 0).await;
    assert_eq!(timeout(Duration::from_secs(1), rogue.read_u8()).await.unwrap().ok(), None);
    let mut out_of_range = attach(&rig, token, 7).await;
    assert!(timeout(Duration::from_millis(300), out_of_range.read_u8()).await.is_err_or_closed());
    let junk = UdpSocket::bind((LOCAL, 0)).await.unwrap();
    for size in [0usize, 1, 19, 32, 1500] {
        junk.send_to(&vec![0xA5; size], (LOCAL, rig.video)).await.unwrap();
        junk.send_to(&vec![0xA5; size], (LOCAL, rig.input)).await.unwrap();
    }
    let (socket, echoed) = udp_client(&rig, token, 10).await;
    assert_eq!(echoed, 10);
    assert_eq!(start(&mut c, echoed).await, Transport::Udp);
    collect_udp_frame(&socket, 0).await;
}

trait ClosedOrIdle {
    fn is_err_or_closed(&self) -> bool;
}

impl<E> ClosedOrIdle for Result<std::io::Result<u8>, E> {
    fn is_err_or_closed(&self) -> bool {
        !matches!(self, Ok(Ok(_)))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn no_path_is_reported() {
    let rig = rig().await;
    let mut c = Control::connect(rig.control).await;
    c.configure(Mode::Auto, 1).await;
    c.send(Msg::ProbeResult { sent: 10, echoed: 0 }).await;
    assert_eq!(c.expect().await, Msg::Bye(Bye::NoPath));
}

#[tokio::test(flavor = "multi_thread")]
async fn input_is_authenticated_deduplicated_and_released() {
    let rig = rig().await;
    let mut c = Control::connect(rig.control).await;
    let token = c.configure(Mode::Auto, 1).await;
    let (_socket, echoed) = udp_client(&rig, token, 10).await;
    start(&mut c, echoed).await;
    let key = |seq, keysym| InputEvent { seq, kind: EventKind::Key { keysym, down: true } };
    let udp = UdpSocket::bind((LOCAL, 0)).await.unwrap();
    let to = SocketAddr::new(LOCAL, rig.input);
    let packet = |token, events| InputPacket { token, events }.encode();
    udp.send_to(&packet([0; 16], vec![key(1, 1)]), to).await.unwrap();
    udp.send_to(&packet(token, vec![key(1, 97)]), to).await.unwrap();
    udp.send_to(&packet(token, vec![key(1, 97), key(2, 98)]), to).await.unwrap();
    let mut got = Vec::new();
    let mut take = |n| {
        for _ in 0..n {
            match rig.input_rx.recv_timeout(Duration::from_secs(2)).unwrap() {
                Cmd::Event(_, EventKind::Key { keysym, .. }) => got.push(keysym),
                _ => panic!(),
            }
        }
    };
    take(2);
    let mut tcp = TcpStream::connect(to).await.unwrap();
    tcp.write_all(&Attach { kind: 1, stripe: 0, token }.encode()).await.unwrap();
    let body = packet(token, vec![key(2, 98), key(3, 99)]);
    tcp.write_all(&(body.len() as u16).to_be_bytes()).await.unwrap();
    tcp.write_all(&body).await.unwrap();
    take(1);
    assert_eq!(got, [97, 98, 99]);
    assert!(rig.input_rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(c);
    assert!(matches!(rig.input_rx.recv_timeout(Duration::from_secs(2)).unwrap(), Cmd::Release(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn killed_client_frees_its_slot_and_token() {
    let rig = rig_with(1, true).await;
    let mut c = Control::connect(rig.control).await;
    let token = c.configure(Mode::Auto, 1).await;
    let mut second = Control::connect(rig.control).await;
    second.login().await;
    let config = Config { width: 0, height: 0, fps: 30, quality: 60, stripes: 1, mode: Mode::Auto, fec: true };
    second.send(Msg::Config(config)).await;
    assert_eq!(second.expect().await, Msg::Bye(Bye::Full));
    drop(c);
    sleep(Duration::from_millis(100)).await;
    assert_eq!(udp_client(&rig, token, 3).await.1, 0);
    let mut again = Control::connect(rig.control).await;
    let token = again.configure(Mode::Auto, 1).await;
    assert_eq!(udp_client(&rig, token, 10).await.1, 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn silent_peer_times_out_after_three_missed_pongs() {
    let rig = rig().await;
    let mut c = Control::connect(rig.control).await;
    c.login().await;
    let started = Instant::now();
    let bye = timeout(Duration::from_secs(6), async {
        loop {
            let len = c.0.read_u32().await.unwrap() as usize;
            let kind = c.0.read_u8().await.unwrap();
            let mut body = vec![0; len];
            c.0.read_exact(&mut body).await.unwrap();
            if let Msg::Bye(reason) = Msg::decode(kind, &body).unwrap() {
                return reason;
            }
        }
    });
    assert_eq!(bye.await.unwrap(), Bye::Timeout);
    assert!(started.elapsed() >= Duration::from_millis(2900));
}

#[tokio::test(flavor = "multi_thread")]
async fn static_screen_still_sends_a_frame_every_200ms() {
    let rig = rig_with(8, false).await;
    let rgba = vec![90; 320 * 180 * 4];
    rig.frames.send_replace(Some(Arc::new(Raw { width: 320, height: 180, rgba, captured_at: Instant::now() })));
    let mut c = Control::connect(rig.control).await;
    let token = c.configure(Mode::Auto, 1).await;
    let (socket, echoed) = udp_client(&rig, token, 10).await;
    start(&mut c, echoed).await;
    let mut ids = BTreeSet::new();
    let mut buf = [0; 2048];
    let began = Instant::now();
    while began.elapsed() < Duration::from_millis(1100) {
        if let Ok(Ok(n)) = timeout(Duration::from_millis(300), socket.recv(&mut buf)).await
            && let Ok(h) = ChunkHeader::parse(&buf[..n])
        {
            ids.insert(h.frame_id);
        }
    }
    assert!((5..=8).contains(&ids.len()), "{} frames", ids.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_tcp_reader_gets_recent_frames_not_a_backlog() {
    let rig = rig().await;
    let mut c = Control::connect(rig.control).await;
    let token = c.configure(Mode::TcpOnly, 1).await;
    let mut tcp = attach(&rig, token, 0).await;
    assert_eq!(start(&mut c, 0).await, Transport::Tcp);
    sleep(Duration::from_secs(2)).await;
    let mut newest = 0;
    let began = Instant::now();
    let mut skipped_ids = false;
    while began.elapsed() < Duration::from_millis(800) {
        let (h, _) = timeout(Duration::from_secs(2), read_chunk(&mut tcp)).await.unwrap().unwrap();
        skipped_ids |= newest != 0 && h.frame_id > newest + 1;
        newest = newest.max(h.frame_id);
    }
    assert!(skipped_ids, "latest-frame-wins never skipped");
    assert!(newest >= 55, "newest frame {newest}");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_viewers_get_independent_sessions() {
    let rig = rig().await;
    let mut a = Control::connect(rig.control).await;
    let mut b = Control::connect(rig.control).await;
    let ta = a.configure(Mode::Auto, 1).await;
    let tb = b.configure(Mode::TcpOnly, 1).await;
    assert_ne!(ta, tb);
    let (sa, ea) = udp_client(&rig, ta, 10).await;
    let mut tcp = attach(&rig, tb, 0).await;
    assert_eq!(start(&mut a, ea).await, Transport::Udp);
    assert_eq!(start(&mut b, 0).await, Transport::Tcp);
    collect_udp_frame(&sa, 0).await;
    assert!(timeout(Duration::from_secs(2), read_chunk(&mut tcp)).await.unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_says_bye_and_exits() {
    let rig = rig().await;
    let mut c = Control::connect(rig.control).await;
    c.login().await;
    rig.stop.cancel();
    assert_eq!(c.expect().await, Msg::Bye(Bye::Normal));
    timeout(Duration::from_secs(2), rig.done).await.unwrap().unwrap();
}
