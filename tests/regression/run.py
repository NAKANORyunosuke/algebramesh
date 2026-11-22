"""Mathematical regression suite (design §13.1).

Runs every case in tests/regression/cases on each listed backend through the
scheduler and compares the typed result with the fixed expectation. Prints the CAS
version that produced each result, so the suite can be rerun after a CAS update.

Uses ALGEBRAMESH_SERVER, ALGEBRAMESH_CA and ALGEBRAMESH_TOKEN_FILE. With
ALGEBRAMESH_BACKENDS (comma-separated), only the cases of those backends run.

With ALGEBRAMESH_VALUES_SAVE=<dir>, the value of every job is saved there as
canonical JSON (values.py). With ALGEBRAMESH_VALUES_COMPARE=<dir>, every value
is compared with the one saved there by an earlier run (another version), and
each difference is a failure.

The GKZ examples of gkz.indicial and gkz.series carry a category (generic,
resonant, rank_jump, inhomogeneous) and a reference: their holonomic rank as
computed independently of the backend (by Singular, see references/, and from
the literature). The rank the backend reports must agree with it and, by the
category, with vol(A), which this script computes from A.

The examples of ideal.groebner_basis fix the reduced Groebner basis of the
ideal in the order of the ring. It is unique, so every backend must return the
same basis, element by element (Issue #66). The examples of ideal.normal_form
fix the remainder and the basis, both unique in the order of the ring; those of
ideal.membership fix `member` and, for a polynomial outside the ideal with a
certificate, the basis and the remainder in it (Issue #67). Their expected
values were computed by hand and checked with Macaulay2 and OSCAR run directly,
apart from the drivers. The cases whose name ends in `_s_pairs` repeat earlier
cases with the certificate `s_pairs` and the same expected values: with the
lcm representations of the S-pairs, gb.groebner is checked, and so are, in
effect, normal_form.unique and membership.not_in (Issue #68).

The examples of module.syzygies, complex.free_resolution and
ideal.hilbert_series (kind `module`, Issues #97, #98) fix what is unique: the
number of syzygies the backends give for a minimal case, the degrees of the
free modules and the graded Betti numbers of a minimal resolution, and the
Hilbert series in lowest terms; and the level and the effective level of every
claim, and the claims the result does not make. Their expected values were
computed by hand and checked with Macaulay2 and OSCAR run directly, apart from
the drivers, so that both backends agree on each Betti table.

The examples of simplicial.homology (kind `topology`, Issue #103) fix the face
counts, the homology groups and the Euler characteristic, which are unique and
known for these triangulations; every claim of their results is checked.
Exit status 0 only if every case passes on every backend.
"""

import json
import os
import sys
from fractions import Fraction
from math import gcd
from pathlib import Path

from algebramesh import Client, JobFailed

sys.path.insert(0, str(Path(__file__).parent))
import values  # noqa: E402

CASES = Path(__file__).with_name("cases")


def check_factor(case, value):
    exp = case["expect"]
    got = {
        "constant": value["constant"],
        "factors": sorted(
            ({"display": f["display"], "multiplicity": f["multiplicity"]} for f in value["factors"]),
            key=lambda f: (f["display"], f["multiplicity"]),
        ),
    }
    problems = []
    if not value.get("verified_product"):
        problems.append("product not verified")
    if got != exp:
        problems.append(f"expected {exp}, got {got}")
    return problems


def check_gkz(case, value):
    exp = case["expect"]
    checks = value["checks"]
    problems = []
    for name in ("euler_operators_match", "toric_binomials_valid", "generated_by_toric_and_euler"):
        if not checks.get(name):
            problems.append(f"{name} is false")
    euler = sorted(g["display"] for g in value["euler_operators"])
    if euler != sorted(exp["euler"]):
        problems.append(f"euler operators {euler}")
    if len(value["toric_generators"]) != exp["toric_count"]:
        problems.append(f"{len(value['toric_generators'])} toric generators")
    membership = [m["in_ideal"] for m in checks["expected_in_ideal"]]
    if membership != exp["expected_in_ideal"]:
        problems.append(f"membership {membership}")
    return problems


