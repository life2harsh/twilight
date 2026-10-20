use crate::proto::{Mode, Stats, Transport};
use std::time::{Duration, Instant};

pub const PROBES: u8 = 10;
pub const PROBES_REQUIRED: u8 = 8;
const LOSS_LIMIT: u16 = 150;
const COMPLETION_LIMIT: u16 = 500;
const CLEAN_MS: u32 = 3000;
const HOLD_MIN: Duration = Duration::from_secs(10);
const HOLD_MAX: Duration = Duration::from_secs(80);
const STABLE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Silent,
    Loss,
    Completion,
    Clean,
}

pub fn choose(mode: Mode, echoed: u8, stripes: usize) -> Option<Transport> {
    let udp = (mode == Mode::Auto && echoed >= PROBES_REQUIRED) || (mode == Mode::UdpOnly && echoed > 0);
    let tcp = mode != Mode::UdpOnly && stripes > 0;
    match (udp, tcp) {
        (true, _) => Some(Transport::Udp),
        (false, true) => Some(Transport::Tcp),
        _ => None,
    }
}

pub struct Selector {
    mode: Mode,
    active: Transport,
    weak: u8,
    hold: Duration,
    last_change: Instant,
    returned: Option<Instant>,
}

impl Selector {
    pub fn new(mode: Mode, active: Transport, now: Instant) -> Self {
        Self { mode, active, weak: 0, hold: HOLD_MIN, last_change: now, returned: None }
    }

    pub fn active(&self) -> Transport {
        self.active
    }

    pub fn hold(&self) -> Duration {
        self.hold
    }

    pub fn on_stats(&mut self, s: &Stats, stripes: usize, now: Instant) -> Option<(Transport, Why)> {
        if self.mode != Mode::Auto {
            return None;
        }
        let out = match self.active {
            Transport::Udp => self.check_udp(s, stripes, now),
            Transport::Tcp => self.check_tcp(s, now),
        }?;
        self.active = out.0;
        self.last_change = now;
        self.weak = 0;
        Some(out)
    }

    fn check_udp(&mut self, s: &Stats, stripes: usize, now: Instant) -> Option<(Transport, Why)> {
        if self.returned.is_some_and(|t| now.duration_since(t) >= STABLE) {
            self.returned = None;
            self.hold = HOLD_MIN;
        }
        self.weak = if s.completion_permille < COMPLETION_LIMIT { self.weak.saturating_add(1) } else { 0 };
        let why = if s.udp_silent() {
            Why::Silent
        } else if s.loss_permille > LOSS_LIMIT {
            Why::Loss
        } else if self.weak >= 2 {
            Why::Completion
        } else {
            return None;
        };
        if stripes == 0 {
            return None;
        }
        if self.returned.take().is_some() {
            self.hold = (self.hold * 2).min(HOLD_MAX);
        }
        Some((Transport::Tcp, why))
    }

