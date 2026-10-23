use crate::fec::Frame;
use crate::sys::unsent_bytes;
use bytes::BytesMut;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

const BATCH_CHUNKS: usize = 16;
const STALE_AFTER: Duration = Duration::from_secs(1);

struct Queued {
    frame: Arc<Frame>,
    enqueued: Instant,
}

pub struct Stripe {
    stream: Arc<TcpStream>,
    slot: Mutex<Option<Queued>>,
    wake: Notify,
    pending: AtomicUsize,
    alive: AtomicBool,
    stop: CancellationToken,
}

impl Stripe {
    fn load(&self) -> usize {
        self.pending.load(Relaxed) + unsent_bytes(&self.stream)
    }

    fn offer(&self, frame: Arc<Frame>) -> bool {
        let size = frame.bytes;
        let old = self.slot.lock().unwrap().replace(Queued { frame, enqueued: Instant::now() });
        self.pending.fetch_add(size, Relaxed);
        if let Some(old) = &old {
            self.pending.fetch_sub(old.frame.bytes, Relaxed);
        }
        self.wake.notify_one();
        old.is_some()
    }

    fn take(&self) -> Option<Queued> {
        self.slot.lock().unwrap().take()
    }

    fn has_waiting(&self) -> bool {
        self.slot.lock().unwrap().is_some()
    }

    fn kill(&self) {
        self.alive.store(false, Relaxed);
        self.stop.cancel();
    }
}

async fn write_all(stream: &TcpStream, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        stream.writable().await?;
        match stream.try_write(buf) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => buf = &buf[n..],
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

async fn send_frame(s: &Stripe, q: &Queued) -> io::Result<()> {
    let mut left = q.frame.bytes;
    let mut out = BytesMut::with_capacity(BATCH_CHUNKS * 1300);
    let result = async {
        for batch in q.frame.data.chunks(BATCH_CHUNKS) {
            if q.enqueued.elapsed() > STALE_AFTER && s.has_waiting() {
                break;
            }
            out.clear();
            let mut size = 0;
            for chunk in batch {
                out.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
                out.extend_from_slice(chunk);
                size += chunk.len();
            }
            write_all(&s.stream, &out).await?;
            s.pending.fetch_sub(size, Relaxed);
            left -= size;
        }
        Ok(())
    }
    .await;
    s.pending.fetch_sub(left, Relaxed);
    result
}

async fn writer(s: Arc<Stripe>) {
    loop {
        let notified = s.wake.notified();
        let Some(q) = s.take() else {
            tokio::select! {
                _ = notified => continue,
                _ = s.stop.cancelled() => return,
            }
        };
        tokio::select! {
            r = send_frame(&s, &q) => if r.is_err() { break },
            _ = s.stop.cancelled() => break,
        }
    }
    s.kill();
}

async fn watcher(s: Arc<Stripe>) {
    let mut sink = [0; 256];
    loop {
        tokio::select! {
            _ = s.stop.cancelled() => return,
            r = s.stream.readable() => {
                if r.is_err() {
                    break;
                }
                match s.stream.try_read(&mut sink) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
            }
        }
    }
    s.kill();
}

pub struct Offer {
    pub backed_up: bool,
}

pub struct Stripes {
    slots: Mutex<Vec<Option<Arc<Stripe>>>>,
    skipped: AtomicU64,
    stop: CancellationToken,
}

impl Stripes {
    pub fn new(count: usize, stop: CancellationToken) -> Self {
        Self {
            slots: Mutex::new(vec![None; count]),
            skipped: AtomicU64::new(0),
            stop,
        }
    }

    pub fn skipped(&self) -> u64 {
        self.skipped.load(Relaxed)
    }

    pub fn alive(&self) -> usize {
        self.live().len()
    }

    fn live(&self) -> Vec<Arc<Stripe>> {
        let slots = self.slots.lock().unwrap();
        slots.iter().flatten().filter(|s| s.alive.load(Relaxed)).cloned().collect()
    }

    pub fn attach(&self, index: usize, stream: TcpStream) -> bool {
        let mut slots = self.slots.lock().unwrap();
        let Some(slot) = slots.get_mut(index) else { return false };
        let stripe = Arc::new(Stripe {
            stream: Arc::new(stream),
            slot: Mutex::new(None),
            wake: Notify::new(),
            pending: AtomicUsize::new(0),
            alive: AtomicBool::new(true),
            stop: self.stop.child_token(),
        });
        if let Some(old) = slot.replace(stripe.clone()) {
            old.kill();
        }
        tokio::spawn(writer(stripe.clone()));
        tokio::spawn(watcher(stripe));
        true
    }

    pub fn offer(&self, frame: Arc<Frame>) -> Option<Offer> {
        let stripes = self.live();
        let (load, best) = stripes.iter().map(|s| (s.load(), s)).min_by_key(|(load, _)| *load)?;
        let backed_up = load > frame.bytes;
        if best.offer(frame) {
            self.skipped.fetch_add(1, Relaxed);
        }
        Some(Offer { backed_up })
    }
}
