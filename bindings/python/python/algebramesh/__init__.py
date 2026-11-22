"""Python client for the AlgebraMesh scheduler.

Example::

    from algebramesh import Client

    client = Client("https://scheduler.internal:50051",
                    ca="ca.pem", token_file="client.token")
    factorization = client.factor(
        ring={"domain": "QQ", "variables": ["x", "y"], "order": "grevlex"},
        polynomial={"terms": [
            {"coefficient": 1, "exponents": [4, 0]},
            {"coefficient": -1, "exponents": [0, 4]}]},
        timeout=120,
    )

    system = client.gkz(A=[[1, 1, 1], [0, 1, 2]], beta=[3, 4])
    series = client.gkz_series(A=[[1, 1, 1], [0, 1, 2]], beta=["1/2", "1/3"],
                               weight=[1, 0, 1], order=6)

Arguments are typed JSON values (rationals as strings such as "1/2"); no CAS source
code is accepted.
"""

from __future__ import annotations

import json
import operator
from fractions import Fraction
from pathlib import Path
from typing import Any, Iterable, Mapping, Optional, Sequence, Union

from ._native import AlgebraMeshError, NativeClient, WaitTimeout, __version__

__all__ = ["Client", "Job", "AlgebraMeshError", "JobFailed", "WaitTimeout", "__version__"]

Number = Union[int, Fraction, str]


class JobFailed(AlgebraMeshError):
    """A job finished in a state other than SUCCEEDED; ``record`` holds its record."""

    def __init__(self, record: dict):
        self.record = record
        error = record.get("error") or {}
        super().__init__(
            f"job {record.get('job_id')} ended {record.get('state')}: "
            f"{error.get('code', '')} {error.get('message', '')}".strip()
        )


def _integer(value: Any, what: str) -> int:
    """An exact integer: ``int`` or anything with ``__index__`` (NumPy integers).

    A float, a ``Fraction`` or a string is refused rather than rounded: rounding
    would quietly change the problem sent to the CAS. ``bool`` is refused too.
    """
    if isinstance(value, bool):
        raise TypeError(f"{what} must be an integer, not a boolean")
    try:
        # int() drops a subclass such as IntEnum, whose str() is not its
        # digits before Python 3.11.
        return int(operator.index(value))
    except TypeError:
        raise TypeError(f"{what} must be an integer, not {value!r}") from None


def _limits(wall_seconds: Any) -> Optional[dict]:
    """The limits of a job with an optional ``wall_seconds``; 0 and negative
    values are errors, not "no limit"."""
    if wall_seconds is None:
        return None
    seconds = _integer(wall_seconds, "wall_seconds")
    if seconds <= 0:
        raise ValueError(f"wall_seconds must be positive, not {seconds}")
    return {"wall_seconds": seconds}


def _polynomial(polynomial: Mapping[str, Any]) -> dict:
    """The JSON of a polynomial whose coefficients are exact rationals and whose
    exponents are non-negative integers. Anything else is refused, not
    rounded (Issue #34)."""
    if not isinstance(polynomial, Mapping):
        raise TypeError(f"a polynomial is a mapping with 'terms', not {type(polynomial).__name__}")
    unknown = set(polynomial) - {"type", "terms"}
    if unknown:
        raise ValueError(f"unknown keys of a polynomial: {sorted(unknown)}")
    if polynomial.get("type", "polynomial") != "polynomial":
        raise ValueError(f"the type of a polynomial is 'polynomial', not {polynomial['type']!r}")
    if "terms" not in polynomial:
        raise ValueError("a polynomial needs 'terms'")
    terms = []
    for term in polynomial["terms"]:
        if not isinstance(term, Mapping) or set(term) != {"coefficient", "exponents"}:
            raise ValueError(f"a term has exactly 'coefficient' and 'exponents', not {term!r}")
        exponents = [_integer(e, "an exponent") for e in term["exponents"]]
        negative = [e for e in exponents if e < 0]
        if negative:
            raise ValueError(f"exponents are non-negative, not {negative[0]}")
        terms.append({"coefficient": _rational(term["coefficient"]), "exponents": exponents})
    return {"type": "polynomial", "terms": terms}


def _matrix(matrix: Sequence[Sequence[Number]], domain: str) -> list:
    """The rows of a matrix over ``domain`` ("ZZ" or "QQ"), every entry exact:
    over ZZ an integer, over QQ an int, a ``Fraction`` or a string such as
    "1/2". Rows of different lengths are refused."""
    if domain not in ("ZZ", "QQ"):
        raise ValueError(f"the domain is ZZ or QQ, not {domain!r}")
    if domain == "ZZ":
        # As decimal strings: a JSON number beyond 64 bits would be read as
        # floating point (Issue #60).
        rows = [[str(_integer(v, "an entry of the matrix")) for v in row] for row in matrix]
    else:
        rows = [[_rational(v) for v in row] for row in matrix]
    if len({len(r) for r in rows}) > 1:
        raise ValueError("the rows of the matrix have different lengths")
    return rows


