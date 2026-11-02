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

## Preliminary results

720p synthetic source, 30 fps target, starter client, `testbed/lab.sh`. These are single short
runs without FEC on the client, so use them as a guide and rerun for the report.

| Condition | 1 connection | 4 stripes |
|---|---|---|
| No impairment | 30.0 fps | 30.0 fps |
| 1% loss, 20 ms | 12.9 fps | 19.6 fps |
| 5% loss, 20 ms | 4.4 fps | 6.1 fps |
| 10% loss, 20 ms | 3.3 fps | 3.9 fps |
| 20 Mbit/s cap, 10 ms | 28.6 fps | 21.0 fps |

Striping helps under random loss and costs throughput under a pure bandwidth cap, where four
flows compete with each other.

UDP blocked mid-stream migrated to TCP in roughly 0.85 s, estimated from the run log. The starter
client only reports silence on its 500 ms timer; reporting at once (client task 7) should bring
this towards the 0.5 s silence limit. Measure this properly for the report.
