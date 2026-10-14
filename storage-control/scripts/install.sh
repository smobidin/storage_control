#!/usr/bin/env bash
# Full Storage Control install: system packages, venv, sudoers and user service.
#
# The systemd *user* service itself is installed by tools/install.sh; this
# script only prepares the host and then delegates the unit/env steps so the
# unit definition lives in exactly one place.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="${APP_DIR:-$(cd -- "$SCRIPT_DIR/.." && pwd)}"
REPO_ROOT="$(cd -- "$APP_DIR/.." && pwd)"
TOOLS_DIR="${TOOLS_DIR:-$REPO_ROOT/tools}"

SERVICE_USER="${SERVICE_USER:-storage}"
SERVICE_HOME="${SERVICE_HOME:-$(getent passwd "$SERVICE_USER" | cut -d: -f6 || true)}"
SERVICE_HOME="${SERVICE_HOME:-/home/storage}"
DATA_DIR="${DATA_DIR:-$SERVICE_HOME/.local/share/storage-control}"
SECRETS_DIR="${SECRETS_DIR:-$SERVICE_HOME/.config/storage-control}"
BIND="${BIND:-0.0.0.0:8765}"

if [[ "$(id -un)" != "$SERVICE_USER" ]]; then
  echo "ERROR: run this script as $SERVICE_USER (or set SERVICE_USER=...)." >&2
  exit 1
fi

if [[ ! -x "$TOOLS_DIR/install.sh" ]]; then
  echo "ERROR: tools/install.sh not found at $TOOLS_DIR/install.sh" >&2
  exit 1
fi

echo "[1/7] install system packages"
sudo apt-get update
sudo apt-get install -y python3-venv python3-pip nginx smartmontools lm-sensors

echo "[2/7] create app dirs"
mkdir -p "$DATA_DIR" "$SECRETS_DIR" "$DATA_DIR/runs"
chmod 700 "$SECRETS_DIR"

echo "[3/7] create venv"
cd "$APP_DIR"
python3 -m venv .venv
.venv/bin/python -m pip install --upgrade pip
.venv/bin/pip install -r requirements.txt

echo "[4/7] sudoers"
sudo cp "$APP_DIR/scripts/sudoers-storage-control" /etc/sudoers.d/storage-control
sudo chmod 440 /etc/sudoers.d/storage-control
sudo visudo -cf /etc/sudoers.d/storage-control

echo "[5/7] user service via tools/install.sh"
export STORAGE_HOME="${STORAGE_HOME:-$SERVICE_HOME}"
export STORAGE_CONTROL_DATA="${STORAGE_CONTROL_DATA:-$DATA_DIR}"
"$TOOLS_DIR/install.sh" \
  --app-dir "$APP_DIR" \
  --env-file "$SECRETS_DIR/secrets.env" \
  --bind "$BIND"

echo "[6/7] nginx site (manual include)"
echo "Copy scripts/nginx-control-site.conf into /etc/nginx/sites-available and enable; expand cert for control.example.com (set your real domain)"

echo "[7/7] done"
echo "Status: systemctl --user status storage-control"
