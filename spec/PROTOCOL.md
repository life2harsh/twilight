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

