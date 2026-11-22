#!/bin/sh
# Wall-clock time of independent jobs on 1, 2, 4, ... one-slot Asir workers
# (design §13.2 performance evaluation). Prints one line per worker count:
#   workers=N jobs=M seconds=T jobs_per_second=R
# The workers share this machine's CPUs, so the numbers measure scheduling and
# protocol overhead, not the speed-up of separate hosts.
#
#   sh tests/bench/run-bench.sh [WORKER_COUNTS] [JOBS] [K]
set -eu
export MSYS_NO_PATHCONV=1

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$ROOT"
COMPOSE="docker compose -f deploy/compose/compose.cluster.yml --profile bench"
if python3 -c "" >/dev/null 2>&1; then PY=python3; else PY=python; fi
COUNTS="${1:-1 2 4}"
JOBS="${2:-16}"
K="${3:-28}"
CLIENT=algebramesh-bench-client

cleanup() {
    docker container rm -f "$CLIENT" >/dev/null 2>&1 || true
    $COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

mkdir -p .generated
$PY tests/integration/make_slow_job.py .generated/bench_job.json never "$K" >&2
for service in certs worker-a; do $COMPOSE build "$service" >&2; done

for n in $COUNTS; do
    cleanup
    $COMPOSE up -d scheduler >&2
    $COMPOSE up -d --scale bench-worker="$n" bench-worker >&2
    $COMPOSE run -d --name "$CLIENT" --entrypoint sleep client infinity >/dev/null
    i=0
    until [ "$(docker exec "$CLIENT" algebramesh workers | grep -c '"online": true')" -ge "$n" ]; do
        i=$((i + 1)); [ "$i" -le 120 ] || { echo "workers did not come online" >&2; exit 1; }
        sleep 1
    done
    start=$(date +%s%N)
    ids=""
    j=0
    while [ "$j" -lt "$JOBS" ]; do
        ids="$ids $(docker exec "$CLIENT" algebramesh submit /repo/.generated/bench_job.json)"
        j=$((j + 1))
    done
    for id in $ids; do
        # grep without -q reads all input, so the writer never sees a closed pipe.
        docker exec "$CLIENT" algebramesh wait "$id" --timeout 900 | grep '^  "state": "SUCCEEDED"' >/dev/null \
            || { echo "job $id failed" >&2; exit 1; }
    done
    end=$(date +%s%N)
    $PY -c "t=($end-$start)/1e9; print(f'workers=$n jobs=$JOBS seconds={t:.1f} jobs_per_second={$JOBS/t:.2f}')"
done
