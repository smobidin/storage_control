#!/usr/bin/env bash
# Install or refresh Storage Control as a systemd *user* service.
#
# The unit is rendered from tools/storage-control.service, written to
# ~/.config/systemd/user/storage-control.service, enabled at login and started.
# Secrets are not committed: the script creates
# ~/.config/storage-control/secrets.env on first install if it does not exist.
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"

SERVICE_NAME="${SERVICE_NAME:-storage-control}"
APP_DIR="${APP_DIR:-$REPO_ROOT/storage-control}"
VENV_DIR="${VENV_DIR:-}"
ENV_FILE="${ENV_FILE:-${XDG_CONFIG_HOME:-$HOME/.config}/storage-control/secrets.env}"
BIND="${BIND:-0.0.0.0:8765}"
WORKERS="${WORKERS:-1}"
THREADS="${THREADS:-4}"
UNIT_SOURCE="$SCRIPT_DIR/storage-control.service"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT_DEST="$UNIT_DIR/$SERVICE_NAME.service"

ENABLE_SERVICE=1
START_SERVICE=1
CREATE_ENV=1
SETUP_VENV=0

usage() {
  cat <<EOF
Usage: $0 [options]

Install Storage Control as a systemd user service.

Options:
  --app-dir PATH     Application directory (default: $APP_DIR)
  --venv PATH        Virtualenv with gunicorn (default: APP_DIR/.venv)
  --env-file PATH    Environment file with secrets (default: $ENV_FILE)
  --bind ADDR        Gunicorn bind address (default: $BIND)
  --workers N        Gunicorn workers; keep at 1 for long-running jobs
  --threads N        Gunicorn threads (default: $THREADS)
  --setup-venv       Create the venv and install requirements if missing
  --no-create-env    Do not create the secrets env file automatically
  --no-enable        Do not enable the unit at login
  --no-start         Do not start/restart the unit now
  -h, --help         Show this help

Examples:
  $0
  $0 --setup-venv
  $0 --bind 127.0.0.1:8765 --no-start
EOF
}

log() { printf '[storage-control] %s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }
escape_sed() { printf '%s' "$1" | sed -e 's/[\\&|]/\\&/g'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --app-dir) APP_DIR="${2:?--app-dir requires a value}"; shift 2 ;;
    --venv) VENV_DIR="${2:?--venv requires a value}"; shift 2 ;;
    --env-file) ENV_FILE="${2:?--env-file requires a value}"; shift 2 ;;
    --bind) BIND="${2:?--bind requires a value}"; shift 2 ;;
    --workers) WORKERS="${2:?--workers requires a value}"; shift 2 ;;
    --threads) THREADS="${2:?--threads requires a value}"; shift 2 ;;
    --setup-venv) SETUP_VENV=1; shift ;;
    --no-create-env) CREATE_ENV=0; shift ;;
    --no-enable) ENABLE_SERVICE=0; shift ;;
    --no-start) START_SERVICE=0; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "Unknown option: $1 (use --help)" ;;
  esac
done

VENV_DIR="${VENV_DIR:-$APP_DIR/.venv}"

command -v systemctl >/dev/null 2>&1 || die "systemctl not found; this script requires systemd"
if [[ "$(id -u)" -eq 0 ]]; then
  die "This is a systemd user service. Run the script as the target user, not root."
fi
[[ -f "$UNIT_SOURCE" ]] || die "Unit template not found: $UNIT_SOURCE"
[[ -d "$APP_DIR" ]] || die "Application directory not found: $APP_DIR"
[[ -f "$APP_DIR/wsgi.py" ]] || die "wsgi.py not found in $APP_DIR"

if ! systemctl --user show-environment >/dev/null 2>&1; then
  die "Cannot talk to the systemd user manager. Log in with an active user session or enable lingering first."
fi

if [[ ! -x "$VENV_DIR/bin/gunicorn" ]]; then
  if [[ "$SETUP_VENV" -eq 1 ]]; then
    log "Creating virtualenv: $VENV_DIR"
    python3 -m venv "$VENV_DIR"
    "$VENV_DIR/bin/python" -m pip install --upgrade pip
    "$VENV_DIR/bin/pip" install -r "$APP_DIR/requirements.txt"
  else
    die "gunicorn not found at $VENV_DIR/bin/gunicorn. Run '$0 --setup-venv' or create the venv manually."
  fi
