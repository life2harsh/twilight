use serde_json::{Value, json};
use twilight_host::fec::packetize;
use twilight_host::proto::*;
use twilight_host::session::pair_tag;

fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}

const TOKEN: Token = [0xAB; 16];
const HOST_NONCE: [u8; 16] = [0x11; 16];
const CLIENT_NONCE: [u8; 16] = [0x22; 16];
const PIN: &str = "123456";

fn config() -> Config {
    Config { width: 1280, height: 720, fps: 30, quality: 60, stripes: 4, mode: Mode::Auto, fec: true }
}

fn stats() -> Stats {
    Stats {
        window_ms: 500,
        loss_permille: 12,
        completion_permille: 987,
        jitter_us: 1500,
        fps_x10: 298,
        latency_us: 21000,
        frames_skipped: 3,
        udp_clean_ms: 0,
        flags: STATS_UDP_SILENT,
    }
}

fn messages() -> Vec<(&'static str, Msg)> {
    let mac = pair_tag(PIN, &HOST_NONCE, &CLIENT_NONCE);
    vec![
        ("HELLO", Msg::Hello { magic: MAGIC, version: VERSION }),
        ("HELLO_OK", Msg::HelloOk { version: VERSION, host_nonce: HOST_NONCE }),
        ("PAIR", Msg::Pair { client_nonce: CLIENT_NONCE, mac }),
        ("PAIR_OK", Msg::PairOk { token: TOKEN }),
        ("PAIR_FAIL", Msg::PairFail { attempts_left: 2 }),
        ("CONFIG", Msg::Config(config())),
        ("CONFIG_OK", Msg::ConfigOk(config())),
        ("PROBE_RESULT", Msg::ProbeResult { sent: 10, echoed: 9 }),
        ("START", Msg::Start { transport: Transport::Udp, first_frame_id: 1 }),
        ("STATS", Msg::Stats(stats())),
        ("MIGRATE", Msg::Migrate { transport: Transport::Tcp, next_frame_id: 4660 }),
        ("PING", Msg::Ping { seq: 7, t1: 1_000_000 }),
        ("PONG", Msg::Pong { seq: 7, t1: 1_000_000, t2: 2_000_000 }),
        ("KEYFRAME_REQ", Msg::KeyframeReq),
        ("BYE", Msg::Bye(Bye::Timeout)),
    ]
}

fn build() -> Value {
    let image: Vec<u8> = (0..2500u32).map(|i| (i % 256) as u8).collect();
    let frame = packetize(258, 1_000_000, &image, true).unwrap();
    let input = InputPacket {
        token: TOKEN,
        events: vec![
            InputEvent { seq: 1, kind: EventKind::MouseMove { x: 32768, y: 16384 } },
            InputEvent { seq: 2, kind: EventKind::MouseButton { button: 1, down: true } },
            InputEvent { seq: 3, kind: EventKind::Key { keysym: 0xff0d, down: true } },
        ],
    };
    let scroll = InputPacket {
        token: TOKEN,
        events: vec![InputEvent { seq: 4, kind: EventKind::Scroll { dx: -1, dy: 3 } }],
    };
    json!({
        "pair": {
            "pin": PIN,
            "host_nonce": hex(&HOST_NONCE),
            "client_nonce": hex(&CLIENT_NONCE),
            "mac": hex(&pair_tag(PIN, &HOST_NONCE, &CLIENT_NONCE)),
        },
        "control": messages().iter().map(|(n, m)| (n.to_string(), json!(hex(&m.encode())))).collect::<serde_json::Map<_, _>>(),
        "attach": hex(&Attach { kind: 0, stripe: 2, token: TOKEN }.encode()),
        "probe": hex(&Probe { echo: false, seq: 5, t_us: 1_000_000, token: TOKEN }.encode()),
        "probe_echo": hex(&Probe { echo: true, seq: 5, t_us: 1_000_000, token: TOKEN }.encode()),
        "input": hex(&input.encode()),
        "input_scroll": hex(&scroll.encode()),
        "frame": {
            "image_rule": "2500 bytes, byte i = i % 256",
            "frame_id": 258,
            "timestamp_us": 1000000,
            "data": frame.data.iter().map(|c| hex(c)).collect::<Vec<_>>(),
            "parity": frame.parity.iter().map(|c| hex(c)).collect::<Vec<_>>(),
        },
    })
}

#[test]
fn matches_spec_vectors() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/vectors.json");
    let built = build();
    if std::env::var_os("TWILIGHT_REGEN").is_some() {
        std::fs::write(path, serde_json::to_string_pretty(&built).unwrap() + "\n").unwrap();
    }
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(stored, built);
}

#[test]
fn control_messages_round_trip() {
    for (name, msg) in messages() {
        let bytes = msg.encode();
        assert_eq!(u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize, bytes.len() - 5, "{name}");
        assert_eq!(Msg::decode(bytes[4], &bytes[5..]), Ok(msg), "{name}");
    }
}

#[test]
fn truncated_and_padded_messages_are_rejected() {
    for (name, msg) in messages() {
        let bytes = msg.encode();
        let mut long = bytes[5..].to_vec();
        long.push(0);
        assert_eq!(Msg::decode(bytes[4], &long), Err(ProtoError::BadLength), "{name}");
        if bytes.len() > 5 {
            assert_eq!(Msg::decode(bytes[4], &bytes[5..bytes.len() - 1]), Err(ProtoError::BadLength), "{name}");
        }
    }
    assert_eq!(Msg::decode(99, &[]), Err(ProtoError::BadType(99)));
}

#[test]
fn packets_reject_wrong_magic_and_length() {
    let mut chunk = packetize(1, 0, &[1, 2, 3], false).unwrap().data[0].to_vec();
    chunk[0] = 0;
    assert_eq!(ChunkHeader::parse(&chunk), Err(ProtoError::BadMagic));
    assert_eq!(ChunkHeader::parse(&[0; 19]), Err(ProtoError::BadLength));
    let probe = Probe { echo: false, seq: 1, t_us: 1, token: TOKEN };
    assert_eq!(Probe::parse(&probe.encode()[..31]), Err(ProtoError::BadLength));
    assert_eq!(Probe::parse(&probe.encode()), Ok(probe));
    let attach = Attach { kind: 1, stripe: 0, token: TOKEN };
    assert_eq!(Attach::parse(&attach.encode()), Ok(attach));
    let mut bad = InputPacket { token: TOKEN, events: vec![] }.encode();
    assert_eq!(InputPacket::parse(&bad), Err(ProtoError::BadValue));
    bad[3] = 4;
    assert_eq!(InputPacket::parse(&bad), Err(ProtoError::BadValue));
    bad[3] = 1;
    assert_eq!(InputPacket::parse(&bad), Err(ProtoError::BadLength));
}
