"""Writes a polynomial.factor job that keeps an Asir worker busy for several seconds:
the expansion of (x+y+z+1)^k (x-y+2z+3)^k, whose factorization is known.

    python make_slow_job.py OUT.json RETRY_POLICY [K]
"""

import json
import sys
from collections import defaultdict


def mul(a, b):
    r = defaultdict(int)
    for ea, ca in a.items():
        for eb, cb in b.items():
            r[tuple(i + j for i, j in zip(ea, eb))] += ca * cb
    return {e: c for e, c in r.items() if c}


def power(p, n):
    r = {(0, 0, 0): 1}
    base = p
    while n:
        if n & 1:
            r = mul(r, base)
        n >>= 1
        if n:
            base = mul(base, base)
    return r


def main():
    out, retry = sys.argv[1], sys.argv[2]
    k = int(sys.argv[3]) if len(sys.argv) > 3 else 34
    f = mul(
        power({(1, 0, 0): 1, (0, 1, 0): 1, (0, 0, 1): 1, (0, 0, 0): 1}, k),
        power({(1, 0, 0): 1, (0, 1, 0): -1, (0, 0, 1): 2, (0, 0, 0): 3}, k),
    )
    job = {
        "schema_version": 1,
        "backend": "asir",
        "operation": "polynomial.factor",
        "arguments": {
            "ring": {"domain": "QQ", "variables": ["x", "y", "z"], "order": "grevlex"},
            "expression": {
                "type": "polynomial",
                "terms": [{"coefficient": str(c), "exponents": list(e)} for e, c in f.items()],
            },
        },
        "limits": {"wall_seconds": 600},
        "retry_policy": retry,
    }
    with open(out, "w", newline="\n") as fh:
        json.dump(job, fh)
    print(f"{out}: {len(f)} terms")


if __name__ == "__main__":
    main()
