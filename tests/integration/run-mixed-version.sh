#!/bin/sh
# Schedulers of two versions in one Raft cluster on Docker (ADR 0020, Issue #46).
# scheduler-c runs an image built from ALGEBRAMESH_OLD_REF, a commit from before
# the agreement on features; scheduler-a, scheduler-b and the Asir worker run
# the checkout.
#
# 1. A new scheduler leads. It enables no feature, results record neither a
#    field of `backend_provenance` nor a guarantee (`result_guarantee`), and
#    the three schedulers hold the same job records.
# 2. The old scheduler leads: the same.
# 3. scheduler-c is updated to the checkout. The cluster enables every
#    feature of the checkout, results record the build of the agent and a
#    guarantee, and the records are still the same on the three schedulers.
# 4. A scheduler of the old image cannot join the cluster any more.
#
# No scheduler stops on the way ("raft stopped").
#
# The old image is ALGEBRAMESH_OLD_IMAGE, pulled if it is not here, when that
# is set (CI sets it). Otherwise it is built from ALGEBRAMESH_OLD_REF, which the
# history of the checkout must hold.
set -eu
export MSYS_NO_PATHCONV=1

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$ROOT"
. tests/integration/build-env.sh
# The last commit before the agreement on features (B0 in docs/roadmap.md).
# Name the tag of that release instead once it is made.
OLD_REF=${ALGEBRAMESH_OLD_REF:-0d0a7f2c1f514ab51ad28883b924f51c2054b0fa}
OLD_IMAGE_GIVEN=${ALGEBRAMESH_OLD_IMAGE:-}
ALGEBRAMESH_OLD_IMAGE=${ALGEBRAMESH_OLD_IMAGE:-algebramesh-cli:old}
export ALGEBRAMESH_OLD_IMAGE
PROJECT=algebramesh-mixed
# The checkout everywhere, and with scheduler-c and scheduler-d of the old image.
NEW="docker compose -p $PROJECT -f deploy/compose/compose.raft.yml"
COMPOSE="$NEW -f deploy/compose/compose.mixed-version.yml"
CLIENT=algebramesh-mixed-client
NODES="a b c"
. tests/integration/raft-helpers.sh

cleanup() {
    docker container rm -f "$CLIENT" >/dev/null 2>&1 || true
    $COMPOSE --profile join down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

for service in certs worker-a; do $NEW build "$service"; done
if [ -n "$OLD_IMAGE_GIVEN" ]; then
    docker image inspect "$ALGEBRAMESH_OLD_IMAGE" >/dev/null 2>&1 ||
        docker pull "$ALGEBRAMESH_OLD_IMAGE"
else
    git rev-parse --verify --quiet "$OLD_REF^{commit}" >/dev/null ||
        fail "the checkout has no commit $OLD_REF; set ALGEBRAMESH_OLD_IMAGE to an image of it"
    # Only the files of that commit, as its own Dockerfile builds them.
    git archive --format=tar "$OLD_REF" |
        docker build -f deploy/docker/algebramesh.Dockerfile --target cli \
            -t "$ALGEBRAMESH_OLD_IMAGE" -
fi
$COMPOSE up -d scheduler-a scheduler-b scheduler-c worker-a
$COMPOSE run -d --name "$CLIENT" --entrypoint sleep client infinity >/dev/null

PEER="--ca /pki/ca.pem --token-file /pki/peer.token"
# lead <node>: makes scheduler-<node> the leader with `server promote`.
lead() {
    j=0
    until [ "$(serving $NODES)" = "$1" ]; do
        j=$((j + 1)); [ "$j" -le 10 ] || fail "scheduler-$1 does not become the leader"
        cli server promote --server "https://scheduler-$1:50051" $PEER >/dev/null 2>&1 || true
        sleep 3
    done
}
# backend_of <job>: the `backend` of the job's record, as JSON.
backend_of() {
    cli wait "$1" --timeout 60 | json "json.dumps(d['backend'], sort_keys=True)"
}
no_provenance() {
    b=$(backend_of "$1")
    case $b in
    *driver_sha256* | *agent_build*) fail "job $1 records provenance before the agreement: $b" ;;
    esac
    [ "$(cli wait "$1" --timeout 60 | json "'guarantee' in d")" = False ] ||
        fail "job $1 records a guarantee before the agreement"
}
# Every given scheduler still runs, and none has stopped its Raft.
all_running() {
    for n in "$@"; do
        [ "$(docker inspect -f '{{.State.Running}}' "$PROJECT-scheduler-$n-1")" = true ] ||
            fail "scheduler-$n has stopped"
        if $COMPOSE logs --no-log-prefix "scheduler-$n" 2>&1 | grep -q "raft stopped"; then
            fail "the Raft of scheduler-$n stopped"
        fi
    done
}
enabled_everywhere() {
    for n in $NODES; do
        [ "$(status "$n" enabled_features)" = "$1" ] || return 1
    done
}
# Every scheduler has enabled every feature the checkout supports, which
# include the two this test reads in results. The list grows with each
# feature, so it is read from the schedulers, not written here.
all_enabled() {
    for n in $NODES; do
        [ "$(cli server status --server "https://scheduler-$n:50051" $PEER 2>/dev/null |
            json "sorted(d['enabled_features']) == sorted(d['features']) and {'backend_provenance', 'result_guarantee'} <= set(d['features'])")" = True ] ||
            return 1
    done
}

