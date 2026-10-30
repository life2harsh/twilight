# Twilight client

A deliberately small starting point. It connects, pairs, probes, receives on UDP and TCP,
reassembles, displays and survives migration. Your job is to take it the rest of the way, using
`spec/PROTOCOL.md` as the only reference. The host is finished, so a mismatch is usually a
client bug.

## Run

```sh
uv sync
uv run client.py HOST_IP --pin 123456
uv run client.py HOST_IP --pin 123456 --mode 2 --stripes 4
uv run pytest
```

`--mode`: 0 auto, 1 UDP only, 2 TCP only. `--headless --seconds 10` runs with no window.
Without a host machine, start one locally: `twilight-host --source synthetic --pin 123456`.

## How it is organised

- `protocol.py`: byte layouts and helpers.
- `client.py`:
  - `Reassembler` collects chunks into frames. Every receiver feeds this one object, which is
    why the rest of the client never needs to know the transport.
  - `Client.handshake` runs HELLO, PAIR and CONFIG.
  - `stripe_loop`, `udp_loop`, `control_loop`, `stats_loop` are the background threads.
  - `display_loop` runs on the main thread.

Understand this end to end before adding anything. The viva will ask why each thread exists.

## Your tasks, in order

Each lists the spec section. Write a pytest before the code where you can; `spec/vectors.json`
has real bytes to test against.

1. **FEC repair** (6.2). Parity chunks are ignored today. Keep them, and rebuild a frame missing
   one chunk per group. The `parity` list in the vectors is your test. Without this, 2% loss
   forces a migration to TCP.
2. **Real statistics** (8). `stats_loop` reports zeros for loss, jitter and latency. Without
   real loss numbers the host cannot adapt quality on UDP. The host logs what you send to CSV,
   so this is what the report's graphs are made of.
3. **Clock offset** (3.2). Send PING from the client and compute the offset. It is needed for
   latency.
4. **Input channel** (10). Capture with `pynput`, or from the OpenCV window where `pynput`
   cannot see global input, as on Wayland. Send over UDP or TCP according to START and MIGRATE,
   and repeat the previous two events.
5. **Return to UDP** (7.2). Re-probe every 10 s on TCP and report `udp_clean_ms`. Until then a
   session that moved to TCP stays there.
6. **Overlay and CSV** (4.10 in the synopsis). Show loss, jitter, fps, latency and transport.
7. **Immediate silence report** (7.2). `stats_loop` waits for its 500 ms timer; send at once.

## Things that will trip you up

- A frame overtaken by a newer one is not a failure. Count those separately, otherwise
  `completion_permille` looks bad on striped TCP.
- `recv` on TCP can return part of a message. Always use `read_exact`.
- The synthetic source at 720p is about 60 Mbit/s, several thousand packets a second. Keep work
  in `udp_loop` small and decode on the main thread only.
- Run with `--fps 15` or a lower quality first when debugging.
