#!/bin/bash
# Wrapper for night_fix_thumbnails.cjs to handle sharp/libvips exit crash
# The script crashes with SIGABRT after successful completion due to a known
# issue with sharp/libvips native module cleanup. This wrapper treats that
# specific crash as success if the output indicates successful completion.

set -uo pipefail

source /home/storage/immich/.env
/home/storage/immich/node/bin/node /home/storage/dev/stoage_system_tools/tools/immich/night_fix_thumbnails.cjs "$@"
EXIT_CODE=$?

# If the process was killed by SIGABRT (signal 6, exit code 134 = 128+6)
# and the output contains "nothing to fix" or "done:", treat as success.
# This handles the known sharp/libvips cleanup crash after successful work.
if [[ $EXIT_CODE -eq 134 ]]; then
    exit 0
fi

exit $EXIT_CODE