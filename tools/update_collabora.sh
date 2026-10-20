#!/bin/bash
set -euo pipefail

# Collabora Online AppImage Updater
# Checks latest release via GitHub API, downloads tar.gz, extracts AppImage, restarts service

GITHUB_API="https://api.github.com/repos/CollaboraOnline/richdocumentscode/releases/latest"
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

# Get latest release info from GitHub API
get_latest_release() {
    curl -sL -H "Accept: application/vnd.github.v3+json" "$GITHUB_API"
}

# Get download URL for x86_64 tar.gz
get_download_url() {
    local release_json="$1"
    echo "$release_json" | jq -r '.assets[] | select(.name | test("^richdocumentscode.*\\.tar\\.gz$")) | select(.name | test("arm64|aarch64") | not) | .browser_download_url'
}

# Get tag name (version)
get_tag_name() {
    local release_json="$1"
    echo "$release_json" | jq -r '.tag_name'
}

# Get current AppImage size
get_appsize() {
    if [[ -f "$APPIMAGE_PATH" ]]; then
        stat -c%s "$APPIMAGE_PATH" 2>/dev/null || echo 0
    else
        echo 0
    fi
}

main() {
    echo "=== Collabora Online AppImage Updater ==="
    echo

    log "Fetching latest release from GitHub API..."
    RELEASE_JSON=$(get_latest_release)
    
    if [[ -z "$RELEASE_JSON" || "$RELEASE_JSON" == "null" ]]; then
        err "Failed to fetch release info from GitHub API"
        exit 1
    fi

    LATEST_TAG=$(get_tag_name "$RELEASE_JSON")
    DOWNLOAD_URL=$(get_download_url "$RELEASE_JSON")
    
    if [[ -z "$DOWNLOAD_URL" || "$DOWNLOAD_URL" == "null" ]]; then
        err "Could not find x86_64 tar.gz download URL in release"
        echo "$RELEASE_JSON" | jq '.assets[].name'
        exit 1
    fi

    log "Latest release: $LATEST_TAG"
    log "Download URL: $DOWNLOAD_URL"
    echo

    CURRENT_SIZE=$(get_appsize)
    log "Current AppImage size: $CURRENT_SIZE bytes"
    echo

    # Backup current AppImage
    mkdir -p "$BACKUP_DIR"
    BACKUP_FILE="$BACKUP_DIR/Collabora_Online.AppImage.$(date +%Y%m%d_%H%M%S)"
    log "Backing up current AppImage to $BACKUP_FILE"
    cp "$APPIMAGE_PATH" "$BACKUP_FILE"

    # Download tar.gz
    log "Downloading $LATEST_TAG from $DOWNLOAD_URL..."
    TMP_DIR=$(mktemp -d)
    curl -L --progress-bar -o "$TMP_DIR/richdocumentscode.tar.gz" "$DOWNLOAD_URL"
    
    # Verify download
    if [[ ! -f "$TMP_DIR/richdocumentscode.tar.gz" ]] || [[ ! -s "$TMP_DIR/richdocumentscode.tar.gz" ]]; then
        err "Download failed or empty file"
        rm -rf "$TMP_DIR"
        exit 1
    fi

    DOWNLOAD_SIZE=$(stat -c%s "$TMP_DIR/richdocumentscode.tar.gz")
    log "Downloaded tar.gz size: $DOWNLOAD_SIZE bytes"

    # Extract AppImage
    log "Extracting AppImage from tar.gz..."
    tar -xzf "$TMP_DIR/richdocumentscode.tar.gz" -C "$TMP_DIR/"
    
    EXTRACTED_APPIMAGE="$TMP_DIR/richdocumentscode/collabora/Collabora_Online.AppImage"
    if [[ ! -f "$EXTRACTED_APPIMAGE" ]]; then
        err "AppImage not found in extracted tar.gz"
        ls -la /tmp/richdocumentscode/
        exit 1
    fi

    NEW_SIZE=$(stat -c%s "$EXTRACTED_APPIMAGE")
    log "New AppImage size: $NEW_SIZE bytes"

    # Skip if same size (likely same version)
    if [[ "$NEW_SIZE" -eq "$CURRENT_SIZE" && "$CURRENT_SIZE" -gt 0 ]]; then
        log "Same file size ($NEW_SIZE bytes). Likely same version. Nothing to do."
        rm -rf "$TMP_DIR"
        exit 0
    fi

    # Stop service
    log "Stopping $SERVICE_NAME..."
    systemctl stop "$SERVICE_NAME"
    sleep 2

    # Replace AppImage
    log "Replacing AppImage..."
    cp "$EXTRACTED_APPIMAGE" "$APPIMAGE_PATH"
    chown cool:cool "$APPIMAGE_PATH"
    chmod +x "$APPIMAGE_PATH"

    # Restart service
    log "Restarting $SERVICE_NAME..."
    systemctl restart "$SERVICE_NAME"

    # Cleanup
    rm -rf "$TMP_DIR"

    # Wait for service to be ready
    log "Waiting for service to start..."
    sleep 10

    # Verify
    if systemctl is-active --quiet "$SERVICE_NAME"; then
        log "Service restarted successfully"
        sleep 3
        log "Update completed successfully!"
        log "Old AppImage backed up at: $BACKUP_FILE"
    else
        err "Service failed to start! Check: systemctl status $SERVICE_NAME"
        warn "Attempting rollback..."
        mv "$BACKUP_FILE" "$APPIMAGE_PATH"
        chown cool:cool "$APPIMAGE_PATH"
        chmod +x "$APPIMAGE_PATH"
        systemctl restart "$SERVICE_NAME"
        exit 1
    fi
}

main "$@"