"""Inputs of the Python client are sent exactly or refused (Issue #34).

Runs without a scheduler: the native module is replaced by a stand-in that
records what would be sent. Run with ``python bindings/python/tests/test_inputs.py``.
"""

import json
import sys
import types
from fractions import Fraction
from pathlib import Path

sent = []
workflows = []


class _Native:
    def __init__(self, *args):
        pass

    def submit(self, spec_json, priority, request_id):
        sent.append(json.loads(spec_json))
        return "job"

    def wait(self, job_id, timeout, follow):
        return json.dumps({"state": "SUCCEEDED", "result": {}})

    def workflow(self, command, definition_json, state, *options):
        workflows.append((command, json.loads(definition_json), state, options))
        return json.dumps({"workflow": "w", "progress": "SUCCEEDED", "nodes": {}})


native = types.ModuleType("algebramesh._native")
native.NativeClient = _Native
native.AlgebraMeshError = type("AlgebraMeshError", (Exception,), {})
native.WaitTimeout = type("WaitTimeout", (Exception,), {})
native.__version__ = "test"
sys.modules["algebramesh._native"] = native
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))

import algebramesh  # noqa: E402

client = algebramesh.Client("https://localhost:1", ca="ca.pem", token="t")


def last():
    return sent[-1]


def refused(call, error=TypeError):
    before = len(sent)
    try:
        call()
    except error:
        assert len(sent) == before, "nothing is sent"
        return
    raise AssertionError(f"{call} was not refused")


def test_non_integers_are_refused_not_rounded():
    for A in ([[1.9, 2]], [[Fraction(3, 2), 2]], [["1", 2]], [[True, 2]]):
        refused(lambda: client.gkz(A=A, beta=[1]))
    refused(lambda: client.gkz_series(A=[[1, 1]], beta=[1], weight=[1.9, -0.9], order=3))
    refused(lambda: client.gkz_series(A=[[1, 1]], beta=[1], weight=[1, 0], order=3.9))
    refused(lambda: client.gkz_indicial(A=[[1, 1]], beta=[1], weight=[1, Fraction(1, 2)]))


def test_integers_are_sent_exactly():
    class Index:
        """An integer type like numpy.int64: it has __index__."""

        def __init__(self, n):
            self.n = n

        def __index__(self):
            return self.n

    client.gkz_series(A=[[1, Index(2)]], beta=["1/2"], weight=[Index(1), -3], order=Index(6))
    spec = last()
    assert spec["arguments"]["A"] == [[1, 2]]
    assert spec["arguments"]["weight"] == [1, -3]
    assert spec["arguments"]["order"] == 6
    assert spec["arguments"]["beta"] == ["1/2"]
    big = 2**70
    client.gkz(A=[[big, 1]], beta=[Fraction(1, 3)])
    assert last()["arguments"]["A"] == [[big, 1]]
    assert last()["arguments"]["beta"] == ["1/3"]


def test_wall_seconds_is_kept_or_refused():
    client.gkz(A=[[1, 1]], beta=[1], wall_seconds=30)
    assert last()["limits"] == {"wall_seconds": 30}
    client.gkz(A=[[1, 1]], beta=[1])
    assert "limits" not in last()
    refused(lambda: client.gkz(A=[[1, 1]], beta=[1], wall_seconds=0), ValueError)
    refused(lambda: client.gkz(A=[[1, 1]], beta=[1], wall_seconds=-5), ValueError)
    refused(lambda: client.gkz(A=[[1, 1]], beta=[1], wall_seconds=1.5))


def test_homology_sends_the_facets_exactly():
    from algebramesh import topology

    class Index:
        def __index__(self):
            return 2

    topology.homology(client, [[0, 1, Index()], [1, 3]], wall_seconds=10)
    spec = last()
    assert spec["operation"] == "simplicial.homology"
    assert spec["backend"] == "macaulay2"
    assert spec["arguments"] == {
        "complex": {"type": "simplicial_complex", "facets": [[0, 1, 2], [1, 3]]},
        "domain": "ZZ",
        "certificate": "include",
    }
    assert spec["limits"] == {"wall_seconds": 10}
    topology.homology(client, [[0]], certificate=False)
    assert last()["arguments"]["certificate"] == "none"
    refused(lambda: topology.homology(client, [[0, 1.0]]))
    refused(lambda: topology.homology(client, [["0", 1]]))
    refused(lambda: topology.homology(client, [[0, True]]))
    refused(lambda: topology.homology(client, [[0, -1]]), ValueError)
    refused(lambda: topology.homology(client, [[0, 1]], certificate="none"))


