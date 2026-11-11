use crate::proto::{EventKind, InputEvent};
use crate::source::Geometry;
use anyhow::Result;
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc;
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Held {
    Key(u32),
    Button(u8),
}

pub trait Sink {
    fn apply(&mut self, kind: EventKind) -> Result<()>;
}

pub struct NullSink;

impl Sink for NullSink {
    fn apply(&mut self, _: EventKind) -> Result<()> {
        Ok(())
    }
}

pub struct ScreenSink {
    enigo: Enigo,
    area: Geometry,
}

impl ScreenSink {
    pub fn new(area: Geometry) -> Result<Self> {
        let mut settings = Settings::default();
        if cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some() {
            settings.x11_display = Some(":wayland-only".into());
        }
        Ok(Self { enigo: Enigo::new(&settings)?, area })
    }
}

pub fn to_screen(v: u16, origin: i32, size: u32) -> i32 {
    origin + (v as i64 * size.saturating_sub(1) as i64 / 65535) as i32
}

pub fn keysym_to_key(sym: u32) -> Option<Key> {
    let named = match sym {
        0xff08 => Key::Backspace,
        0xff09 => Key::Tab,
        0xff0d | 0xff8d => Key::Return,
        0xff1b => Key::Escape,
        0xff50 => Key::Home,
        0xff51 => Key::LeftArrow,
        0xff52 => Key::UpArrow,
        0xff53 => Key::RightArrow,
        0xff54 => Key::DownArrow,
        0xff55 => Key::PageUp,
        0xff56 => Key::PageDown,
        0xff57 => Key::End,
        0xffbe..=0xffc9 => return function_key(sym - 0xffbe),
        0xffe1 => Key::LShift,
        0xffe2 => Key::RShift,
        0xffe3 => Key::LControl,
        0xffe4 => Key::RControl,
        0xffe5 => Key::CapsLock,
        0xffe9 | 0xffea => Key::Alt,
        0xffeb | 0xffec => Key::Meta,
        0xffff => Key::Delete,
        0x20..=0x7e | 0xa0..=0xff => return char::from_u32(sym).map(Key::Unicode),
        0x0100_0100..=0x0110_ffff => return char::from_u32(sym - 0x0100_0000).map(Key::Unicode),
        _ => return None,
    };
    Some(named)
}

fn function_key(n: u32) -> Option<Key> {
    [
        Key::F1, Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9, Key::F10, Key::F11,
        Key::F12,
    ]
    .get(n as usize)
    .copied()
}

fn direction(down: bool) -> Direction {
    if down { Direction::Press } else { Direction::Release }
}

impl Sink for ScreenSink {
    fn apply(&mut self, kind: EventKind) -> Result<()> {
        match kind {
            EventKind::MouseMove { x, y } => self.enigo.move_mouse(
                to_screen(x, self.area.x, self.area.width),
                to_screen(y, self.area.y, self.area.height),
                Coordinate::Abs,
            )?,
            EventKind::MouseButton { button, down } => {
                let button = match button {
                    1 => Button::Left,
                    2 => Button::Middle,
                    _ => Button::Right,
                };
                self.enigo.button(button, direction(down))?
            }
            EventKind::Scroll { dx, dy } => {
                if dx != 0 {
                    self.enigo.scroll(dx as i32, Axis::Horizontal)?;
                }
                if dy != 0 {
                    self.enigo.scroll(dy as i32, Axis::Vertical)?;
                }
            }
            EventKind::Key { keysym, down } => {
                if let Some(key) = keysym_to_key(keysym) {
                    self.enigo.key(key, direction(down))?;
                }
            }
        }
        Ok(())
    }
}

pub enum Cmd {
    Event(u64, EventKind),
    Release(u64),
}

#[derive(Default)]
pub struct Tracker {
    held: HashMap<u64, HashSet<Held>>,
}

impl Tracker {
    pub fn handle(&mut self, cmd: Cmd, sink: &mut dyn Sink) {
        let result = match cmd {
            Cmd::Event(session, kind) => {
                let entry = match kind {
                    EventKind::Key { keysym, down } => Some((Held::Key(keysym), down)),
                    EventKind::MouseButton { button, down } => Some((Held::Button(button), down)),
                    _ => None,
                };
                if let Some((held, down)) = entry {
                    let set = self.held.entry(session).or_default();
                    if down {
                        set.insert(held);
                    } else {
                        set.remove(&held);
                    }
                }
                sink.apply(kind)
            }
            Cmd::Release(session) => self.held.remove(&session).into_iter().flatten().try_for_each(|held| {
                sink.apply(match held {
                    Held::Key(keysym) => EventKind::Key { keysym, down: false },
                    Held::Button(button) => EventKind::MouseButton { button, down: false },
                })
            }),
        };
        if let Err(e) = result {
            warn!("input injection failed: {e:#}");
        }
    }
}

