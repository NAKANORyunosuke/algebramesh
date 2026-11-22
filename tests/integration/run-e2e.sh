#!/bin/sh
# Runs job files end to end: the algebramesh CLI in one container waits on a private
# network, and a Risa/Asir worker reverse-connects with ox_launch.
#
#   tests/integration/run-e2e.sh [job.json ...]
#
# Paths are relative to the repository root. Exits non-zero if any job does not
# finish in state SUCCEEDED.
set -eu
# Keep container paths as they are when run from Git Bash on Windows.
export MSYS_NO_PATHCONV=1

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$ROOT"
. tests/integration/build-env.sh
COMPOSE="docker compose -f deploy/compose/compose.e2e.yml"
[ "$#" -gt 0 ] || set -- examples/factor.json examples/factor_rational.json tests/fixtures/factor_x40_minus_y40.json

$COMPOSE build algebramesh
status=0
for job in "$@"; do
    echo "== $job"
    out=$(JOB="$job" $COMPOSE up --abort-on-container-exit --exit-code-from algebramesh 2>/dev/null \
        | sed -n 's/^algebramesh-1  | //p')
    printf '%s\n' "$out" | grep -E '"(state|verified_product|display)"' || true
    # Only the top-level state (two-space indent) decides; history entries are nested.
    printf '%s\n' "$out" | grep -q '^  "state": "SUCCEEDED"' || status=1
done
$COMPOSE down --remove-orphans >/dev/null 2>&1 || true
exit "$status"
