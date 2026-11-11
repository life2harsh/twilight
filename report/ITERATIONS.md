# Changes from the synopsis

Source material for the final report. Each change was forced by analysis or by a measurement.

| Synopsis | Now | Why |
|---|---|---|
| Migrate on 15% loss or 500 ms silence | Also on frame completion under 50% for two windows | At 10% loss only 1.4% of frames survive, so the 15% rule fires after the stream is already dead |
| Host detects UDP silence | Client detects it and reports in STATS | The sender cannot see silence |
| Parity over consecutive chunks | Parity over `i mod G`, sent after the data | Short bursts now cost each group at most one chunk |
| XOR assumes equal lengths | Parity carries `length_xor` | Restores the exact length of a short last chunk |
| Token authenticates data connections | Token also in every probe and input packet | UDP has no connection to authenticate, so anyone on the LAN could inject keystrokes |
| PIN sent in PAIR | HMAC of the PIN over two nonces | The PIN never crosses the wire |
| KEYFRAME_REQ recovers the stream | Reserved | Every JPEG stands alone |
| Latest frame wins always | A started frame finishes, unless queued over 1 s | See below |
| `SIOCOUTQ` | Plus `SIO_TCP_INFO` on Windows | `SIOCOUTQ` is Linux-only and the project must run on Windows |
| Stale frame after two intervals | UDP only; 1 s on TCP | TCP delivers late, not never |
| New | Frame at least every 200 ms | An idle desktop must not look like a blocked path |
| New | UDP pacing over half a frame interval | RFC 8085; a burst overflows receive buffers |
| New | PIN lockout, held keys released at session end | Failure handling |

## Measured: the stale rule starved TCP

The first TCP binding abandoned a started frame after four frame intervals. Unit tests passed.
Under `netem` at 5% loss it delivered 0.4 fps, because almost no frame finished within 133 ms.
Changing the limit to 1 s raised this to 4.4 fps. Only the network testbed found this.

## Results

720p synthetic source, 30 fps and quality 60 requested, full client with FEC, `testbed/lab.sh`,
12 s runs. Single runs; repeat each three times for the report's graphs. Columns are the
received fps, then the share of frames completed in brackets.

| Condition | UDP + FEC | TCP, 1 connection | TCP, 4 stripes |
|---|---|---|---|
| No impairment | 29.9 (100%) | 30.1 (100%) | 30.1 (100%) |
| 1% loss, 20 ms | 29.3 (97%) | 12.3 (100%) | 28.7 (100%) |
| 5% loss, 20 ms | 6.1 (58%) | 5.2 (100%) | 9.5 (100%) |
| 10% loss, 20 ms | 2.8 (28%) | 2.2 (92%) | 6.0 (92%) |
| 20 Mbit/s cap, 10 ms | 25.5 (95%) | 28.7 (100%) | 24.6 (100%) |

Loss measured by the client matched `netem`: 1.0%, 5.8% and 10.7%.

- **FEC.** At 1% loss UDP stays at 29 fps. The starter client, without FEC, fell to 12.9 fps
  on one TCP connection under the same loss. One parity chunk per four data chunks recovers
  almost every frame.
- **Striping.** Four stripes beat one connection under random loss: at 1% loss, 28.7 fps
  against 12.3. A single stream stalls on every retransmission, but stripes stall
  independently. Under a pure bandwidth cap, one connection wins, because four flows compete
  with each other.
- **Heavy loss.** At 5% and above, UDP completes too few frames. In auto mode the host leaves
  UDP for TCP, as the completion rule in `spec/DESIGN.md` predicts.

**Migration.** With UDP blocked mid-stream, the client reported silence 499 ms after the
block, and the host moved to TCP on that STATS. The starter client took about 0.85 s, because
it waited for its 500 ms timer. With UDP unblocked, the session returned at the next probe
burst plus 3 s of clean probes, and played at 30 fps throughout.

## Client changes found by testing

| Problem | Fix |
|---|---|
| Opening the window stalled Python while UDP was flowing: 6.6% loss in the first window, and sometimes a needless move to TCP on localhost | Open the window before probing and START |
| Counting loss when a frame completes undercounts parity that arrives later | Keep completed frames until their stale time, then count |
| Overtaken frames on striped TCP looked like failures | Count them separately, on TCP only. On UDP an overtaken frame is real loss |
| OpenCV windows report no key release and few special keys | pygame window. OpenCV and numpy are no longer needed |

## Found on real hardware

The first two-laptop run, over Wi-Fi with real screen capture, fell to quality 20 and 5 fps
with no loss at all. The client reported 14 s of jitter. The cause was in the host:

- The first frame of a session carried the capture time of the frame grabbed when the host
  started, which was 4 minutes old.
- Re-encoding an unchanged screen at a new quality reused that frame's old capture time.

The jitter alarm lowered quality, the lower quality forced another re-encode, and that
re-encode raised another false alarm. Now only a genuinely new capture carries its capture
time, and everything else carries the send time, as spec section 4 says. After the fix:
quality 60, 0% loss, about 10 ms jitter and a 10 to 20 ms round trip. The synthetic source
cannot show this bug, because its frames are always new.
