"""Python binding against a running cluster (see tests/integration/run-cluster-e2e.sh).

Uses ALGEBRAMESH_SERVER, ALGEBRAMESH_CA and ALGEBRAMESH_TOKEN_FILE. Runs with pytest
or as a plain script. With ALGEBRAMESH_VALUES_SAVE and ALGEBRAMESH_VALUES_COMPARE,
the result values are saved, or compared with those of an earlier version, as in
tests/regression/run.py (tests/regression/values.py).
"""

import os
import sys
from fractions import Fraction
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tests" / "regression"))
import values  # noqa: E402

import algebramesh
from algebramesh import AlgebraMeshError, Client, JobFailed


def client() -> Client:
    return Client(
        os.environ["ALGEBRAMESH_SERVER"],
        ca=os.environ["ALGEBRAMESH_CA"],
        token_file=os.environ["ALGEBRAMESH_TOKEN_FILE"],
    )


FACTOR_ARGS = {
    "ring": {"domain": "QQ", "variables": ["x", "y"], "order": "grevlex"},
    "expression": {
        "type": "polynomial",
        "terms": [
            {"coefficient": "1", "exponents": [4, 0]},
            {"coefficient": "-1", "exponents": [0, 4]},
        ],
    },
}


def check_value(name, value):
    """Saves `value`, or compares it with the saved one of an earlier version."""
    saved = os.environ.get("ALGEBRAMESH_VALUES_SAVE")
    earlier = os.environ.get("ALGEBRAMESH_VALUES_COMPARE")
    if saved:
        values.save(saved, f"python_{name}", value)
    if earlier:
        problems = values.compare(earlier, f"python_{name}", value)
        assert not problems, problems


def test_backends_and_workers_report_versions():
    c = client()
    names = {b["name"] for b in c.backends()}
    assert {"asir", "macaulay2", "oscar"} <= names, names
    versions = {
        o["name"]: o["backend_version"] for w in c.workers() if w["online"] for o in w["backends"]
    }
    assert versions["oscar"].startswith("OSCAR "), versions
    assert versions["macaulay2"].startswith("Macaulay2 "), versions


def test_factor_through_the_python_api():
    c = client()
    job = c.submit(backend="asir", operation="polynomial.factor", arguments=FACTOR_ARGS)
    value = job.value(timeout=300)
    check_value("factor_asir", value)
    assert value["type"] == "factorization"
    assert value["verified_product"] is True
    assert sorted(f["display"] for f in value["factors"]) == ["x+y", "x-y", "x^2+y^2"]
    assert job.status()["state"] == "SUCCEEDED"
    # Issue #47: the product is checked; irreducibility is the backend's word.
    guarantee = job.guarantee()
    levels = {c["id"]: c["level"] for c in guarantee["claims"]}
    assert levels["factor.product"] == "checked", levels
    assert levels["factor.domain"] == "checked", levels
    assert levels["factor.irreducible"] == "asserted", levels
    assert guarantee["summary"] == "partially_verified", guarantee
    record = job.result()
    assert record["backend"]["name"] == "asir"
    assert len(record["input_sha256"]) == 64


def test_download_writes_the_result_value(tmp_path=None):
    """Job.download writes the value as JSON (ADR 0021); a small value is
    written from the record, a large one from its blob."""
    import json
    import tempfile

    c = client()
    job = c.submit(backend="asir", operation="polynomial.factor", arguments=FACTOR_ARGS)
    with tempfile.TemporaryDirectory() as d:
        path = Path(tmp_path or d) / "value.json"
        record = job.download(path, timeout=300)
        assert record["state"] == "SUCCEEDED"
        assert json.loads(path.read_text()) == job.value()


def test_invalid_spec_is_rejected_by_the_scheduler():
    c = client()
    try:
        c.submit({"schema_version": 2, "backend": "asir", "operation": "x", "arguments": {}})
    except AlgebraMeshError as e:
        assert "schema_version" in str(e)
    else:
        raise AssertionError("expected AlgebraMeshError")


