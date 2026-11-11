# Twilight client

Implements `spec/PROTOCOL.md` in full. Receives video on UDP and TCP at once, repairs loss with
FEC, reports statistics, follows migration both ways and sends mouse and keyboard input.

## Run

```sh
uv sync
uv run client.py HOST_IP --pin 123456
uv run client.py HOST_IP --pin 123456 --mode 2 --stripes 4 --csv run.csv
uv run pytest
```

| Flag | Meaning |
|---|---|
| `--mode` | 0 auto, 1 UDP only, 2 TCP only |
| `--stripes` | TCP connections, 1 to 8 |
| `--width --height --fps --quality` | Requested stream; the host clamps and reports what it uses |
| `--no-fec` | Ask the host not to send parity |
| `--no-input` | View only |
| `--csv FILE` | One row per STATS window |
| `--headless --seconds N` | No window, stop after N seconds (testbed) |

Run with no host to get a connect window, which remembers the last IP. Close the video window
to return to it. While the video window has focus, mouse and keys go to the host. Use `--no-input`
when host and client run on the same machine, or the client will drive its own cursor.

Without a host machine, start one locally: `twilight-host --source synthetic --pin 123456`.
The synthetic source ignores input.

## Files

| File | What |
|---|---|
| `protocol.py` | Byte layouts, FEC repair, input encoding |
| `client.py` | `Reassembler` and the network threads |
| `viewer.py` | pygame window, overlay, mouse and key capture |
| `test_*.py` | Tests against `spec/vectors.json` and synthetic chunks |

## How it works

**Threads.** Each exists because it blocks on something different.

| Thread | Blocks on | Does |
|---|---|---|
| main | the window | Decodes and shows the newest frame, captures input |
| `udp_loop` | UDP socket | Feeds chunks to the `Reassembler`, records probe echoes |
| `stripe_loop` x N | one TCP stripe | Reads `length:u16` then a chunk, feeds the `Reassembler` |
| `control_loop` | control socket | Answers PING, takes PONG, follows MIGRATE |
| `ping_loop` | 1 s timer | Sends PING for the clock offset |
| `stats_loop` | 10 ms tick | Sends STATS every 500 ms, or at once on UDP silence |
| `return_loop` | timers | On TCP, probes UDP and reports `udp_clean_ms` |

**Reassembler** (spec 4, 6). One object, fed by every receiver, so nothing else needs to know
the transport. Frames are keyed by `frame_id` and compared with serial arithmetic, so the
wrap at 2^32 works. A frame is complete when all `D` data chunks are present. When a group
is missing one chunk and its parity has arrived, `protocol.repair` rebuilds it by XOR. Only
the newest complete frame is kept; one replaced before display counts as skipped.

A frame stays in the table after completing until its stale time, 2 frame intervals on UDP or
1 s on TCP. Late parity is still counted, so loss is not overstated. When it leaves:

- UDP frames add their received and expected chunk counts, giving `loss_permille`.
- An incomplete frame counts as dropped, giving `completion_permille`. On TCP an incomplete
  frame older than one already shown counts as overtaken instead, since another stripe
  simply finished first.

**Statistics** (spec 8). Jitter is RFC 3550 over the first chunk of each frame. Latency is
`now + offset - timestamp` after the frame is drawn. The offset comes from the lowest-RTT
PONG of the last 30 (spec 3.2).

**Silence** (spec 7.2). `stats_loop` checks every 10 ms, so a STATS with `UDP_SILENT` goes
out about 500 ms after the last UDP chunk rather than up to 1 s.

**Return to UDP** (spec 7.2). On TCP, 10 probes every 10 s. If 8 echo within 300 ms, it
probes 10 a second and reports how long at least 9 of each burst have echoed. One bad burst
resets it to 0. The host decides when to move back.

**Input** (spec 10). Each event gets the next `seq`. A packet carries it plus the two before
it, so one lost datagram loses nothing. It goes on UDP or the TCP input connection, matching
the current transport. On MIGRATE the last two events are resent on the new path.

## Measured

See `report/ITERATIONS.md`. Run `testbed/lab.sh`, which also writes `results/NAME.client.csv`.

## Known limits

- Keys are sent by keysym of the unshifted key. Shift is sent as its own key, so `A` arrives
  as Shift plus `a`. Check capitals on the demo host. Dead keys and IME input are not
  supported.
- The first STATS window after start can show high jitter. The first chunk's transit time is
  an outlier, and RFC 3550 smoothing takes about a second to settle.
- Pure Python handles the 720p synthetic stream at 30 fps on a laptop. At higher resolution or
  fps, `udp_loop` is the bottleneck.
