# Twilight Protocol, version 1

Normative for host and client. "MUST" means interoperability or safety depends on it.
`spec/vectors.json` holds byte-exact examples; every implementation MUST pass them.

## 1. Conventions

- All multi-byte integers are big-endian.
- `MAGIC` = `0x544C` ("TL"). `VERSION` = `1`.
- Times are microseconds on the sender's monotonic clock (`u64`).
- A receiver MUST drop any packet with a wrong magic, wrong version, or wrong length
  without changing state. On a TCP data connection it MUST also close the connection.

## 2. Channels

| Channel | Port | Protocol | Direction |
|---|---|---|---|
| Control | 48100 | TCP | both |
| Video | 48101 | UDP and TCP | host to client (probes: both) |
| Input | 48102 | UDP and TCP | client to host |

The client keeps both video paths open for the whole session and MUST accept chunks from
either at any time. That is what makes migration lossless for the session.

## 3. Control channel

Each message is `length:u32 | type:u8 | payload`. `length` counts the payload only and MUST
NOT exceed 1024; a larger value is a protocol error.

| Type | Name | Dir | Payload |
|---|---|---|---|
| 1 | HELLO | C→H | `magic:u16 version:u8` |
| 2 | HELLO_OK | H→C | `version:u8 host_nonce:[16]` |
| 3 | PAIR | C→H | `client_nonce:[16] mac:[32]` |
| 4 | PAIR_OK | H→C | `token:[16]` |
| 5 | PAIR_FAIL | H→C | `attempts_left:u8` |
| 6 | CONFIG | C→H | `width:u16 height:u16 fps:u8 quality:u8 stripes:u8 mode:u8 fec:u8` |
| 7 | CONFIG_OK | H→C | same layout, the values actually in force |
| 8 | PROBE_RESULT | C→H | `sent:u8 echoed:u8` |
| 9 | START | H→C | `transport:u8 first_frame_id:u32` |
| 10 | STATS | C→H | see section 8 |
| 11 | MIGRATE | H→C | `transport:u8 next_frame_id:u32` |
| 12 | PING | both | `seq:u32 t1:u64` |
| 13 | PONG | both | `seq:u32 t1:u64 t2:u64` |
| 14 | KEYFRAME_REQ | C→H | empty |
| 15 | BYE | both | `reason:u8` |

- `mac` = HMAC-SHA256(key = PIN as ASCII, msg = `"twilight-pair-v1" | host_nonce | client_nonce`).
- `width`/`height` 0 means native size. The host never upscales and keeps the aspect ratio.
- `fps` 1 to 120, `quality` 1 to 100, `stripes` 1 to 8. The host clamps and reports the result.
- `mode`: 0 auto, 1 UDP only, 2 TCP only. `fec`: 0 off, 1 on.
- `transport`: 0 UDP, 1 TCP.
- BYE `reason`: 0 normal, 1 version mismatch, 2 auth failed, 3 protocol error, 4 timeout,
  5 host full, 6 no usable path.
- KEYFRAME_REQ is reserved. Every JPEG frame stands alone, so a host MAY ignore it.

### 3.1 Session state machine

```
CONNECTED  --HELLO/HELLO_OK-->          HELLOED
HELLOED    --PAIR/PAIR_OK-->            PAIRED
PAIRED     --CONFIG/CONFIG_OK-->        PROBING
PROBING    --PROBE_RESULT/START-->      STREAMING
any        --BYE or timeout-->          CLOSED
```

A message that is not valid in the current state is a protocol error: the host sends
BYE(3) and closes. PING, PONG and BYE are valid in every state after HELLO.

- PAIR_FAIL allows three attempts per connection. After the third the host sends BYE(2).
- Five failures from one IP address lock that address out for 60 s (BYE(2) after HELLO).
- Each step before STREAMING MUST finish within 10 s, or the host sends BYE(4).
- Either side sends PING every 1 s once PAIRED. A receiver answers with PONG, copying `seq`
  and `t1` and setting `t2` to its own clock. Three seconds without a PONG closes the
  session with BYE(4).
- Losing the control connection ends the session and invalidates its token. A client
  reconnects from HELLO.

### 3.2 Clock offset

On PONG the pinger takes `t3` = now, then `rtt = t3 - t1` and
`offset = t2 - (t1 + t3) / 2`. The client SHOULD keep the offset from the lowest-RTT sample
of the last 30.

Displayed latency = `client_now + offset - chunk.timestamp_us`.

## 4. Frame layer