def test_factor_helper_gives_the_value_of_a_submission():
    # Issue #56: the same value as through submit, without a name of its own
    # among the saved values (T0 compares it through factor_asir).
    c = client()
    value = c.factor(FACTOR_ARGS["ring"], FACTOR_ARGS["expression"], timeout=300)
    job = c.submit(backend="asir", operation="polynomial.factor", arguments=FACTOR_ARGS)
    assert value == job.value(timeout=300)
    assert sorted(f["display"] for f in value["factors"]) == ["x+y", "x-y", "x^2+y^2"]


def test_matrix_normal_forms_agree_across_backends():
    # Issue #57: the Smith and Hermite normal forms are unique, so OSCAR and
    # Macaulay2 give the same ones; [[2, 4], [6, 8]] has Smith form diag(2, 4).
    c = client()
    a = [[2, 4], [6, 8]]
    for backend in ("oscar", "macaulay2"):
        snf = c.smith_normal_form(a, backend=backend, timeout=600)
        assert snf["diagonal"] == ["2", "4"], (backend, snf)
        hnf = c.hermite_normal_form(a, backend=backend, timeout=600)
        assert hnf["form"] == [["2", "0"], ["0", "4"]], (backend, hnf)
    none = c.solve(a, [1, 1], domain="ZZ", timeout=600)
    assert none["solution"] is None and "y" in none["certificate"], none


def test_failed_job_raises_job_failed():
    c = client()
    bad = dict(FACTOR_ARGS, ring={"domain": "ZZ", "variables": ["x", "y"], "order": "lex"})
    bad["expression"] = {
        "type": "polynomial",
        "terms": [{"coefficient": "1/2", "exponents": [1, 0]}],
    }
    job = c.submit(backend="asir", operation="polynomial.factor", arguments=bad)
    try:
        job.result(timeout=120)
    except JobFailed as e:
        assert e.record["state"] == "FAILED"
        assert e.record["error"]["code"] == "INVALID_ARGUMENTS"
    else:
        raise AssertionError("expected JobFailed")


def test_gkz_design_example():
    c = client()
    d = lambda *e: {"coefficient": "1", "x_exponents": [0, 0, 0], "d_exponents": list(e)}
    expected = [[d(1, 0, 1), dict(d(0, 2, 0), coefficient="-1")]]
    system = c.gkz(A=[[1, 1, 1], [0, 1, 2]], beta=[3, Fraction(4)], expected=expected, timeout=600)
    check_value("gkz_design", system)
    assert system["type"] == "gkz_system"
    checks = system["checks"]
    assert checks["euler_operators_match"] and checks["toric_binomials_valid"]
    assert checks["generated_by_toric_and_euler"]
    assert [m["in_ideal"] for m in checks["expected_in_ideal"]] == [True]
    displays = {g["display"] for g in system["euler_operators"]}
    assert displays == {"x1*d1+x2*d2+x3*d3-3", "x2*d2+2*x3*d3-4"}, displays


def test_gkz_indicial_and_series():
    c = client()
    A = [[1, 1, 1], [0, 1, 2]]
    indicial = c.gkz_indicial(A=A, beta=["1/2", Fraction(1, 3)], weight=[1, 0, 1], timeout=600)
    check_value("gkz_indicial", indicial)
    assert indicial["type"] == "gkz_indicial"
    assert indicial["holonomic_rank"] == 2
    points = sorted(tuple(e["point"]) for e in indicial["exponents"])
    assert points == [("0", "2/3", "-1/6"), ("1/6", "1/3", "0")], points
    assert indicial["parameters"]["resonant"] is False

    series = c.gkz_series(A=A, beta=["1/2", "1/3"], weight=[1, 0, 1], order=6, timeout=600)
    check_value("gkz_series", series)
    assert [s["status"] for s in series["series"]] == ["verified_to_order"] * 2
    first = next(s for s in series["series"] if s["point"] == ["1/6", "1/3", "0"])
    assert [t["coefficient"] for t in first["terms"]][:2] == ["1", "-4/21"]

    with_logs = c.gkz_series(A=A, beta=[0, 0], weight=[1, 0, 1], order=4, timeout=600)
    check_value("gkz_series_logarithmic", with_logs)
    assert [s["status"] for s in with_logs["series"]] == ["logarithmic"]


if __name__ == "__main__":
    print("algebramesh", algebramesh.__version__)
    for name, test in sorted(globals().items()):
        if name.startswith("test_") and callable(test):
            test()
            print("ok", name)
    print("all python binding tests passed")
