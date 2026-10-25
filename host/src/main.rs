use anyhow::Result;
use clap::{Parser, ValueEnum};
use std::net::IpAddr;
use std::path::PathBuf;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::info;
use twilight_host::input::{NullSink, ScreenSink, Sink, spawn_injector};
use twilight_host::proto::{CONTROL_PORT, EventKind};
use twilight_host::server::{Options, Server};
use twilight_host::source::{Source, spawn_capture};

#[derive(Clone, Copy, ValueEnum)]
enum Capture {
    Screen,
    Synthetic,
}

#[derive(Parser)]
#[command(version, about = "Twilight screen streaming host")]
struct Args {
    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,
    #[arg(long, default_value_t = CONTROL_PORT)]
    base_port: u16,
    #[arg(long, help = "Pairing PIN; random when omitted")]
    pin: Option<String>,
    #[arg(long, value_enum, default_value_t = Capture::Screen)]
    source: Capture,
    #[arg(long, default_value_t = 0)]
    monitor: usize,
    #[arg(long, default_value_t = 60)]
    capture_fps: u32,
    #[arg(long, default_value = "1280x720", value_parser = parse_size)]
    synthetic_size: (u32, u32),
    #[arg(long, default_value_t = 8)]
    max_sessions: usize,
    #[arg(long, default_value_t = 256 * 1024)]
    tcp_sndbuf: usize,
    #[arg(long, help = "Ignore remote input")]
    no_input: bool,
    #[arg(long, help = "Move the cursor to the screen centre, then exit")]
    input_check: bool,
    #[arg(long, help = "Write per-session metrics to this CSV file")]
    metrics: Option<PathBuf>,
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
    let parse = |v: &str| v.parse().map_err(|_| format!("bad number {v}"));
    Ok((parse(w)?, parse(h)?))
}

fn random_pin() -> Result<String> {
    let mut bytes = [0; 4];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("random: {e}"))?;
    Ok(format!("{:06}", u32::from_be_bytes(bytes) % 1_000_000))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "twilight_host=info".into()),
        )
        .init();
    let args = Args::parse();
    let pin = match args.pin {
        Some(pin) => pin,
        None => random_pin()?,
    };
    let stop = CancellationToken::new();
    let (frames, _) = watch::channel(None);
    let (capture_kind, monitor, (width, height)) = (args.source, args.monitor, args.synthetic_size);
    let open = move || match capture_kind {
        Capture::Screen => Source::screen(monitor),
        Capture::Synthetic => Ok(Source::Synthetic { width, height, tick: 0 }),
    };
    let (capture, area) = spawn_capture(open, args.capture_fps, frames.clone(), stop.clone())?;
    if args.input_check {
        ScreenSink::new(area)?.apply(EventKind::MouseMove { x: 32768, y: 32768 })?;
        println!("cursor moved to the centre of {}x{}", area.width, area.height);
        stop.cancel();
        return Ok(());
    }
    let synthetic = matches!(args.source, Capture::Synthetic);
    let no_input = args.no_input || synthetic;
    let input = spawn_injector(move || -> Result<Box<dyn Sink>> {
        if no_input { Ok(Box::new(NullSink)) } else { Ok(Box::new(ScreenSink::new(area)?)) }
    });
    let options = Options {
        bind: args.bind,
        base_port: args.base_port,
        pin: pin.clone(),
        max_sessions: args.max_sessions,
        tcp_sndbuf: args.tcp_sndbuf,
        metrics: args.metrics,
    };
    let server = Server::bind(options, frames, input, stop.clone()).await?;
    let ports = server.ports()?;
    info!("listening on {} control {} video {} input {}", args.bind, ports.control, ports.video, ports.input);
    println!("PIN: {pin}");
    let shutdown = stop.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        info!("shutting down");
        shutdown.cancel();
    });
    server.run().await;
    let _ = capture.join();
    Ok(())
}
