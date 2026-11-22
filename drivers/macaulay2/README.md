# Macaulay2 driver

`algebramesh.m2` is run by the macaulay2 backend as

```bash
M2 --script algebramesh.m2 request.json
```

and answers with one line `ALGEBRAMESH_RESULT {json}` or `ALGEBRAMESH_ERROR message`.
Requests carry exponent vectors and coefficient strings only; see the header of the
script and [ADR 0005](../../docs/adr/0005-cas-drivers.md). It needs Macaulay2 with
the `HolonomicSystems`, `JSON`, `Polyhedra` and `LLLBases` packages.

`gkz.m2` holds the operations of the GKZ package (`gkz.generate`, `gkz.indicial`,
`gkz.series`; [the package](../../docs/packages/gkz.md)). `algebramesh.m2` loads it
when it lies next to it; the backend offers these operations only when it is built
with the feature `pkg-gkz`, and then counts the file in the driver's digest.
`gkz.generate` needs 4ti2.

`topology.m2` holds the operation of the topology package (`simplicial.homology`;
[the package](../../docs/packages/topology.md)). It is loaded and counted in the same
way, with the feature `pkg-topology`. It takes the boundary matrices the backend
builds and computes their Smith normal forms with the procedure of
`matrix.smith_normal_form`.
