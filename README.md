# Twilight

A screen streaming protocol that runs over UDP or TCP and moves between them mid-stream.
CN lab project, JIIT Noida.

| Path | What |
|---|---|
| `spec/` | Protocol, design notes, shared byte vectors |
| `host/` | Rust host | Harsh |
| `client/` | Python client |
| `testbed/` | Network experiments, no root needed |
| `report/` | Changes from the synopsis |

## Quick start

```sh
cd host && cargo run --release -- --pin 123456
cd client && uv sync && uv run client.py HOST_IP --pin 123456
```

Press `q` in the video window to quit. Use `--source synthetic` on the host when you have no
display.

Each folder has its own README.