def _rational(value: Number) -> str:
    if isinstance(value, bool):
        raise TypeError("booleans are not numbers here")
    if isinstance(value, int):
        return str(int(value))
    if isinstance(value, Fraction):
        return str(value)
    if isinstance(value, str):
        return value
    raise TypeError(f"use int, fractions.Fraction or a string like '1/2', not {type(value).__name__}")


class Job:
    """A submitted job."""

    def __init__(self, client: "Client", job_id: str):
        self._client = client
        self.id = job_id

    def __repr__(self) -> str:
        return f"Job({self.id!r})"

    def status(self) -> dict:
        return json.loads(self._client._native.status(self.id))

    def cancel(self) -> dict:
        return json.loads(self._client._native.cancel(self.id))

    def record(self, timeout: Optional[float] = None) -> dict:
        """Waits for the job and returns its full record, whatever the outcome."""
        return json.loads(self._client._native.wait(self.id, timeout, True))

    def result(self, timeout: Optional[float] = None) -> dict:
        """Waits for the job and returns its record; raises JobFailed unless it SUCCEEDED."""
        record = self.record(timeout)
        if record.get("state") != "SUCCEEDED":
            raise JobFailed(record)
        return record

    def value(self, timeout: Optional[float] = None) -> Any:
        """The typed result value of a successful job."""
        return self.result(timeout)["result"]

    def download(self, path: Union[str, Path], timeout: Optional[float] = None) -> dict:
        """Waits for the job and writes its result value as JSON to ``path``,
        for values too large to hold comfortably (ADR 0021). A value the
        scheduler keeps as one blob goes to the file without passing through
        memory. Returns the record, in which values kept as blobs stay markers
        ``{"$blob": "sha256:...", "size": n}``; raises JobFailed unless the job
        SUCCEEDED."""
        record = json.loads(self._client._native.download(self.id, str(path), timeout, True))
        if record.get("state") != "SUCCEEDED":
            raise JobFailed(record)
        return record

    def guarantee(self, timeout: Optional[float] = None) -> Optional[dict]:
        """What the result of a successful job claims (ADR 0018): ``summary``
        (verified, partially_verified or unverified) and ``claims``, each with
        its ``id``, ``level`` (checked, cross_checked or asserted) and ``by``.
        None if the cluster records no guarantee (before every scheduler can
        keep one)."""
        return self.result(timeout).get("guarantee")


