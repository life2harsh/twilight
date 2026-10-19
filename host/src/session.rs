use crate::proto::{Bye, Config, MAGIC, Msg, Stats, Token, VERSION};
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const PAIR_ATTEMPTS: u8 = 3;
const PAIR_LABEL: &[u8] = b"twilight-pair-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Connected,
    Helloed,
    Paired,
    Probing,
    Streaming,
    Closed,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Reply(Msg),
    PairFailed(Msg),
    Paired,
    Configured(Config),
    Probed { echoed: u8 },
    Stats(Stats),
    Ping { seq: u32, t1: u64 },
    Pong { t1: u64 },
    Keyframe,
    Closed,
}

fn pair_mac(pin: &str, host_nonce: &[u8; 16], client_nonce: &[u8; 16]) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(pin.as_bytes()).expect("hmac takes any key");
    mac.update(PAIR_LABEL);
    mac.update(host_nonce);
    mac.update(client_nonce);
    mac
}

pub fn pair_tag(pin: &str, host_nonce: &[u8; 16], client_nonce: &[u8; 16]) -> [u8; 32] {
    pair_mac(pin, host_nonce, client_nonce).finalize().into_bytes().into()
}

pub struct Machine {
    state: State,
    pin: String,
    nonce: [u8; 16],
    token: Token,
    attempts: u8,
}

impl Machine {
    pub fn new(pin: String, nonce: [u8; 16], token: Token) -> Self {
        Self { state: State::Connected, pin, nonce, token, attempts: PAIR_ATTEMPTS }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn close(&mut self) {
        self.state = State::Closed;
    }

    pub fn start_streaming(&mut self) {
        self.state = State::Streaming;
    }

    pub fn on_msg(&mut self, msg: Msg) -> Result<Step, Bye> {
        use State::*;
        match (self.state, msg) {
            (Closed, _) => Err(Bye::Protocol),
            (Connected, Msg::Hello { magic, version }) => {
                if magic != MAGIC || version != VERSION {
                    return Err(Bye::Version);
                }
                self.state = Helloed;
                Ok(Step::Reply(Msg::HelloOk { version: VERSION, host_nonce: self.nonce }))
            }
            (Connected, _) => Err(Bye::Protocol),
            (_, Msg::Bye(_)) => {
                self.state = Closed;
                Ok(Step::Closed)
            }
            (_, Msg::Ping { seq, t1 }) => Ok(Step::Ping { seq, t1 }),
            (_, Msg::Pong { t1, .. }) => Ok(Step::Pong { t1 }),
            (Helloed, Msg::Pair { client_nonce, mac }) => {
                if pair_mac(&self.pin, &self.nonce, &client_nonce).verify_slice(&mac).is_ok() {
                    self.state = Paired;
                    return Ok(Step::Paired);
                }
                self.attempts -= 1;
                if self.attempts == 0 {
                    return Err(Bye::Auth);
                }
                Ok(Step::PairFailed(Msg::PairFail { attempts_left: self.attempts }))
            }
            (Paired, Msg::Config(c)) => {
                self.state = Probing;
                Ok(Step::Configured(c.clamped()))
            }
            (Probing, Msg::ProbeResult { echoed, .. }) => Ok(Step::Probed { echoed }),
            (Streaming, Msg::Stats(s)) => Ok(Step::Stats(s)),
            (Streaming, Msg::KeyframeReq) => Ok(Step::Keyframe),
            _ => Err(Bye::Protocol),
        }
    }

    pub fn token(&self) -> Token {
        self.token
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Mode;

    const NONCE: [u8; 16] = [7; 16];
    const CLIENT: [u8; 16] = [9; 16];

    fn machine() -> Machine {
        Machine::new("123456".into(), NONCE, [1; 16])
    }

    fn hello() -> Msg {
        Msg::Hello { magic: MAGIC, version: VERSION }
    }

    fn pair(pin: &str) -> Msg {
        Msg::Pair { client_nonce: CLIENT, mac: pair_tag(pin, &NONCE, &CLIENT) }
    }

    fn config() -> Config {
        Config { width: 0, height: 0, fps: 200, quality: 0, stripes: 99, mode: Mode::Auto, fec: true }
    }

    #[test]
    fn happy_path_reaches_streaming() {
        let mut m = machine();
        assert!(matches!(m.on_msg(hello()), Ok(Step::Reply(Msg::HelloOk { .. }))));
        assert_eq!(m.on_msg(pair("123456")), Ok(Step::Paired));
        let Ok(Step::Configured(c)) = m.on_msg(Msg::Config(config())) else { panic!() };
        assert_eq!((c.fps, c.quality, c.stripes), (120, 1, 8));
        assert_eq!(m.state(), State::Probing);
        assert_eq!(m.on_msg(Msg::ProbeResult { sent: 10, echoed: 9 }), Ok(Step::Probed { echoed: 9 }));
        m.start_streaming();
        assert_eq!(m.on_msg(Msg::Stats(Stats::default())), Ok(Step::Stats(Stats::default())));
        assert_eq!(m.on_msg(Msg::KeyframeReq), Ok(Step::Keyframe));
        assert_eq!(m.on_msg(Msg::Bye(Bye::Normal)), Ok(Step::Closed));
        assert_eq!(m.state(), State::Closed);
        assert_eq!(m.on_msg(hello()), Err(Bye::Protocol));
    }

    #[test]
    fn version_and_magic_are_checked() {
        assert_eq!(machine().on_msg(Msg::Hello { magic: 1, version: VERSION }), Err(Bye::Version));
        assert_eq!(machine().on_msg(Msg::Hello { magic: MAGIC, version: 2 }), Err(Bye::Version));
    }

    #[test]
    fn nothing_precedes_hello() {
        for msg in [Msg::Ping { seq: 1, t1: 1 }, Msg::Bye(Bye::Normal), pair("123456")] {
            assert_eq!(machine().on_msg(msg), Err(Bye::Protocol));
        }
    }

    #[test]
    fn out_of_order_messages_are_protocol_errors() {
        let mut m = machine();
        m.on_msg(hello()).unwrap();
        assert_eq!(m.on_msg(Msg::Config(config())), Err(Bye::Protocol));
        let mut m = machine();
        m.on_msg(hello()).unwrap();
        m.on_msg(pair("123456")).unwrap();
        assert_eq!(m.on_msg(Msg::Stats(Stats::default())), Err(Bye::Protocol));
        assert_eq!(machine().on_msg(Msg::KeyframeReq), Err(Bye::Protocol));
    }

    #[test]
    fn three_bad_pins_end_the_session() {
        let mut m = machine();
        m.on_msg(hello()).unwrap();
        assert_eq!(m.on_msg(pair("000000")), Ok(Step::PairFailed(Msg::PairFail { attempts_left: 2 })));
        assert_eq!(m.on_msg(pair("000000")), Ok(Step::PairFailed(Msg::PairFail { attempts_left: 1 })));
        assert_eq!(m.on_msg(pair("000000")), Err(Bye::Auth));
    }

    #[test]
    fn good_pin_after_a_bad_one_works() {
        let mut m = machine();
        m.on_msg(hello()).unwrap();
        m.on_msg(pair("000000")).unwrap();
        assert_eq!(m.on_msg(pair("123456")), Ok(Step::Paired));
    }

    #[test]
    fn ping_is_valid_after_hello() {
        let mut m = machine();
        m.on_msg(hello()).unwrap();
        assert_eq!(m.on_msg(Msg::Ping { seq: 2, t1: 3 }), Ok(Step::Ping { seq: 2, t1: 3 }));
    }
}
