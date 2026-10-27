# Twilight host

Implements `spec/PROTOCOL.md` in full on Linux and Windows.

## Build

```sh
cargo build --release
cargo build --release --target x86_64-pc-windows-gnu
```

Native builds need `cmake` and `nasm` for libjpeg-turbo. Linux also needs the X11, Wayland and
PipeWire development libraries that `xcap` and `enigo` use. The Windows target builds from
Linux with MinGW, and the resulting `.exe` needs no install.

## Run

```sh
twilight-host                               # prints a random PIN
twilight-host --pin 123456 --monitor 1
twilight-host --source synthetic            # no display needed
twilight-host --metrics run.csv             # per-STATS and migration log
twilight-host --input-check                 # moves the cursor to prove injection works
```

Allow TCP 48100 to 48102 and UDP 48101 to 48102 through the host firewall.

## Layout

| File | Role |
|---|---|
| `proto.rs` | Wire formats |
| `fec.rs` | Chunking and XOR parity |
| `session.rs` | Control state machine, no I/O |
| `selector.rs` | Transport choice and migration policy, no I/O |
| `rate.rs` | AIMD quality control, no I/O |
| `source.rs` | Capture, scaling, JPEG encoding |
| `stripes.rs` | Striped TCP with latest-frame-wins |
| `input.rs` | Input deduplication and injection |
| `sys.rs` | Per-OS send queue query |
| `server.rs` | Sockets and tasks |

## Tests

```sh
cargo test
TWILIGHT_REGEN=1 cargo test --test vectors    # only after a deliberate wire format change
```

`tests/e2e.rs` drives the host over real sockets. `tests/vectors.rs` holds it to the
shared vectors.

## Known limits

- Linux on Wayland captures at about 11 fps, because `xcap` takes one screenshot per frame.
- The first Wayland capture can take 5 s.
- Windows reports bytes in flight, not bytes unsent, so least-loaded stripe choice is
  coarser there.
- There is no encryption. See section 11 of the spec.