pub fn spawn_injector(make: impl FnOnce() -> Result<Box<dyn Sink>> + Send + 'static) -> mpsc::Sender<Cmd> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut sink = make().unwrap_or_else(|e| {
            warn!("input injection disabled: {e:#}");
            Box::new(NullSink)
        });
        let mut tracker = Tracker::default();
        rx.iter().for_each(|cmd| tracker.handle(cmd, sink.as_mut()));
    });
    tx
}

#[derive(Default)]
pub struct Dedup {
    newest: u32,
}

impl Dedup {
    pub fn accept(&mut self, events: &[InputEvent]) -> Vec<EventKind> {
        let mut out = Vec::new();
        for e in events {
            if e.seq > self.newest {
                self.newest = e.seq;
                out.push(e.kind);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Record(Vec<EventKind>);

    impl Sink for Record {
        fn apply(&mut self, kind: EventKind) -> Result<()> {
            self.0.push(kind);
            Ok(())
        }
    }

    fn key(seq: u32, keysym: u32) -> InputEvent {
        InputEvent { seq, kind: EventKind::Key { keysym, down: true } }
    }

    #[test]
    fn dedup_applies_each_event_once_in_order() {
        let mut d = Dedup::default();
        assert_eq!(d.accept(&[key(1, 65)]).len(), 1);
        assert_eq!(d.accept(&[key(1, 65), key(2, 66)]).len(), 1);
        assert_eq!(d.accept(&[key(1, 65), key(2, 66), key(3, 67)]).len(), 1);
        assert_eq!(d.accept(&[key(2, 66)]).len(), 0);
    }

    #[test]
    fn lost_datagram_is_recovered_from_redundancy() {
        let mut d = Dedup::default();
        d.accept(&[key(1, 65)]);
        assert_eq!(d.accept(&[key(1, 65), key(2, 66), key(3, 67)]).len(), 2);
    }

    #[test]
    fn release_lifts_everything_a_session_holds() {
        let mut t = Tracker::default();
        let mut r = Record::default();
        let down = EventKind::Key { keysym: 0xffe1, down: true };
        let click = EventKind::MouseButton { button: 1, down: true };
        let other = EventKind::Key { keysym: 97, down: true };
        t.handle(Cmd::Event(1, down), &mut r);
        t.handle(Cmd::Event(1, click), &mut r);
        t.handle(Cmd::Event(2, other), &mut r);
        r.0.clear();
        t.handle(Cmd::Release(1), &mut r);
        assert_eq!(r.0.len(), 2);
        assert!(r.0.contains(&EventKind::Key { keysym: 0xffe1, down: false }));
        assert!(r.0.contains(&EventKind::MouseButton { button: 1, down: false }));
    }

    #[test]
    fn released_keys_are_not_lifted_twice() {
        let mut t = Tracker::default();
        let mut r = Record::default();
        t.handle(Cmd::Event(1, EventKind::Key { keysym: 97, down: true }), &mut r);
        t.handle(Cmd::Event(1, EventKind::Key { keysym: 97, down: false }), &mut r);
        r.0.clear();
        t.handle(Cmd::Release(1), &mut r);
        assert!(r.0.is_empty());
    }

    #[test]
    fn screen_mapping_covers_the_monitor() {
        assert_eq!(to_screen(0, 100, 1920), 100);
        assert_eq!(to_screen(65535, 100, 1920), 2019);
    }

    #[test]
    fn keysym_table() {
        assert_eq!(keysym_to_key(0x61), Some(Key::Unicode('a')));
        assert_eq!(keysym_to_key(0xff0d), Some(Key::Return));
        assert_eq!(keysym_to_key(0xffc9), Some(Key::F12));
        assert_eq!(keysym_to_key(0x0100_20ac), Some(Key::Unicode('€')));
        assert_eq!(keysym_to_key(0xdead), None);
    }
}
