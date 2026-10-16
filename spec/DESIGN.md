# Design notes

The reasoning behind the protocol rules. `PROTOCOL.md` says what; this says why.

## Whole-frame delivery fails early

A JPEG frame is useless unless every chunk arrives. For 200 data chunks, 4-of-5 XOR parity and
independent loss `p`:

```
group_ok = (1-p)^5 + 5p(1-p)^4
frame_ok = group_ok ^ 50
```

| p | frame_ok | without FEC |
|---|---|---|
| 2% | 83% | 1.8% |
| 5% | 32% | ~0 |
| 10% | 1.4% | ~0 |

The stream is dead at 10% loss, while the synopsis threshold only fires at 15%. Migration
therefore also watches frame completion. AIMD helps too: lower quality means fewer chunks,
fewer groups, and a higher frame survival rate.

## Why parity is interleaved

Wi-Fi loss comes in bursts. With consecutive groups, a burst of two kills a group. With
`i mod G`, a burst shorter than `G` costs each group at most one chunk, which parity repairs.

## Why parity comes after the data

The frame can complete without waiting for parity when nothing was lost, and the client
needs no knowledge of group layout until a chunk is actually missing.

## Why the client always listens on both paths

Migration then has no handshake on the data path. A chunk is a chunk. MIGRATE exists to
tell the client where to send input, and to make the switch visible in a capture.

## Why the host decides and the client reports

The sender cannot observe silence and the receiver cannot observe send queues. Giving the
decision to one side, fed by STATS, means there is only one state machine to get right.

## Why STATS and MIGRATE travel on control TCP

If they shared the failing path, the failure would silence the report of the failure.

## Why a started TCP frame is finished

The synopsis said latest frame wins. Applied to a frame already being written, that starves
a slow link: every frame is abandoned before it completes. A first version allowed abandoning
after four frame intervals and fell to 0.4 fps at 5% loss, because a lossy frame legitimately
takes longer than 133 ms. The limit is now an absolute 1 s, which only catches real stalls.

## Why the static-screen resend exists

If an unchanged desktop stopped frames, a client could not tell an idle screen from a
blocked path. Sending at least every 200 ms keeps the 500 ms silence rule valid, and also
heals a lost frame on a static screen.

## Portability of the send-queue measure

`SIOCOUTQ` is Linux-only. Windows has no exact equivalent, so `SIO_TCP_INFO.BytesInFlight`
is used, and application pending bytes are always counted. Least-loaded selection works on
both; only its precision differs.
