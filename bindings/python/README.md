# Python binding

Python package `algebramesh`, built with PyO3 and maturin from the same Rust client
library as the CLI (design §10.2).

## Build

```bash
pip install maturin
maturin develop --release -m bindings/python/Cargo.toml
```

or build a wheel with `maturin build --release -m bindings/python/Cargo.toml`.

## Use

```python
from algebramesh import Client

client = Client("https://scheduler.internal:50051", ca="ca.pem", token_file="client.token")
print(client.factor(
    ring={"domain": "QQ", "variables": ["x", "y"], "order": "grevlex"},
    polynomial={"terms": [{"coefficient": 1, "exponents": [4, 0]},
                          {"coefficient": -1, "exponents": [0, 4]}]},
    timeout=120,
))
print(client.gkz(A=[[1, 1, 1], [0, 1, 2]], beta=[3, 4]))
print(client.smith_normal_form([[2, 4], [6, 8]]))          # diagonal ["2", "4"]
print(client.solve([[2, 4]], [1], domain="ZZ"))            # no solution, with y
print(client.gkz_indicial(A=[[1, 1, 1], [0, 1, 2]], beta=["1/2", "1/3"], weight=[1, 0, 1]))
print(client.gkz_series(A=[[1, 1, 1], [0, 1, 2]], beta=["1/2", "1/3"], weight=[1, 0, 1], order=6))
```

`factor` runs on `asir` unless `backend` names another backend, the `gkz*`
helpers on `macaulay2`, and the matrix helpers (`rank`, `kernel`, `solve`,
`hermite_normal_form`, `smith_normal_form`) on `oscar`; the matrix entries must
be exact (integers over ZZ, and also `Fraction`s or strings over QQ). A matrix
result carries a certificate unless `certificate=False`, and `Job.guarantee()`
says which claims were checked. For `factor`, exponents must be non-negative integers and
coefficients ints, `Fraction`s or strings such as `"1/2"`; floats and booleans are
refused before anything is sent. The entries of `A` and the weights of the `gkz*`
helpers are checked in the same way; the terms of `expected` are sent as given.

`algebramesh.topology.homology(client, facets)` computes the homology over ZZ of the
simplicial complex with these facets (`simplicial.homology`, on `macaulay2` built with
the topology package); the vertices must be non-negative integers, and the Smith
normal forms of the boundary matrices come with certificates unless
`certificate=False`.

`Job.result()` raises `JobFailed` unless the job SUCCEEDED; `Job.record()` returns
the record in any terminal state. Arguments are typed JSON values; CAS source code
is not accepted.

## Workflows

`algebramesh.workflow` runs a workflow of jobs whose arguments take values from
other jobs' results (ADR 0032), with the same orchestrator as
`algebramesh workflow run|resume|status|cancel`.

```python
from algebramesh import Client, workflow

client = Client("https://scheduler.internal:50051", ca="ca.pem", token_file="client.token")
report = workflow.run(client, "membership.json")      # state in membership.json.state.json
print(report["progress"], report["nodes"]["member"]["claims"])
workflow.resume(client, "membership.json", retry_failed=True)
workflow.cancel(client, "membership.json")
```

A definition is a JSON file or a mapping `{"workflow": name, "nodes": {node:
{"spec": spec, "priority": 0, "depends_on": {claim: [{"node": n, "claim": c}]}}}}`.
A top-level argument `workflow.output(node, pointer)`, that is
`{"$output": {"node": node, "pointer": "/key"}}`, takes the value at `pointer`
(`""` for the whole result) of the result of `node`. For a mapping, `state`
names the state file. Each function returns the report: `progress`
(`SUCCEEDED`, `RUNNING`, `STOPPED` or `CANCELLED`) and, for each node, its
`state`, `job_id`, `result_sha256`, `summary` and `claims` with their
`effective_level` across the edges of the workflow.

The test `tests/test_cluster.py` runs against a cluster; see
`tests/integration/run-cluster-e2e.sh`.