def normalized_volume(a):
    """vol(A): d! times the Euclidean volume of conv(0, a_1, ..., a_n), in units
    of the lattice ZA, for d = 1 and d = 2 (None otherwise). Computed here from
    A alone, independently of any CAS."""
    if len(a) == 1:
        row = a[0]
        g = 0
        for x in row:
            g = gcd(g, abs(x))
        return (max(row + [0]) - min(row + [0])) // g
    if len(a) != 2:
        return None
    points = sorted(set([(0, 0)] + list(zip(a[0], a[1]))))

    def cross(o, p, q):
        return (p[0] - o[0]) * (q[1] - o[1]) - (p[1] - o[1]) * (q[0] - o[0])

    # Andrew's monotone chain; twice the area by the shoelace formula.
    lower, upper = [], []
    for pt in points:
        while len(lower) >= 2 and cross(lower[-2], lower[-1], pt) <= 0:
            lower.pop()
        lower.append(pt)
    for pt in reversed(points):
        while len(upper) >= 2 and cross(upper[-2], upper[-1], pt) <= 0:
            upper.pop()
        upper.append(pt)
    hull = lower[:-1] + upper[:-1]
    twice_area = abs(sum(p[0] * q[1] - q[0] * p[1] for p, q in zip(hull, hull[1:] + hull[:1])))
    # [Z^2 : ZA] is the gcd of the 2 x 2 minors.
    index = 0
    n = len(a[0])
    for i in range(n):
        for j in range(i + 1, n):
            index = gcd(index, abs(a[0][i] * a[1][j] - a[0][j] * a[1][i]))
    return twice_area // index


# How the holonomic rank of each kind of example relates to vol(A):
# generic: beta non-resonant, A homogeneous (GKZ, Adolphson): rank = vol(A).
# resonant: beta resonant, A homogeneous with a Cohen-Macaulay toric ring
#   (Matusevich-Miller-Walther): rank = vol(A).
# rank_jump: beta in the exceptional set: rank > vol(A).
# inhomogeneous: A not homogeneous, beta non-resonant (Adolphson): rank = vol(A).
CATEGORIES = ("generic", "resonant", "rank_jump", "inhomogeneous")


def check_reference(case, value):
    """The rank against the recorded independent references and vol(A)."""
    ref = case["reference"]
    problems = []
    category = case["category"]
    if category not in CATEGORIES:
        problems.append(f"unknown category {category}")
    if value["holonomic_rank"] != ref["holonomic_rank"]:
        problems.append(
            f"holonomic rank {value['holonomic_rank']}, the references give {ref['holonomic_rank']}"
        )
    volume = normalized_volume(case["arguments"]["A"])
    if volume is not None:
        if category == "rank_jump":
            if not ref["holonomic_rank"] > volume:
                problems.append(f"no rank jump: vol(A) = {volume}")
        elif ref["holonomic_rank"] != volume:
            problems.append(f"vol(A) = {volume}, not the rank {ref['holonomic_rank']}")
    homogeneous = value["parameters"]["homogeneous"]
    if homogeneous != (category != "inhomogeneous"):
        problems.append(f"homogeneous is {homogeneous} for a {category} example")
    return problems


def check_local(case, value):
    """gkz.indicial and gkz.series: exponents, rank, resonance and series status."""
    exp = case["expect"]
    problems = check_reference(case, value)
    if not value["checks"].get("exponents_are_roots"):
        problems.append("exponents not checked as roots")
    if value["holonomic_rank"] != exp["holonomic_rank"]:
        problems.append(f"holonomic rank {value['holonomic_rank']}")
    if value["parameters"]["resonant"] != exp["resonant"]:
        problems.append(f"resonant {value['parameters']['resonant']}")
    got = sorted((tuple(e["point"]), e["multiplicity"]) for e in value["exponents"])
    want = sorted((tuple(e["point"]), e["multiplicity"]) for e in exp["exponents"])
    if got != want:
        problems.append(f"exponents {got}")
    if case["kind"] == "gkz_series":
        series = {tuple(s["point"]): s for s in value["series"]}
        for e in exp["exponents"]:
            s = series.get(tuple(e["point"]))
            if s is None:
                problems.append(f"no series for {e['point']}")
                continue
            if s["status"] != e["status"]:
                problems.append(f"{e['point']}: status {s['status']} ({s.get('reason', '')})")
            want_c = e.get("coefficients", [])
            got_c = [t["coefficient"] for t in s.get("terms", [])][: len(want_c)]
            if got_c != want_c:
                problems.append(f"{e['point']}: coefficients {got_c}")
            # Logarithmic exponents: one entry per solution, in order.
            want_s = e.get("solutions", [])
            got_s = s.get("solutions", [])
            if len(got_s) != len(want_s):
                problems.append(f"{e['point']}: {len(got_s)} solutions ({s.get('reason', '')})")
            for i, (g, w) in enumerate(zip(got_s, want_s)):
                if g["status"] != w["status"] or g.get("direction") != w.get("direction"):
                    problems.append(f"{e['point']} solution {i}: {g['status']} {g.get('direction')}")
                want_c = w.get("coefficients", [])
                got_c = [t["coefficient"] for t in g.get("terms", [])][: len(want_c)]
                if got_c != want_c:
                    problems.append(f"{e['point']} solution {i}: coefficients {got_c}")
                # Canonical unless the case says otherwise (Issue #88: a
                # solution with a logarithm at another exponent is not).
                if g.get("canonical") is not w.get("canonical", True):
                    problems.append(f"{e['point']} solution {i}: canonical {g.get('canonical')}")
    return problems


