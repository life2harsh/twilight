use anyhow::{Context, Result, anyhow};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images::{Image, ImageRef}};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use turbojpeg::{Compressor, PixelFormat, Subsamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub struct Raw {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub captured_at: Instant,
}

pub enum Source {
    Screen(xcap::Monitor),
    #[cfg(target_os = "linux")]
    Screencopy(libwayshot_xcap::WayshotConnection, libwayshot_xcap::output::OutputInfo),
    Synthetic { width: u32, height: u32, tick: u32 },
}

impl Source {
    pub fn screen(index: usize) -> Result<Self> {
        #[cfg(target_os = "linux")]
        if std::env::var_os("WAYLAND_DISPLAY").is_some()
            && let Ok(conn) = libwayshot_xcap::WayshotConnection::new()
            && let Some(output) = conn.get_all_outputs().get(index).cloned()
            && conn.screenshot_single_output(&output, true).is_ok()
        {
            return Ok(Self::Screencopy(conn, output));
        }
        let monitors = xcap::Monitor::all().context("listing monitors")?;
        let monitor = monitors.into_iter().nth(index).ok_or_else(|| anyhow!("no monitor {index}"))?;
        Ok(Self::Screen(monitor))
    }

    pub fn geometry(&self) -> Result<Geometry> {
        Ok(match self {
            Self::Screen(m) => Geometry { x: m.x()?, y: m.y()?, width: m.width()?, height: m.height()? },
            #[cfg(target_os = "linux")]
            Self::Screencopy(_, o) => {
                let r = o.logical_region.inner;
                Geometry { x: r.position.x, y: r.position.y, width: r.size.width, height: r.size.height }
            }
            Self::Synthetic { width, height, .. } => Geometry { x: 0, y: 0, width: *width, height: *height },
        })
    }

    fn grab(&mut self) -> Result<(u32, u32, Vec<u8>)> {
        match self {
            Self::Screen(m) => {
                let image = m.capture_image()?;
                Ok((image.width(), image.height(), image.into_raw()))
            }
            #[cfg(target_os = "linux")]
            Self::Screencopy(conn, output) => {
                let image = conn.screenshot_single_output(output, true)?.into_rgba8();
                Ok((image.width(), image.height(), image.into_raw()))
            }
            Self::Synthetic { width, height, tick } => {
                *tick = tick.wrapping_add(1);
                Ok((*width, *height, synthetic(*width, *height, *tick)))
            }
        }
    }
}

fn synthetic(width: u32, height: u32, tick: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((width * height * 4) as usize);
    let bar = tick.wrapping_mul(7) % width.max(1);
    for y in 0..height {
        for x in 0..width {
            let block = (x / 32).wrapping_mul(2_654_435_761) ^ (y / 32).wrapping_mul(40_503);
            let noise = x.wrapping_mul(73).wrapping_add(y.wrapping_mul(151)).wrapping_add(tick) % 23;
            let v = (block >> 24).wrapping_add(noise) as u8;
            if x.abs_diff(bar) < 40 {
                out.extend_from_slice(&[255, tick as u8, 0, 255]);
            } else {
                out.extend_from_slice(&[v, v.wrapping_mul(3), (y * 255 / height.max(1)) as u8, 255]);
            }
        }
    }
    out
}

pub fn spawn_capture(
    make: impl FnOnce() -> Result<Source> + Send + 'static,
    fps: u32,
    tx: watch::Sender<Option<Arc<Raw>>>,
    stop: CancellationToken,
) -> Result<(std::thread::JoinHandle<()>, Geometry)> {
    let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let (ready_tx, ready_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let opened = make().and_then(|s| Ok((s.geometry()?, s)));
        let mut source = match opened {
            Ok((area, source)) => {
                let _ = ready_tx.send(Ok(area));
                source
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        let mut first = true;
        let mut prev: Option<Arc<Raw>> = None;
        while !stop.is_cancelled() {
            if !first && tx.receiver_count() == 0 {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            first = false;
            let started = Instant::now();
            match source.grab() {
                Ok((width, height, rgba)) => {
                    if prev.as_ref().is_none_or(|p| p.rgba != rgba) {
                        let raw = Arc::new(Raw { width, height, rgba, captured_at: started });
                        prev = Some(raw.clone());
                        tx.send_replace(Some(raw));
                    }
                }
                Err(e) => {
                    warn!("capture failed: {e:#}");
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
            std::thread::sleep(interval.saturating_sub(started.elapsed()));
        }
    });
    let area = ready_rx.recv().context("capture thread exited")??;
    Ok((handle, area))
}

pub fn fit(src_w: u32, src_h: u32, max_w: u16, max_h: u16) -> (u32, u32) {
    let bound_w = if max_w == 0 { src_w } else { src_w.min(max_w as u32) };
    let bound_h = if max_h == 0 { src_h } else { src_h.min(max_h as u32) };
    let scale = (bound_w as f64 / src_w as f64).min(bound_h as f64 / src_h as f64);
    (((src_w as f64 * scale).round() as u32).max(1), ((src_h as f64 * scale).round() as u32).max(1))
}

pub struct Encoder {
    jpeg: Compressor,
    resizer: Resizer,
    max_w: u16,
    max_h: u16,
    quality: u8,
}

impl Encoder {
    pub fn new(max_w: u16, max_h: u16) -> Result<Self> {
        let mut jpeg = Compressor::new()?;
        jpeg.set_subsamp(Subsamp::Sub2x2)?;
        Ok(Self { jpeg, resizer: Resizer::new(), max_w, max_h, quality: 0 })
    }

    pub fn encode(&mut self, raw: &Raw, quality: u8) -> Result<Vec<u8>> {
        if quality != self.quality {
            self.jpeg.set_quality(quality as i32)?;
            self.quality = quality;
        }
        let (w, h) = fit(raw.width, raw.height, self.max_w, self.max_h);
        let scaled;
        let pixels = if (w, h) == (raw.width, raw.height) {
            &raw.rgba
        } else {
            let src = ImageRef::new(raw.width, raw.height, &raw.rgba, PixelType::U8x4)?;
            let mut dst = Image::new(w, h, PixelType::U8x4);
            let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
            self.resizer.resize(&src, &mut dst, &options)?;
            scaled = dst.into_vec();
            &scaled
        };
        let image = turbojpeg::Image {
            pixels: pixels.as_slice(),
            width: w as usize,
            pitch: w as usize * 4,
            height: h as usize,
            format: PixelFormat::RGBA,
        };
        Ok(self.jpeg.compress_to_vec(image)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(width: u32, height: u32) -> Raw {
        Raw { width, height, rgba: synthetic(width, height, 1), captured_at: Instant::now() }
    }

    #[test]
    fn fit_keeps_aspect_and_never_upscales() {
        assert_eq!(fit(1920, 1080, 0, 0), (1920, 1080));
        assert_eq!(fit(1920, 1080, 1280, 720), (1280, 720));
        assert_eq!(fit(1920, 1080, 1280, 0), (1280, 720));
        assert_eq!(fit(1920, 1080, 0, 540), (960, 540));
        assert_eq!(fit(1920, 1200, 1280, 720), (1152, 720));
        assert_eq!(fit(800, 600, 1920, 1080), (800, 600));
    }

    #[test]
    fn encodes_decodable_jpeg_at_requested_size() {
        for (max_w, max_h, want) in [(0, 0, (640, 360)), (320, 180, (320, 180))] {
            let out = Encoder::new(max_w, max_h).unwrap().encode(&raw(640, 360), 60).unwrap();
            let header = turbojpeg::read_header(&out).unwrap();
            assert_eq!((header.width as u32, header.height as u32), want);
        }
    }

    #[test]
    fn lower_quality_is_smaller() {
        let mut e = Encoder::new(0, 0).unwrap();
        let r = raw(640, 360);
        assert!(e.encode(&r, 20).unwrap().len() < e.encode(&r, 90).unwrap().len());
    }

    #[test]
    fn synthetic_frames_change() {
        assert_ne!(synthetic(64, 64, 1), synthetic(64, 64, 2));
    }
}