A frame is one JPEG image. It is split into `D = ceil(len / 1200)` data chunks. Each chunk is
a header plus up to 1200 payload bytes:

| Offset | Field | Type |
|---|---|---|
| 0 | magic | u16 |
| 2 | version | u8 |
| 3 | flags | u8 |
| 4 | frame_id | u32 |
| 8 | chunk_idx | u16 |
| 10 | chunk_total | u16 |
| 12 | timestamp_us | u64 |

Flags: `0x01` KEYFRAME, `0x02` PARITY, `0x04` LAST, `0x80` PROBE (section 6.1).

- `frame_id` starts at `first_frame_id` and increases by one per frame for the whole
  session, across migrations. It wraps at 2^32 (serial arithmetic).
- `chunk_total` is always `D`, in data and parity chunks alike.
- `chunk_idx` is `0..D-1` on data chunks. LAST is set on data chunk `D-1`.
- All chunks of one frame carry the same `timestamp_us`: the capture time, or the send time
  for a resend of an unchanged screen.
- A host MUST drop a frame that needs more than 65535 chunks.
- KEYFRAME is set on every frame in version 1.

The client reassembles by `frame_id`, independent of transport, and displays only the newest
complete frame. It MUST discard anything older than the last displayed frame.

The host MUST emit a frame at least every 200 ms during STREAMING, resending the last image
under a new `frame_id` if the screen has not changed. This keeps silence meaningful.

## 5. TCP binding

Each chunk is prefixed with `length:u16`, the size of header plus payload.

### 5.1 Attach

A data connection opens by sending, within 2 s:

`magic:u16 version:u8 kind:u8 stripe:u8 reserved:u8(0) token:[16]` (22 bytes)

`kind` 0 is video on port 48101, 1 is input on port 48102. `stripe` is `0..stripes-1` for
video and 0 for input. The host closes the connection silently on a bad token or a bad
index, and there is no reply. Attaching an index already in use replaces the old connection.
A client MAY re-attach at any time during the session.

### 5.2 Sending rules

- `TCP_NODELAY` MUST be set and the send buffer MUST be bounded.
- Each stripe holds one frame being written and at most one waiting. A new frame replaces a
  waiting frame; replaced frames are counted as skipped.
- A frame goes whole to the stripe with the least unsent data. The measure is application
  bytes pending plus kernel unsent bytes where the OS reports them: `SIOCOUTQ` on Linux,
  `SIO_TCP_INFO.BytesInFlight` on Windows.
- A frame already started is finished, because abandoning it would starve any link where a
  frame takes longer than a frame interval. The exception: if it has been queued for over
  1 s and a newer frame is waiting, the writer stops at the next chunk boundary.
- Stripes stay connected while UDP is active, so migration opens no connections.

## 6. UDP binding

Each datagram is exactly one chunk. The maximum datagram is 1222 bytes.

### 6.1 Probes

`magic:u16 version:u8 flags:u8 seq:u32 t_us:u64 token:[16]` (32 bytes)

The client sends `flags = 0x80` to UDP 48101. For a valid token the host records the source
address as the session's video address and echoes the packet with `flags = 0x81`. Invalid
tokens get no reply, so the host cannot be used as a reflector. Chunks are sent only to an
address learned this way.

### 6.2 Forward error correction

With `fec` on, the host sends `G = ceil(D / 4)` parity chunks after all data chunks. Data
chunk `i` belongs to group `i mod G`, so neighbouring chunks land in different groups and a
short burst costs each group at most one chunk.

A parity chunk has the PARITY flag, `chunk_idx = g` and this payload:

`length_xor:u16 | data_xor`

`length_xor` is the XOR of the group's payload lengths as u16. `data_xor` is the XOR of the
group's payloads, each zero-padded to the group's longest, and has that longest length.

To repair a group missing exactly one data chunk, XOR `length_xor` with the known lengths to
get the missing length, XOR `data_xor` with the known payloads, and truncate to that length.

### 6.3 Pacing and staleness

The host MUST spread a frame over at most half a frame interval rather than bursting it.
The client MUST drop a frame arriving over UDP that is still incomplete two frame intervals
after its first chunk. Over TCP the rule is 1 s, since TCP delivers late rather than never.

## 7. Transport selection and migration

### 7.1 At session start

After CONFIG_OK the client, at the same time, attaches all stripes and sends 10 probes.
It sends PROBE_RESULT as soon as 8 are echoed or 300 ms have passed. The host then chooses:

