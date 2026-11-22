#!/bin/sh
# Mathematical regression suite (tests/regression) on the Docker cluster with all
# backends. Rerun after updating any CAS image or driver.
set -eu
export MSYS_NO_PATHCONV=1

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$ROOT"
. tests/integration/build-env.sh
COMPOSE="docker compose -f deploy/compose/compose.cluster.yml"
cleanup() { $COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true; }
trap cleanup EXIT INT TERM

for service in certs worker-a worker-m2 worker-oscar python-client; do
    $COMPOSE build "$service"
done
$COMPOSE up -d scheduler worker-a worker-b worker-m2 worker-oscar
i=0
until [ "$($COMPOSE run --rm client workers | grep -c '"online": true')" -ge 4 ]; do
    i=$((i + 1)); [ "$i" -le 120 ] || { echo "workers did not come online" >&2; exit 1; }
    sleep 2
done
# ALGEBRAMESH_VALUES_SAVE and ALGEBRAMESH_VALUES_COMPARE name host directories:
# the values of this run are saved to the first and compared with the second
# (tests/regression/values.py), for example before and after a CAS update.
# ALGEBRAMESH_BACKENDS limits the run to some backends.
set --
if [ -n "${ALGEBRAMESH_VALUES_SAVE:-}" ]; then
    mkdir -p "$ALGEBRAMESH_VALUES_SAVE"
    dir=$(cd "$ALGEBRAMESH_VALUES_SAVE" && (pwd -W 2>/dev/null || pwd))
    set -- "$@" -v "$dir:/values/save" -e ALGEBRAMESH_VALUES_SAVE=/values/save
fi
if [ -n "${ALGEBRAMESH_VALUES_COMPARE:-}" ]; then
    dir=$(cd "$ALGEBRAMESH_VALUES_COMPARE" && (pwd -W 2>/dev/null || pwd))
    set -- "$@" -v "$dir:/values/compare:ro" -e ALGEBRAMESH_VALUES_COMPARE=/values/compare
fi
if [ -n "${ALGEBRAMESH_BACKENDS:-}" ]; then
    set -- "$@" -e ALGEBRAMESH_BACKENDS="$ALGEBRAMESH_BACKENDS"
fi
$COMPOSE run --rm "$@" python-client python /repo/tests/regression/run.py
