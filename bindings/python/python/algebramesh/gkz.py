"""The GKZ package (Issues #99, #100, ``docs/packages/gkz.md``).

Example::

    from algebramesh import Client, gkz

    client = Client("https://scheduler.internal:50051",
                    ca="ca.pem", token_file="client.token")
    system = gkz.generate(client, A=[[1, 1, 1], [0, 1, 2]], beta=[3, 4])
    series = gkz.series(client, A=[[1, 1, 1], [0, 1, 2]], beta=["1/2", "1/3"],
                        weight=[1, 0, 1], order=6)

``Client.gkz``, ``Client.gkz_indicial`` and ``Client.gkz_series`` call these.
The operations run on a Macaulay2 backend built with the package (feature
``pkg-gkz``); each result carries the claims the agent checked in its
``guarantee`` (``docs/claims.md``).
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Any, Iterable, Mapping, Optional, Sequence

from . import Number, _integer, _limits, _rational

if TYPE_CHECKING:
    from . import Client

__all__ = ["generate", "indicial", "series"]


def _system(A: Sequence[Sequence[int]], beta: Iterable[Number]) -> dict:
    return {
        "A": [[_integer(a, "an entry of A") for a in row] for row in A],
        "beta": [_rational(b) for b in beta],
    }


def _run(client: "Client", operation: str, arguments: dict, backend: str,
         timeout: Optional[float], wall_seconds: Optional[int]) -> dict:
    job = client.submit(
        backend=backend,
        operation=operation,
        arguments=arguments,
        limits=_limits(wall_seconds),
    )
    return job.value(timeout)


def generate(
    client: "Client",
    A: Sequence[Sequence[int]],
    beta: Iterable[Number],
    *,
    expected: Optional[Sequence[Sequence[Mapping[str, Any]]]] = None,
    backend: str = "macaulay2",
    timeout: Optional[float] = None,
    wall_seconds: Optional[int] = None,
) -> dict:
    """Generators of the GKZ system H_A(beta) (``gkz.generate``).

    ``expected`` lists operators (as lists of terms with ``coefficient``,
    ``x_exponents`` and ``d_exponents``) whose membership in the left ideal is
    decided by the CAS.
    """
    arguments = _system(A, beta)
    if expected:
        arguments["expected"] = [list(op) for op in expected]
    return _run(client, "gkz.generate", arguments, backend, timeout, wall_seconds)


def indicial(
    client: "Client",
    A: Sequence[Sequence[int]],
    beta: Iterable[Number],
    weight: Sequence[int],
    *,
    backend: str = "macaulay2",
    timeout: Optional[float] = None,
    wall_seconds: Optional[int] = None,
) -> dict:
    """The indicial ideal of H_A(beta) for a generic weight and its roots
    (``gkz.indicial``).

    The result also reports the facets of cone(A), whether ``beta`` is resonant
    and whether ``A`` is homogeneous.
    """
    return _local(client, "gkz.indicial", A, beta, weight, None, backend, timeout, wall_seconds)


def series(
    client: "Client",
    A: Sequence[Sequence[int]],
    beta: Iterable[Number],
    weight: Sequence[int],
    order: int,
    *,
    backend: str = "macaulay2",
    timeout: Optional[float] = None,
    wall_seconds: Optional[int] = None,
) -> dict:
    """Gamma-series of H_A(beta) truncated to ``weight . u <= order``
    (``gkz.series``).

    Each exponent of multiplicity one gets a status: ``verified_to_order`` (the
    truncated series satisfies the system up to the order), ``not_a_solution``
    (with the series the CAS gave as ``evidence_terms``), or ``unsupported``.
    Exponents of higher multiplicity are ``logarithmic``: ``solutions`` lists
    the solutions found, each with its own status, and ``missing`` counts those
    not found. Terms with logarithms carry ``log_powers`` k for the divided
    power prod_i log(x_i)^k_i / k_i!. Verified series also report ``canonical``
    and, if normalization changed them, ``canonical_terms`` (docs/adr/0011).
    """
    return _local(client, "gkz.series", A, beta, weight, order, backend, timeout, wall_seconds)


def _local(client, operation, A, beta, weight, order, backend, timeout, wall_seconds) -> dict:
    arguments = _system(A, beta)
    arguments["weight"] = [_integer(w, "an entry of weight") for w in weight]
    if order is not None:
        arguments["order"] = _integer(order, "order")
    return _run(client, operation, arguments, backend, timeout, wall_seconds)
