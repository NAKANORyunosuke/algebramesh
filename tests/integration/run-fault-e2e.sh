#!/bin/sh
# Fault injection on the Docker cluster (design §13.1 "障害注入, 再接続"):
#
# 1. A worker is killed while running a job with retry_policy if_deterministic.
#    The job becomes LOST after the heartbeat timeout and its retry succeeds on
#    the other worker.
# 2. The scheduler is killed and started again while a job runs. That job becomes
#    LOST, the agents register again, and new jobs succeed.
set -eu
export MSYS_NO_PATHCONV=1

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$ROOT"
. tests/integration/build-env.sh
COMPOSE="docker compose -f deploy/compose/compose.cluster.yml"
# python3 on Linux; on Windows "python3" may be a store stub that does not run.
if python3 -c "" >/dev/null 2>&1; then PY=python3; else PY=python; fi
CLIENT=algebramesh-fault-client

cleanup() {
    docker container rm -f "$CLIENT" >/dev/null 2>&1 || true
    $COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM
fail() { echo "FAIL: $*" >&2; $COMPOSE logs --tail 40 scheduler worker-a worker-b >&2; exit 1; }

mkdir -p .generated
$PY tests/integration/make_slow_job.py .generated/slow_retry.json if_deterministic
$PY tests/integration/make_slow_job.py .generated/slow_never.json never

for service in certs worker-a; do $COMPOSE build "$service"; done
$COMPOSE up -d scheduler worker-a worker-b
$COMPOSE run -d --name "$CLIENT" --entrypoint sleep client infinity >/dev/null
cli() { docker exec "$CLIENT" algebramesh "$@"; }
json() { $PY -c "import json,sys; d=json.load(sys.stdin); sys.stdout.write(str($1))"; }

wait_online() {
    i=0
    until [ "$(cli workers | json "sum(w['online'] for w in d)")" -ge "$1" ]; do
        i=$((i + 1)); [ "$i" -le 90 ] || fail "fewer than $1 workers online"
        sleep 1
    done
}
wait_running() {
    i=0
    while :; do
        state=$(cli status "$1" | json "d['state']")
        [ "$state" = RUNNING ] && return 0
        i=$((i + 1)); [ "$i" -le 300 ] || fail "job $1 never ran (state $state)"
        sleep 0.2
    done
}
service_of() {
    cli workers | json "next(w['name'] for w in d if w['agent_id'] == '$1')"
}

wait_online 2
echo "== 1. a worker dies while running a job"
id=$(cli submit /repo/.generated/slow_retry.json)
wait_running "$id"
agent=$(cli status "$id" | json "d['agent_id']")
victim=$(service_of "$agent")
echo "job $id runs on $victim; killing it"
$COMPOSE kill "$victim" >/dev/null
record=$(cli wait "$id" --timeout 600) || { echo "$record"; fail "retried job did not succeed"; }
first=$(cli status "$id")
[ "$(echo "$first" | json "d['state']")" = LOST ] || fail "first attempt is not LOST: $first"
retry=$(echo "$first" | json "d['retried_as']")
summary=$(echo "$record" | json "(d['state'], d['attempt'], d['retry_of'], d['result']['verified_product'])")
echo "first attempt LOST, retry $retry: $summary"
[ "$summary" = "('SUCCEEDED', 2, '$id', True)" ] || fail "unexpected retry record $summary"
new_agent=$(echo "$record" | json "d['agent_id']")
[ "$new_agent" != "$agent" ] || fail "the retry ran on the dead agent"

echo "== 2. the scheduler restarts while a job runs"
$COMPOSE up -d "$victim" >/dev/null
wait_online 2
id2=$(cli submit /repo/.generated/slow_never.json)
wait_running "$id2"
# SIGKILL, so the scheduler stops at once (as in a crash) and cannot record the
# job's result in between, however fast the job is.
$COMPOSE kill scheduler >/dev/null
$COMPOSE up -d scheduler >/dev/null
i=0
state=unknown
until state=$(cli status "$id2" 2>/dev/null | json "d['state']") && [ "$state" = LOST ]; do
    i=$((i + 1)); [ "$i" -le 60 ] || fail "job $id2 is $state, not LOST, after the restart"
    sleep 1
done
echo "job $id2 is LOST after the scheduler restart"
wait_online 2
after=$(cli submit /repo/examples/factor.json)
record=$(cli wait "$after" --timeout 300) || fail "job after restart failed"
echo "after the restart, job $after: $(echo "$record" | json "d['state']")"
echo "all fault checks passed"
