#!/bin/bash
set -euo pipefail

# Collabora Online AppImage Updater
# Downloads latest AppImage from official source, replaces, restarts service
# NOTE: Official download URL is Cloudflare-protected. This script documents the manual process.

APPIMAGE_PATH="/home/storage/nextcloud/apps/richdocumentscode/collabora/Collabora_Online.AppImage"
BACKUP_DIR="/home/storage/nextcloud/apps/richdocumentscode/collabora/backups"
SERVICE_NAME="collabora-online.service"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log() { echo -e "${GREEN}[INFO]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
err() { echo -e "${RED}[ERROR]${NC} $*" >&2; }

# Check running as root (for systemctl)
if [[ $EUID -ne 0 ]]; then
    err "Must run as root (sudo) for systemctl restart"
    exit 1
fi

show_manual_instructions() {
    cat << 'EOF'

========================================
MANUAL COLLABORA ONLINE UPDATE PROCESS
========================================

The official AppImage is distributed via Nextcloud App Store:
https://apps.nextcloud.com/apps/richdocumentscode/releases?platform=26

This provides a tar.gz package containing the AppImage at:
richdocumentscode/collabora/Collabora_Online.AppImage

STEP 1: Download the tar.gz from Nextcloud App Store
   - Visit: https://apps.nextcloud.com/apps/richdocumentscode/releases?platform=26
   - Download the latest richdocumentscode.tar.gz
   - Or use direct GitHub release:
     https://github.com/CollaboraOnline/richdocumentscode/releases/download/26.4.402/richdocumentscode.tar.gz

STEP 2: Extract and replace AppImage
   tar -xzf richdocumentscode.tar.gz
   sudo systemctl stop collabora-online.service
   sudo cp richdocumentscode/collabora/Collabora_Online.AppImage /home/storage/nextcloud/apps/richdocumentscode/collabora/Collabora_Online.AppImage
   sudo chown cool:cool /home/storage/nextcloud/apps/richdocumentscode/collabora/Collabora_Online.AppImage
   sudo chmod +x /home/storage/nextcloud/apps/richdocumentscode/collabora/Collabora_Online.AppImage

STEP 3: Restart service
   sudo systemctl restart collabora-online.service

STEP 4: Verify
   sudo systemctl status collabora-online.service
   # Check logs for version:
   sudo journalctl -u collabora-online.service --since "2 minutes ago" | grep -iE 'version|product'

========================================
AUTOMATED CHECKS (what this script does)
========================================

EOF
}

main() {
    echo "=== Collabora Online AppImage Update Helper ==="
    echo
    
    # Check current version from logs
    log "Checking current Collabora version from service logs..."
    ACTUAL_VERSION=$(journalctl -u collabora-online.service --since "1 hour ago" 2>/dev/null | grep -oE 'ProductVersion": "[0-9]+\.[0-9]+' | head -1 | cut -d'"' -f4)
    if [[ -n "$ACTUAL_VERSION" ]]; then
        log "Current running version: $ACTUAL_VERSION"
    else
        warn "Could not determine current version from logs"
    fi
    echo
    
    # Show manual instructions
    show_manual_instructions
    
    # Check service status
    log "Checking service status..."
    if systemctl is-active --quiet "$SERVICE_NAME"; then
        log "Service is RUNNING"
        systemctl status "$SERVICE_NAME" --no-pager | head -15
    else
        warn "Service is NOT RUNNING"
        systemctl status "$SERVICE_NAME" --no-pager | head -15
    fi
    echo
    
    log "Current AppImage file:"
    ls -lh "$APPIMAGE_PATH"
    echo
    
    log "Backup directory:"
    ls -lh "$BACKUP_DIR"
}

main "$@"