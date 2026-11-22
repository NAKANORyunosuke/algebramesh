#!/bin/sh
# Issue #60: both drivers print an integer as a JSON number when it fits in 64
# bits and as a decimal string beyond, since JSON readers take larger numbers
# as floating point. Runs the drivers in the images built by
# run-cluster-e2e.sh (algebramesh-agent-oscar:dev, algebramesh-agent-macaulay2:dev)
# with their self-check request, which the backends never send.
set -eu
export MSYS_NO_PATHCONV=1

REQUEST='{"operation":"system.json_integers","values":["9223372036854775807","-9223372036854775808","9223372036854775808","-9223372036854775809"]}'
WANT='{"values":[9223372036854775807,-9223372036854775808,"9223372036854775808","-9223372036854775809"]}'

oscar=$(printf '%s\n' "$REQUEST" |
    docker run --rm -i --entrypoint julia algebramesh-agent-oscar:dev \
        --project=/opt/algebramesh-oscar --startup-file=no /opt/algebramesh/drivers/oscar/algebramesh.jl |
    sed -n 's/^ALGEBRAMESH_RESULT //p')
m2=$(printf '%s' "$REQUEST" |
    docker run --rm -i --entrypoint sh algebramesh-agent-macaulay2:dev -c \
        'cat > /tmp/request.json && M2 --script /opt/algebramesh/drivers/macaulay2/algebramesh.m2 /tmp/request.json' |
    sed -n 's/^ALGEBRAMESH_RESULT //p')
status=0
for driver in oscar m2; do
    eval "got=\$$driver"
    if [ "$got" = "$WANT" ]; then
        echo "ok   $driver: $got"
    else
        echo "FAIL $driver: $got, expected $WANT" >&2
        status=1
    fi
done
exit "$status"