echo "== 1. a new scheduler leads"
leader=$(wait_serving $NODES)
[ -z "$(status c version)" ] || fail "scheduler-c is not of the old image: $(status c version)"
[ -n "$(status a version)" ] || fail "scheduler-a does not say its version"
if [ "$leader" = c ]; then
    lead a
    leader=a
fi
job=$(run_job)
no_provenance "$job"
# Longer than a few rounds of the leader's questions (ADR 0020).
sleep 6
enabled_everywhere "[]" || fail "a feature was enabled with an old scheduler in the cluster"
wait_agree "$leader" $NODES
digest=$(same_records $NODES)
all_running $NODES
echo "scheduler-$leader leads; no feature is enabled; jobs and digest of the records: $digest"

echo "== 2. the old scheduler leads"
lead c
job=$(run_job)
no_provenance "$job"
wait_agree c $NODES
digest=$(same_records $NODES)
all_running $NODES
echo "scheduler-c leads; jobs and digest of the records: $digest"

echo "== 3. scheduler-c is updated"
# Recreated from the image of the checkout, over the same data volume.
$NEW up -d --no-deps scheduler-c
i=0
until [ -n "$(status c version)" ] && [ "$(status c version)" != unreachable ]; do
    i=$((i + 1)); [ "$i" -le 60 ] || fail "scheduler-c does not come back"
    sleep 1
done
i=0
until all_enabled; do
    i=$((i + 1)); [ "$i" -le 60 ] || fail "the features are not enabled: $(status a enabled_features)"
    sleep 1
done
$COMPOSE logs --no-log-prefix scheduler-a scheduler-b scheduler-c 2>&1 |
    grep -q "the cluster agreed on features" || fail "the agreement is not in the logs"
# The agent hears of the agreement in a heartbeat; then its results carry the
# build (the Asir backend reads no driver file, so no driver_sha256).
k=0
while :; do
    job=$(run_job)
    b=$(backend_of "$job")
    case $b in *agent_build*) break ;; esac
    k=$((k + 1)); [ "$k" -le 10 ] || fail "results record no agent build: $b"
    sleep 1
done
build=$(cli wait "$job" --timeout 60 | json "d['backend']['agent_build']")
if [ -n "${ALGEBRAMESH_BUILD_COMMIT:-}" ]; then
    want=$ALGEBRAMESH_BUILD_COMMIT
    [ "${ALGEBRAMESH_BUILD_CLEAN:-}" = 1 ] || want="$want-modified"
    [ "$build" = "$want" ] || fail "agent_build is $build, not $want"
fi
summary=$(cli wait "$job" --timeout 60 | json "d['guarantee']['summary']")
[ "$summary" = partially_verified ] || fail "job $job records the summary $summary"
leader=$(wait_serving $NODES)
wait_agree "$leader" $NODES
digest=$(same_records $NODES)
all_running $NODES
echo "every feature is enabled; job $job records agent_build $build and a guarantee ($summary); jobs and digest: $digest"

echo "== 4. a scheduler of the old image cannot join"
$COMPOSE run --rm --no-deps --user 0 --entrypoint algebramesh certs certs server \
    --ca-dir /pki --host scheduler-d --out /pki --name server-d >/dev/null 2>&1 ||
    fail "cannot issue a certificate for scheduler-d"
$COMPOSE run --rm --no-deps --user 0 --entrypoint chmod certs 0644 \
    /pki/server-d.pem /pki/server-d-key.pem >/dev/null 2>&1
$COMPOSE --profile join up -d scheduler-d >/dev/null 2>&1
PEERS=https://scheduler-a:50051,https://scheduler-b:50051,https://scheduler-c:50051
if out=$(cli server add-member --id 4 --name scheduler-d --url https://scheduler-d:50051 \
    --server "$PEERS" $PEER 2>&1); then
    fail "the old scheduler-d was added: $out"
fi
case $out in
*"cannot use the features"*) ;;
*) fail "scheduler-d was refused for another reason: $out" ;;
esac
voters=$(cli server members --server "$PEERS" $PEER |
    json "' '.join(sorted(m['name'] for m in d['members'] if m['voter']))")
[ "$voters" = "scheduler-a scheduler-b scheduler-c" ] || fail "voters: $voters"
all_running $NODES
echo "refused: $out"
echo "all mixed-version checks passed"