    fn check_tcp(&mut self, s: &Stats, now: Instant) -> Option<(Transport, Why)> {
        if s.udp_clean_ms >= CLEAN_MS && now.duration_since(self.last_change) >= self.hold {
            self.returned = Some(now);
            return Some((Transport::Udp, Why::Clean));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> Stats {
        Stats { completion_permille: 1000, ..Default::default() }
    }

    fn secs(t: Instant, s: u64) -> Instant {
        t + Duration::from_secs(s)
    }

    #[test]
    fn start_choice_matches_table() {
        assert_eq!(choose(Mode::Auto, 10, 4), Some(Transport::Udp));
        assert_eq!(choose(Mode::Auto, 8, 0), Some(Transport::Udp));
        assert_eq!(choose(Mode::Auto, 7, 4), Some(Transport::Tcp));
        assert_eq!(choose(Mode::Auto, 0, 1), Some(Transport::Tcp));
        assert_eq!(choose(Mode::Auto, 7, 0), None);
        assert_eq!(choose(Mode::UdpOnly, 1, 0), Some(Transport::Udp));
        assert_eq!(choose(Mode::UdpOnly, 0, 4), None);
        assert_eq!(choose(Mode::TcpOnly, 10, 2), Some(Transport::Tcp));
        assert_eq!(choose(Mode::TcpOnly, 10, 0), None);
    }

    #[test]
    fn silence_migrates_at_once() {
        let t = Instant::now();
        let mut s = Selector::new(Mode::Auto, Transport::Udp, t);
        let st = Stats { flags: 1, ..clean() };
        assert_eq!(s.on_stats(&st, 4, t), Some((Transport::Tcp, Why::Silent)));
        assert_eq!(s.active(), Transport::Tcp);
    }

    #[test]
    fn loss_threshold_is_strict() {
        let t = Instant::now();
        let mut s = Selector::new(Mode::Auto, Transport::Udp, t);
        assert_eq!(s.on_stats(&Stats { loss_permille: 150, ..clean() }, 4, t), None);
        let st = Stats { loss_permille: 151, ..clean() };
        assert_eq!(s.on_stats(&st, 4, t), Some((Transport::Tcp, Why::Loss)));
    }

    #[test]
    fn completion_needs_two_consecutive_windows() {
        let t = Instant::now();
        let mut s = Selector::new(Mode::Auto, Transport::Udp, t);
        let weak = Stats { completion_permille: 400, ..Default::default() };
        assert_eq!(s.on_stats(&weak, 4, t), None);
        assert_eq!(s.on_stats(&clean(), 4, t), None);
        assert_eq!(s.on_stats(&weak, 4, t), None);
        assert_eq!(s.on_stats(&weak, 4, t), Some((Transport::Tcp, Why::Completion)));
    }

    #[test]
    fn stays_on_udp_without_stripes() {
        let t = Instant::now();
        let mut s = Selector::new(Mode::Auto, Transport::Udp, t);
        assert_eq!(s.on_stats(&Stats { flags: 1, ..clean() }, 0, t), None);
        assert_eq!(s.active(), Transport::Udp);
    }

    #[test]
    fn fixed_modes_never_migrate() {
        let t = Instant::now();
        let mut s = Selector::new(Mode::UdpOnly, Transport::Udp, t);
        assert_eq!(s.on_stats(&Stats { flags: 1, ..clean() }, 4, t), None);
    }

    #[test]
    fn return_waits_for_clean_and_hold() {
        let t = Instant::now();
        let mut s = Selector::new(Mode::Auto, Transport::Tcp, t);
        let ok = Stats { udp_clean_ms: 3000, ..clean() };
        assert_eq!(s.on_stats(&ok, 4, secs(t, 9)), None);
        assert_eq!(s.on_stats(&Stats { udp_clean_ms: 2999, ..clean() }, 4, secs(t, 10)), None);
        assert_eq!(s.on_stats(&ok, 4, secs(t, 10)), Some((Transport::Udp, Why::Clean)));
    }

    #[test]
    fn flapping_doubles_hold_up_to_limit_and_stability_resets() {
        let t = Instant::now();
        let mut s = Selector::new(Mode::Auto, Transport::Tcp, t);
        let ok = Stats { udp_clean_ms: 3000, ..clean() };
        let bad = Stats { flags: 1, ..clean() };
        let mut now = t;
        for want in [20, 40, 80, 80] {
            now += s.hold();
            assert!(s.on_stats(&ok, 4, now).is_some());
            now += Duration::from_secs(1);
            assert!(s.on_stats(&bad, 4, now).is_some());
            assert_eq!(s.hold(), Duration::from_secs(want));
        }
        now += s.hold();
        assert!(s.on_stats(&ok, 4, now).is_some());
        assert_eq!(s.on_stats(&clean(), 4, now + STABLE), None);
        assert_eq!(s.hold(), HOLD_MIN);
    }
}
