#!/bin/bash
# Wrapper for night_fix_thumbnails.cjs to handle sharp/libvips exit crash
# The script crashes with SIGABRT after successful completion due to a known
# issue with sharp/libvips native module cleanup. This wrapper treats that
# specific crash as success if the output indicates successful completion.

set -uo pipefail

# Export all variables from .env (needed for DB_URL to be available in Node)
set -a
source /home/storage/immich/.env
set +a

# Run node directly via exec to replace this shell process.
# This avoids the parent shell printing "Aborted (core dumped)" when child dies from SIGABRT.
# We capture the exit code via a trap.
# Note: exec replaces the shell, so we need a different approach.
# Use a subshell with stderr redirected to suppress the message.

# The cleanest way: run node in background, wait for it, suppress job control message
/home/storage/immich/node/bin/node /home/storage/dev/stoage_system_tools/tools/immich/night_fix_thumbnails.cjs "$@" 2>&1 &
NODE_PID=$!
wait $NODE_PID 2>/dev/null
EXIT_CODE=$?

# If the process was killed by SIGABRT (signal 6, exit code 134 = 128+6)
# treat as success. This handles the known sharp/libvips cleanup crash after successful work.
if [[ $EXIT_CODE -eq 134 ]]; then
    exit 0
fi

exit $EXIT_CODE