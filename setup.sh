#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

client_only=0
[ "${1:-}" = "--client-only" ] && client_only=1
step() { printf '\n== %s\n' "$*"; }
have() { command -v "$1" >/dev/null 2>&1; }

linux_packages() {
  if have cc && have cmake && have nasm && have pkg-config &&
    pkg-config --exists gbm xcb xkbcommon wayland-client libdrm; then
    return
  fi
  step "Installing system packages (needs sudo)"
  if have apt-get; then
    sudo apt-get update
    sudo apt-get install -y build-essential pkg-config cmake nasm curl \
      libgbm-dev libxcb1-dev libxkbcommon-dev libwayland-dev libdrm-dev libegl-dev
  elif have pacman; then
    sudo pacman -S --needed --noconfirm base-devel pkgconf cmake nasm curl \
      mesa libxcb libxkbcommon wayland libdrm
  elif have dnf; then
    sudo dnf install -y gcc gcc-c++ make pkgconf-pkg-config cmake nasm curl \
      mesa-libgbm-devel libxcb-devel libxkbcommon-devel wayland-devel libdrm-devel mesa-libEGL-devel
  else
    echo "Unknown distro: install cmake, nasm, pkg-config and the gbm, xcb, xkbcommon, wayland, drm dev packages" >&2
    exit 1
  fi
}

step "Python client"
if ! have uv; then
  curl -LsSf https://astral.sh/uv/install.sh | sh
  export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
fi
(cd client && uv sync && uv run pytest -q)

if [ "$client_only" = 1 ]; then
  printf '\nDone. Run: cd client && uv run client.py HOST_IP --pin PIN\n'
  exit 0
fi

step "Rust host"
linux_packages
if ! have cargo; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  export PATH="$HOME/.cargo/bin:$PATH"
fi
(cd host && cargo build --release && cargo test --release -q)

cat <<'EOF'

Done.
  Host:   host/target/release/twilight-host --source synthetic --pin 123456
  Client: cd client && uv run client.py 127.0.0.1 --pin 123456
EOF
