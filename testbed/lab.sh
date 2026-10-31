#!/usr/bin/env bash
set -euo pipefail

usage="usage: lab.sh NAME SECONDS MODE(0 auto,1 udp,2 tcp) STRIPES NETEM|none [BLOCK_AT UNBLOCK_AT]"
[ $# -ge 5 ] || { echo "$usage" >&2; exit 2; }

if [ -z "${TWILIGHT_IN_NS:-}" ]; then
  exec unshare -Urn --kill-child env TWILIGHT_IN_NS=1 "$0" "$@"
fi

name=$1 seconds=$2 mode=$3 stripes=$4 netem=$5 block_at=${6:-} unblock_at=${7:-}
root=$(cd "$(dirname "$0")/.." && pwd)
out=$root/results
host=${TWILIGHT_HOST:-$root/host/target/release/twilight-host}
python=${TWILIGHT_PYTHON:-$root/client/.venv/bin/python}
pin=424242
mkdir -p "$out"

unshare -n sleep infinity &
peer=$!
trap 'kill $peer 2>/dev/null || true; jobs -p | xargs -r kill 2>/dev/null || true' EXIT
sleep 0.2
in_peer() { nsenter -t "$peer" -n "$@"; }

ip link set lo up
ip link add veth0 type veth peer name veth1 netns "$peer"
ip addr add 10.9.0.1/24 dev veth0
ip link set veth0 up
in_peer ip link set lo up
in_peer ip addr add 10.9.0.2/24 dev veth1
in_peer ip link set veth1 up
if [ "$netem" != none ]; then
  read -r -a netem_args <<<"$netem"
  tc qdisc add dev veth0 root netem "${netem_args[@]}"
fi

"$host" --source "${TWILIGHT_SOURCE:-synthetic}" --pin "$pin" --bind 10.9.0.1 \
  --metrics "$out/$name.csv" >"$out/$name.host.log" 2>&1 &
sleep 0.5

if [ -n "$block_at" ]; then
  ( sleep "$block_at"; iptables -A OUTPUT -p udp -j DROP
    if [ -n "$unblock_at" ]; then sleep $((unblock_at - block_at)); iptables -D OUTPUT -p udp -j DROP; fi ) &
fi

in_peer env PYTHONUNBUFFERED=1 "$python" "$root/client/client.py" 10.9.0.1 --pin "$pin" --mode "$mode" \
  --stripes "$stripes" --seconds "$seconds" --headless 2>&1 | tee "$out/$name.client.log"