def check_matrix(case, value):
    """The fields of the value the case fixes (Issue #57). `basis_vectors` is
    the number of columns of a basis that is not unique."""
    problems = []
    for key, want in case["expect"].items():
        if key == "basis_vectors":
            got = len(value["basis"][0]) if value["basis"] else 0
        else:
            got = value.get(key)
        if got != want:
            problems.append(f"{key}: expected {want}, got {got}")
    return problems


def check_topology(case, value):
    """The face counts, the homology groups and the Euler characteristic
    (Issue #103)."""
    return [
        f"{key}: expected {want}, got {value.get(key)}"
        for key, want in case["expect"].items()
        if value.get(key) != want
    ]


def check_checked(record):
    """Every claim of a matrix result is checked (Issue #58), and of a
    simplicial.homology result with certificates (Issue #103)."""
    guarantee = record.get("guarantee")
    if guarantee is None:
        return ["no guarantee recorded"]
    return [
        f"claim {c['id']} is {c['level']}"
        for c in guarantee["claims"]
        if c["level"] != "checked"
    ]


def polynomial_terms(p):
    """The terms of a PolynomialData as (exponents, coefficient) pairs, in
    descending order of exponent vectors, each coefficient reduced: two
    polynomials are equal if and only if these are."""
    return sorted(
        ((tuple(t["exponents"]), str(Fraction(t["coefficient"]))) for t in p["terms"]),
        reverse=True,
    )


def check_ideal(case, value):
    """The reduced Groebner basis, element by element and in the order of the
    value (Issue #66)."""
    problems = []
    if value.get("type") != "groebner_basis":
        problems.append(f"type {value.get('type')}")
    return problems + check_basis(case["expect"]["basis"], value["basis"])


def check_basis(want, got):
    """The elements of a reduced Groebner basis, one by one and in order."""
    problems = []
    if len(got) != len(want):
        problems.append(f"{len(got)} elements, expected {len(want)}")
    for j, (g, w) in enumerate(zip(got, want)):
        if g.get("type") != "polynomial" or polynomial_terms(g) != polynomial_terms(w):
            problems.append(f"g_{j}: expected {w['terms']}, got {g['terms']}")
    return problems


def check_polynomial(name, want, got):
    """One polynomial of the value against the expected one."""
    if got is None:
        return [f"{name}: expected {want['terms']}, got none"]
    if got.get("type") != "polynomial" or polynomial_terms(got) != polynomial_terms(want):
        return [f"{name}: expected {want['terms']}, got {got.get('terms')}"]
    return []


def check_ideal_normal_form(case, value):
    """The remainder and the basis (Issue #67). Both are unique in the order
    of the ring; the quotients and the matrices of the certificate are not."""
    exp = case["expect"]
    problems = []
    if value.get("type") != "normal_form":
        problems.append(f"type {value.get('type')}")
    problems += check_polynomial("remainder", exp["remainder"], value.get("remainder"))
    problems += check_basis(exp["basis"], value.get("basis", []))
    return problems


def check_ideal_membership(case, value):
    """`member` and, when the case fixes them and the value has a certificate,
    the basis and the remainder of the certificate of a polynomial outside the
    ideal (Issue #67). The coefficients of a member are not unique."""
    exp = case["expect"]
    problems = []
    if value.get("type") != "ideal_membership":
        problems.append(f"type {value.get('type')}")
    if value.get("member") is not exp["member"]:
        problems.append(f"member {value.get('member')}, expected {exp['member']}")
    certified = case["arguments"].get("certificate", "representation") != "none"
    certificate = value.get("certificate")
    if not certified:
        if certificate is not None:
            problems.append("a certificate the job did not ask for")
    elif certificate is None:
        problems.append("no certificate")
    elif "omitted" not in certificate:
        if "basis" in exp:
            problems += check_basis(exp["basis"], certificate.get("basis", []))
        if "remainder" in exp:
            problems += check_polynomial(
                "remainder", exp["remainder"], certificate.get("remainder")
            )
    return problems


