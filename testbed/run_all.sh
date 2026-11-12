#!/usr/bin/env bash
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
out=$root/results
secs=${SECS:-12}
modes="udp tcp1 tcp4"
conditions="clean l1 l5 l10 cap"
block_at=5 unblock_at=15
declare -A netem=([clean]=none [l1]="loss 1% delay 20ms" [l5]="loss 5% delay 20ms" [l10]="loss 10% delay 20ms" [cap]="rate 20mbit delay 10ms")
declare -A args=([udp]="1 4" [tcp1]="2 1" [tcp4]="2 4")
report=()
mkdir -p "$out"
exec > >(tee "$out/run_all.log") 2>&1

stage() {
  local name=$1; shift
  printf '\n\033[1m=== %s ===\033[0m\n' "$name"
  if "$@"; then report+=("PASS  $name"); else report+=("FAIL  $name"); fi
}

matrix() {
  local c m ok=0
  for c in $conditions; do for m in $modes; do
    echo "--- ${c}_$m: ${netem[$c]}"
    "$root/testbed/lab.sh" "${c}_$m" "$secs" ${args[$m]} "${netem[$c]}" >/dev/null || ok=1
  done; done
  return $ok
}

ns2() {
  local loss
  for loss in 0 0.01; do
    ns "$root/testbed/ns2/bottleneck.tcl" "$loss" "$out/ns2_$loss.tr" &&
      awk -f "$root/testbed/ns2/throughput.awk" "$out/ns2_$loss.tr" >"$out/ns2_$loss.dat" || return 1
  done
}

stage "host unit + e2e tests" bash -c "cd '$root/host' && cargo test"
stage "client tests" bash -c "cd '$root/client' && uv run pytest -q"
stage "release build" bash -c "cd '$root/host' && cargo build --release && cd '$root/client' && uv sync"
stage "testbed matrix (${secs}s runs)" matrix
stage "migration (UDP blocked ${block_at}s-${unblock_at}s)" bash -c \
  "'$root/testbed/lab.sh' blocked 30 0 4 none $block_at $unblock_at >/dev/null && grep -q migrate_Silent '$out/blocked.csv'"
stage "summary" bash -c "cd '$out' && python3 '$root/testbed/summary.py' *_*.csv blocked.csv"
command -v ns >/dev/null && stage "ns-2 simulation" ns2
stage "graphs" uv run --no-project --with matplotlib python "$root/testbed/plot.py" "$out" "$conditions" "$modes" "$block_at" "$unblock_at"

printf '\n\033[1m=== result ===\033[0m\n'
printf '%s\n' "${report[@]}"
echo "logs and csv: $out   graphs: $out/figures   full log: $out/run_all.log"
! printf '%s\n' "${report[@]}" | grep -q FAIL
