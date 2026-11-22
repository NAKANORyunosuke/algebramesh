# Functions of the Raft tests on Docker (run-raft-e2e.sh, run-mixed-version.sh).
# Sourced from the root of the checkout after setting COMPOSE (the compose
# command), PROJECT (its project name) and CLIENT (the name of a running client
# container on `mesh`).

if python3 -c "" >/dev/null 2>&1; then PY=python3; else PY=python; fi

fail() {
    echo "FAIL: $*" >&2
    $COMPOSE --profile join logs --no-log-prefix 2>&1 | grep -v openraft | tail -40 >&2
    exit 1
}
cli() { docker exec "$CLIENT" algebramesh "$@"; }
json() { $PY -c "import json,sys; d=json.load(sys.stdin); sys.stdout.write(str($1))"; }
# status <node> [field]: a field of `server status` of scheduler-<node>, seen
# from the client on `mesh`.
status() {
    cli server status --server "https://scheduler-$1:50051" --ca /pki/ca.pem \
        --token-file /pki/peer.token 2>/dev/null | json "d['${2:-role}']" || echo unreachable
}
# The one node that serves calls, among the given nodes.
serving() {
    found=""
    for n in "$@"; do
        [ "$(status "$n" serving)" = True ] && found="$found $n"
    done
    set -- $found
    [ $# -eq 1 ] && echo "$1"
}
wait_serving() {
    i=0
    until leader=$(serving "$@"); do
        i=$((i + 1)); [ "$i" -le 60 ] || fail "no single leader among $*"
        sleep 1
    done
    echo "$leader"
}
wait_online() {
    i=0
    until [ "$(cli workers 2>/dev/null | json "sum(w['online'] for w in d)" || echo 0)" -ge 1 ]; do
        i=$((i + 1)); [ "$i" -le 90 ] || fail "the worker is not online"
        sleep 1
    done
}
run_job() {
    wait_online
    id=$(cli submit /repo/examples/factor.json) || fail "cannot submit"
    cli wait "$id" --timeout 300 >/dev/null || fail "job $id failed"
    echo "$id"
}
# Waits until every given node has applied as many changes as the leader $1.
wait_agree() {
    leader=$1
    i=0
    while :; do
        want=$(status "$leader" last_seq)
        ok=1
        for n in "$@"; do [ "$(status "$n" last_seq)" = "$want" ] || ok=0; done
        [ $ok = 1 ] && return 0
        i=$((i + 1)); [ "$i" -le 60 ] || fail "the stores do not agree"
        sleep 1
    done
}
# records <node>: the number of jobs in the state of scheduler-<node> and the
# SHA-256 of their records (tests/integration/records_digest.py).
records() {
    docker run --rm -i --network none -v "${PROJECT}_state-$1:/state" \
        python:3.11-slim-bookworm python3 - /state/metadata.sqlite3 \
        <tests/integration/records_digest.py
}
# same_records <node>...: the given schedulers, which have applied the same
# changes (wait_agree), hold the same job records, field for field (Issue #46).
# Prints the number of jobs and the digest.
same_records() {
    _want=$(records "$1") || fail "cannot read the records of scheduler-$1"
    for _n in "$@"; do
        _got=$(records "$_n") || fail "cannot read the records of scheduler-$_n"
        [ "$_got" = "$_want" ] ||
            fail "scheduler-$_n holds other records ($_got) than scheduler-$1 ($_want)"
    done
    echo "$_want"
}
