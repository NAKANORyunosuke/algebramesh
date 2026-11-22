"""Workflows of jobs whose arguments take values from other jobs' results
(W2, ADR 0032).

Example::

    from algebramesh import Client, workflow

    client = Client("https://scheduler.internal:50051",
                    ca="ca.pem", token_file="client.token")
    definition = {
        "workflow": "two-steps",
        "nodes": {
            "first": {"spec": first_spec},
            "second": {"spec": {
                "schema_version": 1, "backend": "oscar", "operation": "...",
                "arguments": {"basis": workflow.output("first", "/basis")},
            }},
        },
    }
    report = workflow.run(client, definition, state="two-steps.state.json")

The orchestrator runs in this process; the cluster knows the jobs of a
workflow by their request ids ``wf/<workflow>/<node>/<attempt>``. When this
process or a scheduler goes away, ``resume`` (the same as ``run``) finds the
jobs that were submitted and submits only the others. ``cancel`` records the
cancellation in the state file, so that no orchestrator submits another node,
and cancels the jobs that have not finished.

Each function returns the report: ``workflow``, ``progress`` (SUCCEEDED,
RUNNING, STOPPED or CANCELLED) and, for each node, ``state``, ``attempt``,
``request_id``, ``job_id``, ``error``, ``result_sha256``, ``summary`` and
``claims`` with their ``effective_level`` across the edges of the workflow.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import TYPE_CHECKING, Mapping, Optional, Union

if TYPE_CHECKING:
    from . import Client

__all__ = ["output", "run", "resume", "status", "cancel", "default_state_path"]

Definition = Union[Mapping, str, Path]


def output(node: str, pointer: str = "") -> dict:
    """``{"$output": {"node": node, "pointer": pointer}}``: as a top-level
    argument of a node, the value at ``pointer`` ("" for the whole result,
    "/key" for one top-level value) of the result of ``node``."""
    if not isinstance(node, str) or not isinstance(pointer, str):
        raise TypeError("node and pointer are strings")
    return {"$output": {"node": node, "pointer": pointer}}


def default_state_path(path: Union[str, Path]) -> Path:
    """The state file of the workflow defined in the file ``path``:
    ``<path>.state.json``."""
    return Path(str(path) + ".state.json")


def _definition(definition: Definition, state: Union[str, Path, None]) -> tuple:
    """The definition as JSON text and the state file."""
    if isinstance(definition, (str, Path)):
        text = Path(definition).read_text(encoding="utf-8")
        if state is None:
            state = default_state_path(definition)
    elif isinstance(definition, Mapping):
        text = json.dumps(definition)
        if state is None:
            raise ValueError("give state, the file that keeps the cancellation of the workflow")
    else:
        raise TypeError(f"a definition is a mapping or a file, not {type(definition).__name__}")
    return text, str(state)


def run(
    client: "Client",
    definition: Definition,
    *,
    state: Union[str, Path, None] = None,
    retry_failed: bool = False,
    inline_max_bytes: Optional[int] = None,
    poll: float = 2.0,
) -> dict:
    """Runs the workflow, or resumes it where an earlier run stopped, until
    every node SUCCEEDED, a node stopped or the workflow was cancelled.

    ``definition`` is a mapping or a JSON file; the state file defaults to
    ``<file>.state.json`` for a file and must be given for a mapping. With
    ``retry_failed``, a node whose job failed, timed out, was cancelled or
    lost is submitted again once as its next attempt. A value taken from a
    producer goes by reference up to ``inline_max_bytes`` and in a blob above
    it. Raises AlgebraMeshError while another orchestrator runs the workflow
    with the same state file, and ValueError for a definition that is not
    one."""
    if not isinstance(retry_failed, bool):
        raise TypeError("retry_failed is True or False")
    text, state = _definition(definition, state)
    return json.loads(client._native.workflow(
        "run", text, state, retry_failed, inline_max_bytes, float(poll)))


def resume(client: "Client", definition: Definition, **options) -> dict:
    """The same as :func:`run`: after this process or a scheduler went away,
    the nodes whose jobs the cluster has are not submitted again."""
    return run(client, definition, **options)


def status(client: "Client", definition: Definition, *,
           state: Union[str, Path, None] = None) -> dict:
    """How the workflow stands, without submitting anything."""
    text, state = _definition(definition, state)
    return json.loads(client._native.workflow("status", text, state))


def cancel(client: "Client", definition: Definition, *,
           state: Union[str, Path, None] = None) -> dict:
    """Cancels the workflow: no orchestrator submits another node, and its
    jobs that have not finished are cancelled."""
    text, state = _definition(definition, state)
    return json.loads(client._native.workflow("cancel", text, state))
