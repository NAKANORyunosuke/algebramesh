"""The topology package (Issue #103, ``docs/packages/topology.md``).

Example::

    from algebramesh import Client, topology

    client = Client("https://scheduler.internal:50051",
                    ca="ca.pem", token_file="client.token")
    # The boundary of the tetrahedron: H_0 = ZZ, H_1 = 0, H_2 = ZZ.
    value = topology.homology(client, [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]])
    [h["rank"] for h in value["homology"]]   # [1, 0, 1]

The operation runs on a Macaulay2 backend built with the package (feature
``pkg-topology``); each result carries the claims the agent checked in its
``guarantee`` (``docs/claims.md``).
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Optional, Sequence

from . import _integer, _limits

if TYPE_CHECKING:
    from . import Client

__all__ = ["homology"]


def _vertex(v) -> int:
    n = _integer(v, "a vertex")
    if n < 0 or n >= 2**32:
        raise ValueError(f"a vertex is an integer from 0 to 2^32 - 1, not {n}")
    return n


def homology(
    client: "Client",
    facets: Sequence[Sequence[int]],
    *,
    certificate: bool = True,
    backend: str = "macaulay2",
    timeout: Optional[float] = None,
    wall_seconds: Optional[int] = None,
) -> dict:
    """The homology over ZZ of the simplicial complex with these facets
    (``simplicial.homology``).

    Each facet is a set of vertices, numbered by non-negative integers; no
    facet may contain another. The value gives ``homology``, one entry
    ``{"rank": b_k, "torsion": [t, ...]}`` for each dimension k (H_k is
    ZZ^b_k plus ZZ/t for each t), with the face counts, the Smith normal forms
    of the boundary matrices and the Euler characteristic. Unless
    ``certificate=False``, the forms come with their certificates, which the
    agent checks. A complex of more than 512 faces is refused by the backend.
    """
    if not isinstance(certificate, bool):
        raise TypeError("certificate is True or False")
    arguments = {
        "complex": {
            "type": "simplicial_complex",
            "facets": [[_vertex(v) for v in facet] for facet in facets],
        },
        "domain": "ZZ",
        "certificate": "include" if certificate else "none",
    }
    job = client.submit(
        backend=backend,
        operation="simplicial.homology",
        arguments=arguments,
        limits=_limits(wall_seconds),
    )
    return job.value(timeout)
