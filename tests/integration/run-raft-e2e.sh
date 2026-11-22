#!/bin/sh
# A Raft cluster of three schedulers on Docker (ADR 0013):
#
# 1. The schedulers elect a leader; a job runs through it.
# 2. The leader is cut off from the others (docker network disconnect). The other
#    two elect a new leader and jobs go on there. The cut-off leader, still
#    reachable on its own network, accepts no job: neither one submitted right
#    after the cut (it cannot be committed) nor one submitted later (the leader
#    knows it has lost the majority).
# 3. The network heals: the old leader follows and all three stores agree,
#    record for record (Issue #46).
# 4. The new leader is killed; the remaining two elect another and jobs go on.
#    The killed scheduler starts again and catches up.
# 5. A fourth scheduler, with a certificate from `certs server`, joins with
#    `server add-member`; then the leader leaves with `server remove-member`,
#    and the other three, the new one among them, go on.
set -eu
export MSYS_NO_PATHCONV=1

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$ROOT"
. tests/integration/build-env.sh
PROJECT=algebramesh-raft
COMPOSE="docker compose -f deploy/compose/compose.raft.yml"
CLIENT=algebramesh-raft-client
PROBE=algebramesh-raft-probe
NODES="a b c"
. tests/integration/raft-helpers.sh

cleanup() {
    docker container rm -f "$CLIENT" "$PROBE" >/dev/null 2>&1 || true
    $COMPOSE --profile join down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

for service in certs worker-a; do $COMPOSE build "$service"; done
$COMPOSE up -d scheduler-a scheduler-b scheduler-c worker-a
$COMPOSE run -d --name "$CLIENT" --entrypoint sleep client infinity >/dev/null

echo "== 1. a leader is elected and runs a job"
first_leader=$(wait_serving $NODES)
first=$(run_job)
echo "scheduler-$first_leader leads (term $(status "$first_leader" epoch)); job $first succeeded"

echo "== 2. the leader is cut off"
others=$(echo $NODES | tr ' ' '
' | grep -v "^$first_leader$" | tr '
' ' ')
# A client that reaches only that scheduler, on its own network.
docker run -d --name "$PROBE" --network "${PROJECT}_side-$first_leader"     -v "${PROJECT}_pki:/pki:ro" -v "$ROOT:/repo:ro" -w /repo     -e ALGEBRAMESH_SERVER="https://scheduler-$first_leader:50051"     -e ALGEBRAMESH_CA=/pki/ca.pem -e ALGEBRAMESH_TOKEN_FILE=/pki/client.token     --entrypoint sleep algebramesh-cli:dev infinity >/dev/null
docker network disconnect "${PROJECT}_mesh" "${PROJECT}-scheduler-$first_leader-1"
# Right after the cut the leader may still take the job, but cannot commit it.
early=$(mktemp)
(docker exec "$PROBE" algebramesh submit /repo/examples/factor.json >"$early" 2>&1     && echo accepted >>"$early" || echo refused >>"$early") &
early_pid=$!
second_leader=$(wait_serving $others)
term=$(status "$second_leader" epoch)
echo "scheduler-$second_leader leads the majority (term $term)"
second=$(run_job)
[ "$(cli status "$first" | json "d['state']")" = SUCCEEDED ] || fail "job $first is gone"
echo "job $second succeeded on the majority side"
wait "$early_pid" || true
grep -q '^refused$' "$early" || fail "the cut-off scheduler took a job right after the cut: $(cat "$early")"
echo "right after the cut: $(head -1 "$early")"
if out=$(docker exec "$PROBE" algebramesh submit /repo/examples/factor.json 2>&1); then
    fail "the cut-off scheduler accepted a job: $out"
fi
echo "later: $out"
rm -f "$early"

echo "== 3. the network heals"
docker network connect --alias "scheduler-$first_leader" "${PROJECT}_mesh" \
    "${PROJECT}-scheduler-$first_leader-1"
i=0
until [ "$(status "$first_leader")" = follower ]; do
    i=$((i + 1)); [ "$i" -le 60 ] || fail "scheduler-$first_leader does not follow"
    sleep 1
done
wait_agree "$second_leader" $NODES
digest=$(same_records $NODES)
echo "scheduler-$first_leader follows; the three stores agree (last_seq $(status "$second_leader" last_seq); jobs and digest $digest)"

echo "== 4. the leader is killed"
$COMPOSE kill "scheduler-$second_leader" >/dev/null
rest=$(echo $NODES | tr ' ' '\n' | grep -v "^$second_leader$" | tr '\n' ' ')
third_leader=$(wait_serving $rest)
third=$(run_job)
echo "scheduler-$third_leader leads (term $(status "$third_leader" epoch)); job $third succeeded"
$COMPOSE start "scheduler-$second_leader" >/dev/null
wait_agree "$third_leader" $NODES
for id in "$first" "$second" "$third"; do
    [ "$(cli status "$id" | json "d['state']")" = SUCCEEDED ] || fail "job $id is not SUCCEEDED"
done
same_records $NODES >/dev/null
echo "scheduler-$second_leader is back and caught up"

echo "== 5. a scheduler joins and the leader leaves"
id_of() { case $1 in a) echo 1 ;; b) echo 2 ;; c) echo 3 ;; d) echo 4 ;; esac; }
$COMPOSE run --rm --no-deps --user 0 --entrypoint algebramesh certs certs server     --ca-dir /pki --host scheduler-d --out /pki --name server-d >/dev/null 2>&1     || fail "cannot issue a certificate for scheduler-d"
$COMPOSE run --rm --no-deps --user 0 --entrypoint chmod certs 0644     /pki/server-d.pem /pki/server-d-key.pem >/dev/null 2>&1
$COMPOSE --profile join up -d scheduler-d >/dev/null 2>&1
PEERS=https://scheduler-a:50051,https://scheduler-b:50051,https://scheduler-c:50051
member() { cli server "$@" --server "$PEERS" --ca /pki/ca.pem --token-file /pki/peer.token; }
voters() { member members | json "' '.join(sorted(m['name'] for m in d['members'] if m['voter']))"; }
member add-member --id 4 --name scheduler-d --url https://scheduler-d:50051 >/dev/null     || fail "cannot add scheduler-d"
[ "$(voters)" = "scheduler-a scheduler-b scheduler-c scheduler-d" ] || fail "voters: $(voters)"
wait_agree "$third_leader" a b c d
same_records a b c d >/dev/null
echo "scheduler-d is a voter and has caught up"
leaving=$third_leader
member remove-member --id "$(id_of "$leaving")" >/dev/null || fail "cannot remove scheduler-$leaving"
remaining=$(echo a b c d | tr ' ' '
' | grep -v "^$leaving$" | tr '
' ' ')
fourth_leader=$(wait_serving $remaining)
fourth=$(run_job)
expected=$(for n in $remaining; do echo "scheduler-$n"; done | tr '
' ' ' | sed 's/ $//')
PEERS=$(for n in $remaining; do printf 'https://scheduler-%s:50051,' "$n"; done | sed 's/,$//')
[ "$(voters)" = "$expected" ] || fail "voters after the removal: $(voters)"
$COMPOSE stop "scheduler-$leaving" >/dev/null
echo "scheduler-$leaving left; scheduler-$fourth_leader leads ($expected); job $fourth succeeded"
echo "all Raft checks passed"