def check_module(case, value):
    """A value of module.syzygies, complex.free_resolution or
    ideal.hilbert_series (Issues #97, #98): each key of `expect` but
    `claims` and `not_claimed` equals that of the value; `syzygies` is the
    number of columns of S, and a key `null` must be absent."""
    problems = []
    for key, want in case["expect"].items():
        if key in ("claims", "not_claimed"):
            continue
        if key == "syzygies":
            got = len(value.get("syzygies", {}).get("columns", []))
        else:
            got = value.get(key)
        if got != want:
            problems.append(f"{key}: expected {want}, got {got}")
    return problems


def check_module_claims(case, record):
    """The level and the effective level of each claim of a module result,
    `[level, effective level]` by claim in `expect.claims` and no others, and
    its not_claimed (Issues #97, #98)."""
    guarantee = record.get("guarantee")
    if not guarantee:
        return ["no guarantee"]
    claims = guarantee["claims"]
    got = {c["id"]: [c["level"], effective_level(claims, c["id"])] for c in claims}
    problems = []
    if got != case["expect"]["claims"]:
        problems.append(f"claims {got}, expected {case['expect']['claims']}")
    if guarantee.get("not_claimed", []) != case["expect"]["not_claimed"]:
        problems.append(
            f"not_claimed {guarantee.get('not_claimed')}, expected {case['expect']['not_claimed']}"
        )
    return problems


def check_s_pairs_key(case, value):
    """The certificate carries `s_pairs` if and only if the job asked for it
    (Issue #68); a membership certificate of a member never does. A
    certificate left out for its size is not looked at."""
    certificate = value.get("certificate")
    if not isinstance(certificate, dict) or "omitted" in certificate:
        return []
    want = case["arguments"].get("certificate") == "s_pairs" and value.get("member") is not True
    if ("s_pairs" in certificate) == want:
        return []
    return ["the certificate has no s_pairs" if want else "s_pairs the job did not ask for"]


def effective_level(claims, claim_id, path=()):
    """The weakest of the level of the claim and the effective levels of the
    claims it depends on (Guarantee::effective_level). A claim that is
    missing, or depends on itself, is asserted."""
    claim = next((c for c in claims if c["id"] == claim_id), None)
    if claim is None or claim_id in path:
        return "asserted"
    levels = [claim["level"]] + [
        effective_level(claims, d, path + (claim_id,)) for d in claim.get("depends_on", [])
    ]
    return "checked" if all(level == "checked" for level in levels) else "asserted"


def ideal_claim_levels(case):
    """The level and the effective level of each claim of an ideal.* result.

    ideal.groebner_basis (Issue #66): gb.reduced_form and, with a certificate,
    gb.contained and gb.contains are checked; gb.groebner is checked with the
    certificate `s_pairs` (Issue #68) and asserted with `representation` or
    `none`.
    ideal.normal_form (Issue #67): the same, plus normal_form.reduced (checked
    with a certificate) and normal_form.unique, checked by a theorem from its
    four premises: in effect checked with `s_pairs`, asserted without.
    ideal.membership (Issue #67): membership.in (checked with a certificate,
    which is the same whatever certificate was asked); or, for a polynomial
    outside the ideal with a certificate, gb.reduced_form, gb.contains,
    gb.groebner, normal_form.reduced and membership.not_in, the last checked
    by a theorem from the three before it (it does not depend on
    gb.contained): in effect checked with `s_pairs`, asserted without;
    without a certificate, only membership.not_in, asserted."""
    asked = case["arguments"].get("certificate", "representation")
    level = "checked" if asked != "none" else "asserted"
    groebner = "checked" if asked == "s_pairs" else "asserted"
    kind = case["kind"]
    if kind == "ideal":
        want = {
            "gb.reduced_form": "checked",
            "gb.contained": level,
            "gb.contains": level,
            "gb.groebner": groebner,
        }
        return want, dict(want)
    if kind == "ideal_normal_form":
        want = {
            "gb.reduced_form": "checked",
            "gb.contained": level,
            "gb.contains": level,
            "gb.groebner": groebner,
            "normal_form.reduced": level,
            "normal_form.unique": "checked",
        }
        return want, dict(want, **{"normal_form.unique": groebner})
    if case["expect"]["member"]:
        want = {"membership.in": level}
        return want, dict(want)
    if asked == "none":
        want = {"membership.not_in": "asserted"}
        return want, dict(want)
    want = {
        "gb.reduced_form": "checked",
        "gb.contains": "checked",
        "gb.groebner": groebner,
        "normal_form.reduced": "checked",
        "membership.not_in": "checked",
    }
    return want, dict(want, **{"membership.not_in": groebner})


