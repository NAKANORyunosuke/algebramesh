#!/bin/sh
# v0.2 end-to-end test: a scheduler and two Asir worker agents on a private network;
# independent jobs are distributed over both workers through gRPC over TLS.
set -eu
# Keep container paths as they are when run from Git Bash on Windows.
export MSYS_NO_PATHCONV=1

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$ROOT"
. tests/integration/build-env.sh
COMPOSE="docker compose -f deploy/compose/compose.cluster.yml"
cleanup() { $COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true; }
trap cleanup EXIT INT TERM

# One image at a time: parallel BuildKit exports of these large images have
# failed with missing snapshots.
for service in certs worker-a worker-m2 worker-oscar python-client; do
    $COMPOSE build "$service"
done
$COMPOSE up -d scheduler worker-a worker-b worker-m2 worker-oscar
status=0
$COMPOSE run --rm --entrypoint sh client /repo/tests/integration/cluster-check.sh || status=$?
if [ "$status" -eq 0 ]; then
    $COMPOSE run --rm python-client python /repo/bindings/python/tests/test_cluster.py || status=$?
fi
if [ "$status" -ne 0 ]; then
    $COMPOSE logs --tail 80 scheduler worker-a worker-b worker-m2 worker-oscar >&2
fi
exit "$status"