RING = {"domain": "QQ", "variables": ["x", "y"], "order": "grevlex"}


def test_factor_sends_the_ring_and_the_polynomial():
    client.factor(
        RING,
        {"terms": [
            {"coefficient": 1, "exponents": [4, 0]},
            {"coefficient": Fraction(-1, 2), "exponents": [0, 4]},
        ]},
        backend="macaulay2",
        wall_seconds=60,
    )
    spec = last()
    assert spec["operation"] == "polynomial.factor"
    assert spec["backend"] == "macaulay2"
    assert spec["arguments"] == {
        "ring": RING,
        "expression": {"type": "polynomial", "terms": [
            {"coefficient": "1", "exponents": [4, 0]},
            {"coefficient": "-1/2", "exponents": [0, 4]},
        ]},
    }
    assert spec["limits"] == {"wall_seconds": 60}
    client.factor(RING, {"type": "polynomial", "terms": [{"coefficient": "2", "exponents": [1, 1]}]})
    assert last()["backend"] == "asir"


def test_factor_refuses_inexact_numbers():
    def factor(coefficient, exponents):
        return lambda: client.factor(
            RING, {"terms": [{"coefficient": coefficient, "exponents": exponents}]}
        )

    for exponent in (1.0, True, Fraction(1, 1), "1"):
        refused(factor(1, [exponent, 0]))
    for coefficient in (0.5, True):
        refused(factor(coefficient, [1, 0]))
    refused(factor(1, [-1, 0]), ValueError)
    refused(lambda: client.factor(RING, {"terms": [], "script": "quit;"}), ValueError)
    refused(lambda: client.factor(RING, {"terms": [{"coefficient": 1}]}), ValueError)
    refused(lambda: client.factor(RING, "x^4-y^4"))



def test_guarantee_is_read_from_the_record():
    job = client.submit(backend="asir", operation="polynomial.factor", arguments={})
    assert job.guarantee() is None
    record = {"state": "SUCCEEDED", "result": {}, "guarantee": {"summary": "unverified", "claims": []}}
    wait = client._native.wait
    client._native.wait = lambda *args: json.dumps(record)
    try:
        assert job.guarantee()["summary"] == "unverified"
    finally:
        client._native.wait = wait



def test_matrix_operations_send_exact_entries():
    client.smith_normal_form([[2, 4], [6, 8]], wall_seconds=30)
    spec = last()
    assert spec["operation"] == "matrix.smith_normal_form"
    assert spec["backend"] == "oscar"
    assert spec["arguments"] == {"domain": "ZZ", "matrix": [["2", "4"], ["6", "8"]], "certificate": "include"}
    client.smith_normal_form([[2**64, 1], [0, 1]])
    assert last()["arguments"]["matrix"] == [[str(2**64), "1"], ["0", "1"]]
    client.solve([[1, Fraction(1, 2)]], [Fraction(3, 4)], domain="QQ", certificate=False, backend="macaulay2")
    spec = last()
    assert spec["arguments"] == {
        "domain": "QQ", "matrix": [["1", "1/2"]], "rhs": ["3/4"], "certificate": "none"}
    assert spec["backend"] == "macaulay2"
    client.solve([[2, 4]], [2**70], domain="ZZ")
    assert last()["arguments"]["rhs"] == [str(2**70)]


class _Odd(int):
    """An int whose str() is not its digits, as IntEnum before Python 3.11."""

    def __str__(self):
        return "odd"


def test_int_subclasses_are_sent_as_their_digits():
    client.smith_normal_form([[_Odd(2), 0], [0, 1]])
    assert last()["arguments"]["matrix"] == [["2", "0"], ["0", "1"]]
    client.rank([[_Odd(2), 1]], domain="QQ")
    assert last()["arguments"]["matrix"] == [["2", "1"]]
    client.solve([[1]], [_Odd(2)], domain="ZZ")
    assert last()["arguments"]["rhs"] == ["2"]


