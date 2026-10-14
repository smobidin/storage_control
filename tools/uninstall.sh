#!/usr/bin/env bash
# Remove the Storage Control systemd *user* service.
#
# Stops and disables the unit, deletes
# ~/.config/systemd/user/storage-control.service and reloads the user manager.
# Application data, logs and secrets are intentionally kept.
set -euo pipefail

SERVICE_NAME="${SERVICE_NAME:-storage-control}"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT_DEST="$UNIT_DIR/$SERVICE_NAME.service"

usage() {
  cat <<EOF
Usage: $0 [options]

Stop, disable and remove the Storage Control systemd user service.

Options:
  -h, --help   Show this help

Environment:
  SERVICE_NAME   Unit name without .service (default: $SERVICE_NAME)
EOF
}

log() { printf '[storage-control] %s\n' "$*"; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    *) printf 'ERROR: Unknown option: %s\n' "$1" >&2; exit 1 ;;
  esac
done

command -v systemctl >/dev/null 2>&1 || { echo "systemctl not found; nothing to do" >&2; exit 1; }
if [[ "$(id -u)" -eq 0 ]]; then
  echo "ERROR: This is a systemd user service. Run the script as the target user, not root." >&2
  exit 1
fi

if systemctl --user is-active --quiet "$SERVICE_NAME.service"; then
  systemctl --user stop "$SERVICE_NAME.service"
  log "Stopped: $SERVICE_NAME.service"
fi

if systemctl --user is-enabled --quiet "$SERVICE_NAME.service" 2>/dev/null; then
  systemctl --user disable "$SERVICE_NAME.service"
  log "Disabled at login: $SERVICE_NAME.service"
fi

if [[ -f "$UNIT_DEST" || -L "$UNIT_DEST" ]]; then
  rm -f "$UNIT_DEST"
  log "Removed unit file: $UNIT_DEST"
else
  log "Unit file not found (already removed): $UNIT_DEST"
fi

systemctl --user daemon-reload
systemctl --user reset-failed "$SERVICE_NAME.service" 2>/dev/null || true
log "Done. Secrets and application data were not touched."
