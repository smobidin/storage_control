#!/bin/bash
# Wrapper for night_fix_thumbnails.cjs to handle sharp/libvips exit crash
# The script crashes with a fatal signal after completing its queue due to a
# known issue with sharp/libvips native module cleanup. The signal depends on
# the node version: 24.15.0 died with SIGABRT (exit 134), 24.22.1 with SIGSEGV
# (exit 139).
#
# That crash destroys the script's own exit code: with sharp loaded, both
# process.exit(0) and process.exit(1) end up as 134/139. The outcome therefore
# has to be recovered from the script's output, never from the process status:
#
#   "nothing to fix"                        -> nothing to do       -> 0
#   "done: N fixed, 0 failed, 0 still ..."  -> queue finished clean -> 0
#   "done: N fixed, F failed, L still ..."  -> assets left unfixed  -> 1
#   no marker at all                        -> crashed mid-run     -> keep the signal code
#
# Output is streamed to the journal (tee) and captured to $LOG for the marker
# check, so a killed run still leaves its progress in the journal.

set -uo pipefail

# Export all variables from .env (needed for DB_URL to be available in Node)
set -a
source /home/storage/immich/.env
set +a

LOG=$(mktemp)
trap 'rm -f "$LOG"' EXIT

/home/storage/immich/node/bin/node /home/storage/dev/stoage_system_tools/tools/immich/night_fix_thumbnails.cjs "$@" 2>&1 | tee -a "$LOG" &
PIPE_PID=$!
# stderr of wait is redirected so the shell does not print
# "Segmentation fault (core dumped)" for the dead child.
wait $PIPE_PID 2>/dev/null
# PIPESTATUS[0] is the node process, not tee.
EXIT_CODE=${PIPESTATUS[0]}

if [[ $EXIT_CODE -eq 134 || $EXIT_CODE -eq 139 ]]; then
    if grep -qE '^nothing to fix' "$LOG"; then
        exit 0
    elif grep -qE '^done: [0-9]+ fixed, 0 failed, 0 still without preview' "$LOG"; then
        exit 0
    elif grep -qE '^done: ' "$LOG"; then
        # The queue finished, but some assets are still without a preview.
        exit 1
    fi
    # No marker: the script died mid-run, keep the signal code.
fi

exit $EXIT_CODE