- mode 1: UDP if `echoed > 0`, otherwise BYE(6).
- mode 2: TCP if a stripe attaches within 2 s, otherwise BYE(6).
- mode 0: UDP if `echoed >= 8`, otherwise TCP, otherwise BYE(6).

### 7.2 During streaming

The host alone decides and announces the change with MIGRATE. The client follows only so
that it sends input on the same transport; video needs no client action.

UDP to TCP, on any of:

- the client sets `UDP_SILENT` in STATS. It sends such a STATS at once after 500 ms with no
  chunk while UDP is active, without waiting for the 500 ms timer. The clock starts at the
  first chunk, or 2 s after START if none has arrived, since the first capture can be slow.
  A host cannot observe silence itself because it is the sender.
- loss above 15% in one STATS window.
- frame completion below 50% in two consecutive windows. Whole-frame delivery fails long
  before packet loss reaches 15%: see `spec/DESIGN.md`.

TCP to UDP:

- While on TCP, the client sends a burst of 10 probes every 10 s.
- If 8 or more echo within 300 ms it sends 10 probes per second for 3 s. If at least 90% echo
  it keeps reporting `udp_clean_ms`, the time UDP has been continuously clean. Any
  verification failure resets it to 0.
- The host migrates when `udp_clean_ms >= 3000` and the hold-off has passed. The hold-off
  starts at 10 s and doubles, to at most 80 s, whenever UDP fails again within 30 s of a
  return. It resets after a stable 30 s.

## 8. Statistics

The client sends STATS every 500 ms during STREAMING:

`window_ms:u16 loss_permille:u16 completion_permille:u16 jitter_us:u32 fps_x10:u16
latency_us:u32 frames_skipped:u32 udp_clean_ms:u32 flags:u8` (25 bytes)

- `loss_permille`: chunks received against chunks expected (`D`, plus `G` with FEC) over
  frames closed in the window, on UDP. 0 on TCP.
- `completion_permille`: frames completed against frames closed in the window, where a
  frame closes by completing or by going stale.
- `jitter_us`: RFC 3550 interarrival jitter over first chunks per frame.
- `fps_x10`: frames displayed per second times 10.
- `latency_us`: mean capture-to-display latency in the window.
- `frames_skipped`: frames completed but never shown, cumulative.
- `flags`: `0x01` UDP_SILENT.

## 9. Adaptive quality

The host acts on every STATS, multiplying quality by 0.8 when any congestion signal is true:

- UDP: loss above 2%, or jitter above one frame interval.
- TCP: the chosen stripe held more than one frame for three frames in a row, or smoothed RTT
  (RFC 6298, gain 1/8) above twice the session minimum plus 20 ms.

At the quality floor of 20 it reduces fps by the same factor, to a floor of 5. After 2 s with
no signal it raises fps to the target first, then adds 5 to quality, up to the configured
quality. Quality and fps are host-side decisions and need no signalling.

## 10. Input

Payload, on both transports:

`magic:u16 version:u8 count:u8 token:[16] event*count`

UDP: one datagram to port 48102. TCP: prefixed with `length:u16` on the input connection.
`count` is 1 to 3: the newest event plus up to two earlier ones, oldest first.

Each event is 16 bytes:

`seq:u32 kind:u8 state:u8 x:u16 y:u16 code:u32 reserved:u16(0)`

| kind | Name | Fields |
|---|---|---|
| 1 | MOUSE_MOVE | `x`, `y` absolute, 0 to 65535 across the captured screen |
| 2 | MOUSE_BUTTON | `code` 1 left, 2 middle, 3 right; `state` 1 down, 0 up |
| 3 | SCROLL | `x`, `y` signed deltas as two's complement i16 |
| 4 | KEY | `code` an X11 keysym; `state` 1 down, 0 up |

`seq` starts at 1 and increases per event across both transports. The host applies events in
order and ignores any with `seq` at or below the newest applied, which also deduplicates a
migration in progress. An event overtaken by more than the two-event redundancy window is
lost; a client switching transport therefore repeats its last two events on the new path. A
datagram with a bad token is dropped silently.

Keysyms are portable across operating systems. The host MUST release every key and button
a session holds down when that session ends.

## 11. Security model

Pairing proves knowledge of the PIN without sending it. Probes, attaches and input carry the
session token, so a host on a shared LAN rejects off-path traffic.

There is no encryption. Anyone able to sniff the path can read the screen, read the token
and inject input, and can test PINs offline against a captured PAIR. Twilight is meant for a
trusted LAN only. Internet use requires a VPN or a future encrypted version.