def check_ideal_claims(case, record):
    """The levels and the effective levels of the claims of an ideal.* result
    (ideal_claim_levels)."""
    guarantee = record.get("guarantee")
    if guarantee is None:
        return ["no guarantee recorded"]
    want, want_effective = ideal_claim_levels(case)
    claims = guarantee["claims"]
    got = {c["id"]: c["level"] for c in claims}
    problems = [
        f"claim {claim} is {got.get(claim)}, expected {want.get(claim)}"
        for claim in sorted(set(want) | set(got))
        if got.get(claim) != want.get(claim)
    ]
    for claim in sorted(set(want_effective) & set(got)):
        effective = effective_level(claims, claim)
        if effective != want_effective[claim]:
            problems.append(
                f"claim {claim} is effectively {effective}, expected {want_effective[claim]}"
            )
    return problems


OPERATIONS = {
    "factor": ("polynomial.factor", check_factor),
    "gkz": ("gkz.generate", check_gkz),
    "gkz_indicial": ("gkz.indicial", check_local),
    "gkz_series": ("gkz.series", check_local),
    "matrix": (None, check_matrix),
    "ideal": ("ideal.groebner_basis", check_ideal),
    "ideal_normal_form": ("ideal.normal_form", check_ideal_normal_form),
    "ideal_membership": ("ideal.membership", check_ideal_membership),
    "module": (None, check_module),
    "topology": ("simplicial.homology", check_topology),
}

IDEAL_KINDS = ("ideal", "ideal_normal_form", "ideal_membership")


def main():
    client = Client(
        os.environ["ALGEBRAMESH_SERVER"],
        ca=os.environ["ALGEBRAMESH_CA"],
        token_file=os.environ["ALGEBRAMESH_TOKEN_FILE"],
    )
    save_dir = os.environ.get("ALGEBRAMESH_VALUES_SAVE")
    compare_dir = os.environ.get("ALGEBRAMESH_VALUES_COMPARE")
    only = os.environ.get("ALGEBRAMESH_BACKENDS")
    only = set(only.split(",")) if only else None
    jobs = []
    for path in sorted(CASES.glob("*.json")):
        case = json.loads(path.read_text())
        operation = OPERATIONS[case["kind"]][0] or case["operation"]
        for backend in case["backends"]:
            if only is not None and backend not in only:
                continue
            job = client.submit(
                backend=backend,
                operation=operation,
                arguments=case["arguments"],
                limits={"wall_seconds": 600},
                retry_policy="if_deterministic",
            )
            jobs.append((case, backend, job))

    failures = 0
    rows = []
    for case, backend, job in jobs:
        notes = []
        try:
            record = job.result(timeout=900)
        except JobFailed as e:
            problems = [f"job {e.record['state']}: {e.record.get('error')}"]
            version = ""
        else:
            check = OPERATIONS[case["kind"]][1]
            problems = check(case, record["result"])
            if case["kind"] in ("matrix", "topology"):
                problems += check_checked(record)
            elif case["kind"] in IDEAL_KINDS:
                problems += check_ideal_claims(case, record)
                problems += check_s_pairs_key(case, record["result"])
            elif case["kind"] == "module":
                problems += check_module_claims(case, record)
            version = record["backend"].get("backend_version") or ""
            name = f"{case['name']}__{backend}"
            if save_dir:
                values.save(save_dir, name, record["result"])
            if compare_dir:
                # A case added since the earlier run has nothing to compare.
                if values.load(compare_dir, name) is None:
                    notes.append(f"new: no value of {name} in the earlier run")
                else:
                    problems += values.compare(compare_dir, name, record["result"])
        status = "ok" if not problems else "FAIL"
        failures += bool(problems)
        rows.append((case["name"], backend, version, status))
        category = case.get("category", "")
        print(f"{status:4} {case['name']:36} {category:13} {backend:10} {version}")
        for p in problems + notes:
            print(f"     {p}")
    print(json.dumps({"cases": len(rows), "failures": failures, "results": rows}))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
