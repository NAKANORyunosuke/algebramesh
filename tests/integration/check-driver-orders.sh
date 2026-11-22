#!/bin/sh
# Issue #64: the leading terms of both drivers agree with MonomialOrder::compare
# in every kind of monomial order. The cases are drawn in Rust from a fixed
# seed: the order_cases example of algebramesh-math-types writes, for rings of
# 0, 1, 4 and 6 variables, polynomials for lex, grlex, grevlex, weight orders
# (with the weights 0 and MAX_WEIGHT) and block orders, with their leading
# exponents by compare: random ones, and ones whose terms share a degree (or
# the exponents of the blocks before a grlex or grevlex block), where the
# orders differ most. The examples macaulay2_leading_terms and
# oscar_leading_terms of the backends write them as requests
# system.leading_terms, with the order as that backend sends it (order_items),
# and the answers expected. The drivers run in the images built by
# run-cluster-e2e.sh (algebramesh-agent-macaulay2:dev,
# algebramesh-agent-oscar:dev) with this self-check request, which the backends
# never send. The drivers of the working tree are mounted over those of the
# images, so that a change of a driver is checked without rebuilding its image;
# DRIVERS names another directory of drivers (macaulay2/algebramesh.m2 and
# oscar/algebramesh.jl) to check instead, such as a copy with a deliberate
# fault, which the cases must find. Needs cargo besides Docker.
set -eu
export MSYS_NO_PATHCONV=1

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
cd "$ROOT"
# Docker and cargo on Windows need the Windows form of the paths.
DRIVERS=$(cd "${DRIVERS:-$ROOT/drivers}" && (pwd -W 2>/dev/null || pwd))
WORK=$(cd "$(mktemp -d)" && (pwd -W 2>/dev/null || pwd))
trap 'rm -rf "$WORK"' EXIT
trap 'exit 1' INT TERM

cargo run -q --locked -p algebramesh-math-types --example order_cases > "$WORK/cases"
for backend in macaulay2 oscar; do
    cargo run -q --locked -p "algebramesh-backend-$backend" --example "${backend}_leading_terms" -- \
        "$WORK/$backend.requests" "$WORK/$backend.expected" < "$WORK/cases"
done

# One answer line for each request: the reply, or the error of the driver.
answers() {
    sed -n -e 's/^ALGEBRAMESH_RESULT //p' -e '/^ALGEBRAMESH_ERROR /p'
}

# The two drivers run at the same time, each in a container named after this
# run, which an interrupted run stops.
M2_CONTAINER=algebramesh-check-orders-macaulay2-$$
OSCAR_CONTAINER=algebramesh-check-orders-oscar-$$
trap 'docker kill "$M2_CONTAINER" "$OSCAR_CONTAINER" > /dev/null 2>&1; exit 1' INT TERM

# Macaulay2 runs once for each request, as the backend runs it.
M2_DRIVER=/opt/algebramesh/drivers/macaulay2/algebramesh.m2
docker run --rm -i --name "$M2_CONTAINER" -v "$DRIVERS/macaulay2/algebramesh.m2:$M2_DRIVER:ro" \
    --entrypoint timeout algebramesh-agent-macaulay2:dev 600 sh -c '
        while IFS= read -r request; do
            printf "%s\n" "$request" > /tmp/request.json
            M2 --script "$0" /tmp/request.json > /tmp/answer 2>&1 || true
            grep -m 1 -E "^ALGEBRAMESH_(RESULT|ERROR) " /tmp/answer ||
                echo "ALGEBRAMESH_ERROR no answer: $(tail -n 3 /tmp/answer | tr "\n" " ")"
        done' "$M2_DRIVER" < "$WORK/macaulay2.requests" | answers > "$WORK/macaulay2.answers" &

# OSCAR answers every request in one process, as for the backend.
OSCAR_DRIVER=/opt/algebramesh/drivers/oscar/algebramesh.jl
docker run --rm -i --name "$OSCAR_CONTAINER" -v "$DRIVERS/oscar/algebramesh.jl:$OSCAR_DRIVER:ro" \
    --entrypoint timeout algebramesh-agent-oscar:dev 600 \
    julia --project=/opt/algebramesh-oscar --startup-file=no "$OSCAR_DRIVER" \
    < "$WORK/oscar.requests" | answers > "$WORK/oscar.answers"
wait
trap 'exit 1' INT TERM

# Compares the answers of a driver with those expected, polynomial by
# polynomial; prints the differences and a summary line, and fails on any
# difference.
compare() {
    awk -v driver="$1" '
        # The entries of a reply {"leading_exponents":[...]} into e, each null
        # or an exponent vector, and their number; -1 for any other line.
        function entries(s, e) {
            if (s !~ /^\{"leading_exponents":\[.*\]\}$/) return -1
            s = substr(s, 23, length(s) - 24)
            if (s == "") return 0
            gsub(/null,/, "null ", s)
            gsub(/\],/, "] ", s)
            return split(s, e, " ")
        }
        FNR == 1 { file++ }
        file == 1 {
            match($0, /"order":.*,"polynomials":/)
            order[FNR] = substr($0, RSTART + 8, RLENGTH - 23)
            cases = FNR
        }
        file == 2 { want[FNR] = $0 }
        file == 3 { got[FNR] = $0 }
        END {
            for (i = 1; i <= cases; i++) {
                n = entries(want[i], w)
                polynomials += n
                answer = (i in got) ? got[i] : "no answer"
                if (entries(answer, g) != n) {
                    wrong += n
                    printf "FAIL %s: order %s: %s\n", driver, order[i], answer
                    continue
                }
                for (j = 1; j <= n; j++) if (g[j] != w[j]) {
                    wrong++
                    printf "FAIL %s: order %s, polynomial %d: %s, expected %s\n",
                        driver, order[i], j - 1, g[j], w[j]
                }
            }
            printf "%s %s: %d orders, %d polynomials, %d differences\n",
                (wrong ? "FAIL" : "ok  "), driver, cases, polynomials, wrong
            exit (wrong > 0)
        }' "$WORK/$1.requests" "$WORK/$1.expected" "$WORK/$1.answers"
}

status=0
for backend in macaulay2 oscar; do
    compare "$backend" || status=1
done
exit "$status"
