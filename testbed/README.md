# Testbed

## Everything at once

```sh
testbed/run_all.sh          # about 5 minutes
SECS=5 testbed/run_all.sh   # shorter runs
```

Runs `cargo test`, `pytest`, a release build, the full `lab.sh` matrix (clean, 1/5/10% loss,
20 Mbit/s cap, each over UDP, TCP and 4 TCP stripes), a UDP block and unblock for migration,
the summary table, the ns-2 bottleneck simulation if `ns` is installed, and the graphs. It
prints PASS or FAIL per stage and exits non-zero on any failure. Output goes to `results/`,
graphs to `results/figures/` (`modes.png`, `migration.png`, `ns2.png`), and the full log to
`results/run_all.log`. `plot.py` fetches matplotlib through `uv` on first run, so that run
needs internet.

## Single runs

Runs host and client in two Linux network namespaces joined by a veth pair, with no root.
`netem` loss and delay apply to host-to-client traffic.

Build the host in release mode and run `uv sync` in `client/` first.

```sh
testbed/lab.sh NAME SECONDS MODE STRIPES NETEM [BLOCK_AT [UNBLOCK_AT]]
python3 testbed/summary.py results/*.csv
```

`MODE`: 0 auto, 1 UDP only, 2 TCP only.

```sh
testbed/lab.sh clean_udp 10 1 4 none
testbed/lab.sh loss5 10 2 4 "loss 5% delay 20ms"
testbed/lab.sh capped 12 2 1 "rate 20mbit delay 10ms"
testbed/lab.sh blocked 12 0 4 none 4
```

The last one blocks UDP 4 s in and should migrate to TCP. Results go to `results/`, which git
ignores. `NAME.csv` is the host metrics log.

Set `TWILIGHT_SOURCE=screen` for real capture.
