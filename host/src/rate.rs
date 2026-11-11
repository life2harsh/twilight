use crate::proto::{Stats, Transport};
use std::time::{Duration, Instant};

const MIN_QUALITY: u8 = 20;
const MIN_FPS: u8 = 5;
const STEP: u8 = 5;
const LOSS_LIMIT: u16 = 20;
const QUEUE_FRAMES: u32 = 3;
const CLEAN: Duration = Duration::from_secs(2);

pub struct Rate {
    target_quality: u8,
    target_fps: u8,
    quality: u8,
    fps: u8,
    last_signal: Instant,
    min_rtt_us: u64,
    rtt_us: u64,
    srtt_us: u64,
    backlog: u32,
}

fn cut(v: u8, floor: u8) -> u8 {
    ((v as u32 * 4 / 5) as u8).max(floor)
}

impl Rate {
    pub fn new(quality: u8, fps: u8, now: Instant) -> Self {
        Self {
            target_quality: quality,
            target_fps: fps,
            quality,
            fps,
            last_signal: now,
            min_rtt_us: u64::MAX,
            rtt_us: 0,
            srtt_us: 0,
            backlog: 0,
        }
    }

    pub fn quality(&self) -> u8 {
        self.quality
    }

    pub fn fps(&self) -> u8 {
        self.fps
    }

    pub fn rtt_us(&self) -> u64 {
        self.rtt_us
    }

    pub fn on_rtt(&mut self, rtt_us: u64) {
        self.rtt_us = rtt_us;
        self.srtt_us = if self.srtt_us == 0 { rtt_us } else { (self.srtt_us * 7 + rtt_us) / 8 };
        self.min_rtt_us = self.min_rtt_us.min(rtt_us);
    }

    pub fn on_stripe_backlog(&mut self, backed_up: bool) {
        self.backlog = if backed_up { self.backlog.saturating_add(1) } else { 0 };
    }

    fn congested(&self, s: &Stats, transport: Transport) -> bool {
        match transport {
            Transport::Udp => {
                s.loss_permille > LOSS_LIMIT || s.jitter_us as u64 > 1_000_000 / self.fps.max(1) as u64
            }
            Transport::Tcp => {
                self.backlog >= QUEUE_FRAMES
                    || (self.min_rtt_us != u64::MAX && self.srtt_us > self.min_rtt_us * 2 + 20_000)
            }
        }
    }

    pub fn on_stats(&mut self, s: &Stats, transport: Transport, now: Instant) {
        self.apply(self.congested(s, transport), now);
    }

    fn apply(&mut self, congested: bool, now: Instant) {
        if congested {
            self.last_signal = now;
            self.backlog = 0;
            if self.quality > MIN_QUALITY.min(self.target_quality) {
                self.quality = cut(self.quality, MIN_QUALITY.min(self.target_quality));
            } else {
                self.fps = cut(self.fps, MIN_FPS.min(self.target_fps));
            }
        } else if now.duration_since(self.last_signal) >= CLEAN {
            self.last_signal = now;
            if self.fps < self.target_fps {
                self.fps = (self.fps + STEP).min(self.target_fps);
            } else {
                self.quality = (self.quality + STEP).min(self.target_quality);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lossy() -> Stats {
        Stats { loss_permille: 21, ..Default::default() }
    }

    #[test]
    fn multiplicative_decrease_then_fps() {
        let t = Instant::now();
        let mut r = Rate::new(60, 30, t);
        let seen: Vec<_> = (0..7)
            .map(|_| {
                r.on_stats(&lossy(), Transport::Udp, t);
                r.quality()
            })
            .collect();
        assert_eq!(seen, [48, 38, 30, 24, 20, 20, 20]);
        assert_eq!(r.fps(), 19);
    }

    #[test]
    fn rtt_spike_is_not_congestion_but_sustained_rise_is() {
        let t = Instant::now();
        let mut r = Rate::new(60, 30, t);
        for rtt in [2_000, 2_000, 50_000, 2_000] {
            r.on_rtt(rtt);
            r.on_stats(&Stats::default(), Transport::Tcp, t);
        }
        assert_eq!(r.quality(), 60);
        for _ in 0..8 {
            r.on_rtt(60_000);
        }
        r.on_stats(&Stats::default(), Transport::Tcp, t);
        assert_eq!(r.quality(), 48);
    }

    #[test]
    fn fps_floor_holds() {
        let t = Instant::now();
        let mut r = Rate::new(20, 30, t);
        (0..30).for_each(|_| r.on_stats(&lossy(), Transport::Udp, t));
        assert_eq!((r.quality(), r.fps()), (20, 5));
    }

    #[test]
    fn loss_limit_is_strict() {
        let t = Instant::now();
        let mut r = Rate::new(60, 30, t);
        r.on_stats(&Stats { loss_permille: 20, ..Default::default() }, Transport::Udp, t);
        assert_eq!(r.quality(), 60);
    }

    #[test]
    fn recovers_fps_first_then_quality_every_two_seconds() {
        let t = Instant::now();
        let mut r = Rate::new(40, 30, t);
        (0..6).for_each(|_| r.on_stats(&lossy(), Transport::Udp, t));
        assert_eq!((r.quality(), r.fps()), (20, 15));
        let mut now = t;
        let clean = Stats::default();
        for want in [(20, 20), (20, 25), (20, 30), (25, 30)] {
            now += Duration::from_millis(1999);
            r.on_stats(&clean, Transport::Udp, now);
            now += Duration::from_millis(1);
            r.on_stats(&clean, Transport::Udp, now);
            assert_eq!((r.quality(), r.fps()), want);
        }
    }

    #[test]
    fn jitter_above_one_interval_is_congestion() {
        let t = Instant::now();
        let mut r = Rate::new(60, 25, t);
        r.on_stats(&Stats { jitter_us: 40_000, ..Default::default() }, Transport::Udp, t);
        assert_eq!(r.quality(), 60);
        r.on_stats(&Stats { jitter_us: 40_001, ..Default::default() }, Transport::Udp, t);
        assert_eq!(r.quality(), 48);
    }

    #[test]
    fn tcp_backlog_needs_three_frames_in_a_row() {
        let t = Instant::now();
        let mut r = Rate::new(60, 30, t);
        let s = Stats::default();
        r.on_stripe_backlog(true);
        r.on_stripe_backlog(true);
        r.on_stripe_backlog(false);
        r.on_stripe_backlog(true);
        r.on_stripe_backlog(true);
        r.on_stats(&s, Transport::Tcp, t);
        assert_eq!(r.quality(), 60);
        r.on_stripe_backlog(true);
        r.on_stats(&s, Transport::Tcp, t);
        assert_eq!(r.quality(), 48);
    }

    #[test]
    fn low_configured_quality_is_not_raised_to_floor() {
        let t = Instant::now();
        let mut r = Rate::new(10, 3, t);
        r.on_stats(&lossy(), Transport::Udp, t);
        assert_eq!((r.quality(), r.fps()), (10, 3));
    }
}
