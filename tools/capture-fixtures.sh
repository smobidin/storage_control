#!/usr/bin/env bash
# Capture real command output from the running host as test fixtures for the
# Rust rewrite. Raw output lands in a git-ignored directory; nothing here is
# committed as-is (serial numbers are redacted by sanitize.sh).
#
# Usage: tools/capture-fixtures.sh [outdir]
set -uo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$REPO_ROOT/storage-control-rs/tests/fixtures/raw}"
mkdir -p "$OUT"/{smart,mdadm,systemd,git,releases,legacy}

log() { printf '[capture] %s\n' "$*"; }

# --- RAID ---------------------------------------------------------------
log "raid"
cp /proc/mdstat "$OUT/mdadm/mdstat.txt" 2>/dev/null || true
sudo -n mdadm --detail /dev/md0 > "$OUT/mdadm/md0-detail.txt" 2>&1 || true

# --- SMART --------------------------------------------------------------
log "smart"
for dev in /sys/block/sd*; do
  name="$(basename "$dev")"
  case "$name" in sr*) continue ;; esac
  sudo -n smartctl -a "/dev/$name" > "$OUT/smart/$name.txt" 2>&1 || true
done

# --- systemd ------------------------------------------------------------
log "systemd"
UNIT_SPECS="immich-ml.service:user immich-server.service:user immich-nightfix.service:user \
immich-nightfix.timer:user backup-immich-nc.timer:user stalwart.service:user \
postgresql.service:system redis-server.service:system php8.5-fpm.service:system \
nginx.service:system certbot-renew.timer:system mdcheck_start.timer:system"

for spec in $UNIT_SPECS; do
  unit="${spec%%:*}"; scope="${spec##*:}"
  if [ "$scope" = user ]; then
    systemctl --user show "$unit" \
      -p ActiveEnterTimestamp -p ActiveExitTimestamp -p ExecMainStatus -p ExecMainCode \
      -p UnitFileState -p Type -p Result -p NextEligibleTimestamp \
      > "$OUT/systemd/${unit}.show.txt" 2>&1 || true
    systemctl --user is-active "$unit" > "$OUT/systemd/${unit}.is-active.txt" 2>&1 || true
    systemctl --user is-enabled "$unit" > "$OUT/systemd/${unit}.is-enabled.txt" 2>&1 || true
    case "$unit" in *.service)
      systemctl --user show "${unit%.service}.timer" -p LastTriggerUSec \
        > "$OUT/systemd/${unit}.timer-show.txt" 2>&1 || true ;;
    esac
  else
    sudo -n systemctl show "$unit" \
      -p ActiveEnterTimestamp -p ActiveExitTimestamp -p ExecMainStatus -p ExecMainCode \
      -p UnitFileState -p Type -p Result -p NextEligibleTimestamp \
      > "$OUT/systemd/${unit}.show.txt" 2>&1 || true
    sudo -n systemctl is-active "$unit" > "$OUT/systemd/${unit}.is-active.txt" 2>&1 || true
    sudo -n systemctl is-enabled "$unit" > "$OUT/systemd/${unit}.is-enabled.txt" 2>&1 || true
  fi
done

systemctl --user list-timers --all --no-pager > "$OUT/systemd/user-list-timers.txt" 2>&1 || true
sudo -n systemctl list-timers --all --no-pager > "$OUT/systemd/system-list-timers.txt" 2>&1 || true
systemctl --user cat immich-nightfix.timer > "$OUT/systemd/immich-nightfix.timer.cat.txt" 2>&1 || true

# --- git ----------------------------------------------------------------
log "git"
for repo in "$HOME/immich/src" "$HOME/nextcloud"; do
  [ -d "$repo" ] || continue
  tag="$(basename "$repo")"
  {
    echo "== describe --tags --exact-match"; git -C "$repo" describe --tags --exact-match 2>&1
    echo "== describe --tags --abbrev=0";     git -C "$repo" describe --tags --abbrev=0 2>&1
    echo "== tag --sort=-creatordate";        git -C "$repo" tag --sort=-creatordate 2>&1 | head -60
    echo "== status --porcelain";             git -C "$repo" status --porcelain 2>&1 | head -40
  } > "$OUT/git/$tag.txt" 2>&1 || true
done

# --- GitHub releases ----------------------------------------------------
log "releases"
for pair in "immich:immich-app/immich" "nextcloud:nextcloud/server" "stalwart:stalwartlabs/stalwart"; do
  name="${pair%%:*}"; repo="${pair##*:}"
  curl -sS -m 20 -H 'User-Agent: storage-control' -H 'Accept: application/vnd.github+json' \
    "https://api.github.com/repos/$repo/releases/latest" > "$OUT/releases/$name.json" 2>/dev/null || true
  sleep 1
done

# --- Nextcloud status ---------------------------------------------------
log "occ status"
if [ -x "$HOME/nextcloud/occ" ]; then
  php "$HOME/nextcloud/occ" status > "$OUT/legacy/occ-status.txt" 2>&1 || true
fi

# --- legacy database ----------------------------------------------------
log "legacy db"
DB="$HOME/.local/share/storage-control/app.db"
if [ -f "$DB" ]; then
  cp "$DB" "$OUT/legacy/app.db"
fi

log "done: $OUT"
du -sh "$OUT" 2>/dev/null
