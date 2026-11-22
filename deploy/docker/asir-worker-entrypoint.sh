#!/bin/sh
# Connects ox_launch back to an algebramesh process (ReverseConnect) and serves one
# OpenXM session at a time, reconnecting after each session ends.
#
# Required environment:
#   ALGEBRAMESH_HOST           host name of the algebramesh process
#   ALGEBRAMESH_CONTROL_PORT   its control port
#   ALGEBRAMESH_DATA_PORT      its data port
# Optional:
#   OX_LAUNCH (default /OpenXM/lib/asir/ox_launch), OX_ASIR (default /OpenXM/bin/ox_asir)
#   RETRY_SECONDS (default 1), ONESHOT=1 to exit after the first session
set -eu

: "${ALGEBRAMESH_HOST:?ALGEBRAMESH_HOST is required}"
: "${ALGEBRAMESH_CONTROL_PORT:?ALGEBRAMESH_CONTROL_PORT is required}"
: "${ALGEBRAMESH_DATA_PORT:?ALGEBRAMESH_DATA_PORT is required}"
OX_LAUNCH="${OX_LAUNCH:-/OpenXM/lib/asir/ox_launch}"
OX_ASIR="${OX_ASIR:-/OpenXM/bin/ox_asir}"
RETRY_SECONDS="${RETRY_SECONDS:-1}"

for f in "$OX_LAUNCH" "$OX_ASIR"; do
    [ -x "$f" ] || { echo "asir-worker: not executable: $f" >&2; exit 1; }
done

while :; do
    # Arguments: host, 0 = connect mode, control port, data port, server, 0 = no display.
    if "$OX_LAUNCH" "$ALGEBRAMESH_HOST" 0 "$ALGEBRAMESH_CONTROL_PORT" "$ALGEBRAMESH_DATA_PORT" "$OX_ASIR" 0; then
        echo "asir-worker: session finished"
        [ "${ONESHOT:-0}" = 1 ] && exit 0
    fi
    sleep "$RETRY_SECONDS"
done