class Client:
    """Connection to a scheduler over TLS with a client token."""

    def __init__(
        self,
        url: str,
        *,
        ca: Union[str, Path],
        token: Optional[str] = None,
        token_file: Union[str, Path, None] = None,
        server_name: Optional[str] = None,
    ):
        if (token is None) == (token_file is None):
            raise ValueError("give exactly one of token and token_file")
        if token is None:
            lines = Path(token_file).read_text().splitlines()
            token = next((l.strip() for l in lines if l.strip() and not l.startswith("#")), "")
        self._native = NativeClient(url, str(ca), token, server_name)

    def submit(
        self,
        spec: Optional[Mapping[str, Any]] = None,
        *,
        backend: Optional[str] = None,
        operation: Optional[str] = None,
        arguments: Optional[Mapping[str, Any]] = None,
        limits: Optional[Mapping[str, Any]] = None,
        retry_policy: str = "never",
        environment: Optional[Mapping[str, Any]] = None,
        priority: int = 0,
        request_id: Optional[str] = None,
    ) -> Job:
        """Submits a job given as a full specification or by its parts.

        ``request_id`` is an idempotency key: submitting again with the same key
        (after a lost answer, from this or another process) returns the job the
        first submission created. Without it, retries inside one call are safe,
        but calling ``submit`` again creates another job.
        """
        if spec is None:
            if backend is None or operation is None:
                raise ValueError("give a spec, or backend and operation")
            spec = {
                "schema_version": 1,
                "backend": backend,
                "operation": operation,
                "arguments": dict(arguments or {}),
                "retry_policy": retry_policy,
            }
            if limits:
                spec["limits"] = dict(limits)
            if environment:
                spec["environment"] = dict(environment)
        elif any(v is not None for v in (backend, operation, arguments, limits, environment)):
            raise ValueError("give either a spec or its parts, not both")
        return Job(self, self._native.submit(json.dumps(spec), priority, request_id))

    def job(self, job_id: str) -> Job:
        return Job(self, job_id)

    def backends(self) -> list:
        return json.loads(self._native.backends())

    def workers(self) -> list:
        return json.loads(self._native.workers())

    def factor(
        self,
        ring: Mapping[str, Any],
        polynomial: Mapping[str, Any],
        *,
        backend: str = "asir",
        timeout: Optional[float] = None,
        wall_seconds: Optional[int] = None,
    ) -> dict:
        """The irreducible factors of ``polynomial`` over the coefficient domain of
        ``ring``, with their multiplicities (``polynomial.factor``).

        ``ring`` has ``domain`` ("ZZ" or "QQ"), ``variables`` and ``order``;
        ``polynomial`` has ``terms``, each with a ``coefficient`` (an int, a
        ``Fraction`` or a string such as "1/2") and ``exponents``, one
        non-negative integer per variable. The value has ``constant`` and
        ``factors`` (``factor``, ``multiplicity``, ``display``); the backend has
        multiplied the factors out and found the input (``verified_product``).
        """
        if not isinstance(ring, Mapping):
            raise TypeError(f"a ring is a mapping, not {type(ring).__name__}")
        arguments = {"ring": dict(ring), "expression": _polynomial(polynomial)}
        job = self.submit(
            backend=backend,
            operation="polynomial.factor",
            arguments=arguments,
            limits=_limits(wall_seconds),
        )
        return job.value(timeout)

    def rank(self, matrix, *, domain: str = "QQ", **options) -> dict:
        """The rank of ``matrix`` (``matrix.rank``), with the certificate that
        bounds it from both sides. ``options``: ``backend`` (default "oscar"),
        ``certificate`` (True), ``timeout`` and ``wall_seconds``."""
        return self._matrix_operation("matrix.rank", matrix, domain, None, options)

    def kernel(self, matrix, *, domain: str = "QQ", **options) -> dict:
        """A basis of {x : A x = 0} (``matrix.kernel``), over ZZ of the lattice;
        the basis vectors are the columns of ``basis``."""
        return self._matrix_operation("matrix.kernel", matrix, domain, None, options)

    def solve(self, matrix, rhs, *, domain: str = "QQ", **options) -> dict:
        """A solution x of A x = rhs (``matrix.solve``), or ``solution`` None
        with a certificate y that there is none."""
        return self._matrix_operation("matrix.solve", matrix, domain, rhs, options)

    def hermite_normal_form(self, matrix, **options) -> dict:
        """The row Hermite normal form H = U A over ZZ."""
        return self._matrix_operation("matrix.hermite_normal_form", matrix, "ZZ", None, options)

    def smith_normal_form(self, matrix, **options) -> dict:
        """The diagonal of the Smith normal form P A Q over ZZ."""
        return self._matrix_operation("matrix.smith_normal_form", matrix, "ZZ", None, options)

    def _matrix_operation(self, operation, matrix, domain, rhs, options) -> dict:
        unknown = set(options) - {"backend", "certificate", "timeout", "wall_seconds"}
        if unknown:
            raise TypeError(f"unknown options {sorted(unknown)}")
        certificate = options.get("certificate", True)
        if not isinstance(certificate, bool):
            raise TypeError("certificate is True or False")
        arguments: dict = {
            "domain": domain,
            "matrix": _matrix(matrix, domain),
            "certificate": "include" if certificate else "none",
        }
        if rhs is not None:
            convert = (lambda v: str(_integer(v, "an entry of rhs"))) if domain == "ZZ" else _rational
            arguments["rhs"] = [convert(v) for v in rhs]
        job = self.submit(
            backend=options.get("backend", "oscar"),
            operation=operation,
            arguments=arguments,
            limits=_limits(options.get("wall_seconds")),
        )
        return job.value(options.get("timeout"))

    def gkz(
        self,
        A: Sequence[Sequence[int]],
        beta: Iterable[Number],
        *,
        expected: Optional[Sequence[Sequence[Mapping[str, Any]]]] = None,
        backend: str = "macaulay2",
        timeout: Optional[float] = None,
        wall_seconds: Optional[int] = None,
    ) -> dict:
        """Generators of the GKZ system H_A(beta): :func:`algebramesh.gkz.generate`."""
        from . import gkz

        return gkz.generate(self, A, beta, expected=expected, backend=backend,
                            timeout=timeout, wall_seconds=wall_seconds)

    def gkz_indicial(
        self,
        A: Sequence[Sequence[int]],
        beta: Iterable[Number],
        weight: Sequence[int],
        *,
        backend: str = "macaulay2",
        timeout: Optional[float] = None,
        wall_seconds: Optional[int] = None,
    ) -> dict:
        """The indicial ideal of H_A(beta) and its roots: :func:`algebramesh.gkz.indicial`."""
        from . import gkz

        return gkz.indicial(self, A, beta, weight, backend=backend,
                            timeout=timeout, wall_seconds=wall_seconds)

    def gkz_series(
        self,
        A: Sequence[Sequence[int]],
        beta: Iterable[Number],
        weight: Sequence[int],
        order: int,
        *,
        backend: str = "macaulay2",
        timeout: Optional[float] = None,
        wall_seconds: Optional[int] = None,
    ) -> dict:
        """Gamma-series of H_A(beta): :func:`algebramesh.gkz.series`."""
        from . import gkz

        return gkz.series(self, A, beta, weight, order, backend=backend,
                          timeout=timeout, wall_seconds=wall_seconds)