def test_matrix_operations_refuse_inexact_entries():
    refused(lambda: client.rank([[0.5, 1]]))
    refused(lambda: client.hermite_normal_form([[Fraction(1, 2), 1]]))
    refused(lambda: client.smith_normal_form([[True, 1]]))
    refused(lambda: client.kernel([[1, 2], [3]]), ValueError)
    refused(lambda: client.rank([[1]], domain="RR"), ValueError)
    refused(lambda: client.solve([[1]], [1.5], domain="QQ"))
    refused(lambda: client.rank([[1]], certificate="yes"))
    refused(lambda: client.rank([[1]], cert=True))



def test_the_gkz_package_sends_what_the_client_methods_send():
    # Issue #100: Client.gkz* call algebramesh.gkz.
    from algebramesh import gkz

    A, beta = [[1, 1, 1], [0, 1, 2]], ["1/2", Fraction(1, 3)]
    pairs = [
        (lambda: client.gkz(A=A, beta=beta), lambda: gkz.generate(client, A=A, beta=beta)),
        (lambda: client.gkz_indicial(A=A, beta=beta, weight=[1, 0, 1]),
         lambda: gkz.indicial(client, A=A, beta=beta, weight=[1, 0, 1])),
        (lambda: client.gkz_series(A=A, beta=beta, weight=[1, 0, 1], order=6, wall_seconds=60),
         lambda: gkz.series(client, A=A, beta=beta, weight=[1, 0, 1], order=6, wall_seconds=60)),
    ]
    for method, function in pairs:
        method()
        by_method = last()
        function()
        assert last() == by_method, (by_method, last())
    assert last()["operation"] == "gkz.series"
    assert last()["arguments"] == {
        "A": A, "beta": ["1/2", "1/3"], "weight": [1, 0, 1], "order": 6,
    }
    refused(lambda: gkz.series(client, A=A, beta=beta, weight=[1, 0.5, 1], order=6))

def test_workflows_are_sent_as_defined():
    # W2: algebramesh.workflow passes the definition to the orchestrator.
    import tempfile

    from algebramesh import workflow

    assert workflow.output("a", "/basis") == {"$output": {"node": "a", "pointer": "/basis"}}
    assert workflow.output("a") == {"$output": {"node": "a", "pointer": ""}}
    refused(lambda: workflow.output("a", 1))
    definition = {"workflow": "w", "nodes": {
        "a": {"spec": {"schema_version": 1, "backend": "test", "operation": "test.op",
                       "arguments": {}}},
        "b": {"spec": {"schema_version": 1, "backend": "test", "operation": "test.op",
                       "arguments": {"x": workflow.output("a", "/x")}}},
    }}
    report = workflow.run(client, definition, state="w.state.json", retry_failed=True)
    assert report["progress"] == "SUCCEEDED"
    assert workflows[-1] == ("run", definition, "w.state.json", (True, None, 2.0))
    workflow.resume(client, definition, state="w.state.json", inline_max_bytes=10)
    assert workflows[-1] == ("run", definition, "w.state.json", (False, 10, 2.0))
    # A definition without a file needs its state file.
    before = len(workflows)
    for call in (lambda: workflow.run(client, definition),
                 lambda: workflow.status(client, definition)):
        try:
            call()
        except ValueError:
            pass
        else:
            raise AssertionError("no state file was asked for")
    try:
        workflow.run(client, definition, state="s", retry_failed=1)
    except TypeError:
        pass
    else:
        raise AssertionError("retry_failed=1 was taken")
    assert len(workflows) == before
    with tempfile.TemporaryDirectory() as d:
        path = Path(d) / "w.json"
        path.write_text(json.dumps(definition))
        workflow.cancel(client, path)
        assert workflows[-1] == ("cancel", definition, str(path) + ".state.json", ())
        workflow.status(client, str(path), state=Path(d) / "other")
        assert workflows[-1] == ("status", definition, str(Path(d) / "other"), ())


if __name__ == "__main__":
    failed = 0
    for name, test in sorted(globals().items()):
        if name.startswith("test_") and callable(test):
            try:
                test()
                print(f"ok {name}")
            except Exception as e:  # noqa: BLE001
                failed += 1
                print(f"FAILED {name}: {e!r}")
    sys.exit(1 if failed else 0)
