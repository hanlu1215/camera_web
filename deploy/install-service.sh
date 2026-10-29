#!/usr/bin/env bash
# Build (optional), install the binary, update the unit file and restart.
# Must be run with sudo (writes /etc/systemd/system and restarts the service).
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINARY="$REPO_DIR/target/release/camera_web"
DEST="/vol1/1000/TEMP/camera_web_bin"
UNIT="/etc/systemd/system/camera-web.service"
CONFIG="/vol1/1000/TEMP/camera_web_config.json"

# Use -f rather than -x: the target filesystem may deny execute checks to
# root on 0700 files. `install -m 0755` sets the mode of the destination.
if [[ ! -f "$BINARY" ]]; then
  echo "release binary not found: $BINARY" >&2
  echo "run: cd $REPO_DIR && cargo build --release" >&2
  exit 1
fi
chmod 0755 "$BINARY" 2>/dev/null || true

if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "warning: ffmpeg was not found on PATH; live capture and MP4 conversion need it" >&2
fi

install -m 0755 "$BINARY" "$DEST"
install -m 0644 "$REPO_DIR/deploy/camera-web.service" "$UNIT"
# Keep an existing config (it holds the user's paths); only seed a new one.
if [[ ! -f "$CONFIG" ]]; then
  install -m 0644 "$REPO_DIR/deploy/camera_web_config.json" "$CONFIG"
fi
systemctl daemon-reload
systemctl restart camera-web
systemctl --no-pager --full status camera-web || true