fi

if [[ "$CREATE_ENV" -eq 1 && ! -f "$ENV_FILE" ]]; then
  log "Creating secrets file: $ENV_FILE"
  mkdir -p "$(dirname "$ENV_FILE")"
  chmod 700 "$(dirname "$ENV_FILE")" 2>/dev/null || true
  secret_key="$(python3 -c 'import secrets; print(secrets.token_hex(32))')"
  admin_password="${ADMIN_PASSWORD:-$(python3 -c 'import secrets; print(secrets.token_urlsafe(12))')}"
  storage_home="${STORAGE_HOME:-/home/storage}"
  storage_data="${STORAGE_CONTROL_DATA:-$storage_home/.local/share/storage-control}"
  umask 077
  cat > "$ENV_FILE" <<EOF
SECRET_KEY=$secret_key
ADMIN_USERNAME=${ADMIN_USERNAME:-admin}
ADMIN_PASSWORD=$admin_password
STORAGE_HOME=$storage_home
STORAGE_CONTROL_DATA=$storage_data
ENABLE_SCHEDULER=${ENABLE_SCHEDULER:-true}
EOF
  chmod 600 "$ENV_FILE"
  log "Admin user: ${ADMIN_USERNAME:-admin}"
  log "Admin password: $admin_password"
  log "Save it now: it is stored only in $ENV_FILE"
  log "The app resets the admin password to ADMIN_PASSWORD on each start."
fi

tmp_unit="$(mktemp)"
trap 'rm -f "$tmp_unit"' EXIT
sed \
  -e "s|@APP_DIR@|$(escape_sed "$APP_DIR")|g" \
  -e "s|@VENV_DIR@|$(escape_sed "$VENV_DIR")|g" \
  -e "s|@ENV_FILE@|$(escape_sed "$ENV_FILE")|g" \
  -e "s|@BIND@|$(escape_sed "$BIND")|g" \
  -e "s|@WORKERS@|$(escape_sed "$WORKERS")|g" \
  -e "s|@THREADS@|$(escape_sed "$THREADS")|g" \
  "$UNIT_SOURCE" > "$tmp_unit"

install -D -m 0644 "$tmp_unit" "$UNIT_DEST"
log "Installed user unit: $UNIT_DEST"

systemctl --user daemon-reload

if command -v loginctl >/dev/null 2>&1; then
  linger="$(loginctl show-user "$USER" -p Linger --value 2>/dev/null || true)"
  if [[ "$linger" != "yes" ]]; then
    log "Enabling linger for $USER so the service survives logout"
    loginctl enable-linger "$USER" 2>/dev/null || log "WARNING: could not enable linger; the service may stop after logout"
  fi
fi

if [[ "$ENABLE_SERVICE" -eq 1 ]]; then
  systemctl --user enable "$SERVICE_NAME.service"
  log "Enabled at login: $SERVICE_NAME.service"
fi

if [[ "$START_SERVICE" -eq 1 ]]; then
  if systemctl --user restart "$SERVICE_NAME.service"; then
    log "Started: $SERVICE_NAME.service"
  else
    log "Service failed to start. Current status:"
    systemctl --user --no-pager --full status "$SERVICE_NAME.service" || true
    exit 1
  fi
fi

case "$BIND" in
  0.0.0.0:*) SERVICE_URL="http://<host>:${BIND#0.0.0.0:}/" ;;
  *) SERVICE_URL="http://$BIND/" ;;
esac
log "Service URL: $SERVICE_URL (put firewall/nginx/TLS in front for external access)"
if [[ "$START_SERVICE" -eq 1 ]]; then
  systemctl --user --no-pager --full status "$SERVICE_NAME.service" || true
fi
cat <<EOF

Manage the service:
  systemctl --user status $SERVICE_NAME
  systemctl --user restart $SERVICE_NAME
  systemctl --user stop $SERVICE_NAME
  journalctl --user -u $SERVICE_NAME -f

Remove the service (secrets/data stay):
  $SCRIPT_DIR/uninstall.sh
EOF
