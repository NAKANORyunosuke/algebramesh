"""Result values saved as canonical JSON, and compared across versions.

A run saves the `value` of each job (the record's `result`) under a name such
as `<case>__<backend>`, one file per value, as canonical JSON (sorted keys, no
spaces). A later run compares its values with the saved ones and reports each
place where they differ, as a JSON pointer. Job ids, times, hosts and logs are
in the record, not in the value, so they are not compared.

Some lists hold the same elements in an order that the CAS does not fix. Their
paths are listed in UNORDERED; such a list is compared as a multiset.
"""

import json
from pathlib import Path

# Paths (JSON pointers, with "*" for any list index or object key) of lists
# whose order a CAS does not fix. Found by running the regression suite three
# times on the same images (docs/adr/0006-verification-and-regression.md).
UNORDERED = []


def canonical(value):
    """The canonical JSON text of `value`: sorted keys, no spaces."""
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def _file(directory, name):
    safe = "".join(c if c.isalnum() or c in "._-" else "_" for c in name)
    return Path(directory) / f"{safe}.json"


def save(directory, name, value):
    """Writes `value` as canonical JSON."""
    path = _file(directory, name)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(canonical(value) + "\n", encoding="utf-8")


def load(directory, name):
    """The saved value, or None if there is none."""
    path = _file(directory, name)
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding="utf-8"))


def _matches(pattern, path):
    parts = pattern.strip("/").split("/") if pattern.strip("/") else []
    if len(parts) != len(path):
        return False
    return all(p == "*" or p == str(q) for p, q in zip(parts, path))


def _unordered(path, unordered):
    return any(_matches(p, path) for p in unordered)


def differences(old, new, unordered=None, path=()):
    """The JSON pointers at which `new` differs from `old`."""
    unordered = UNORDERED if unordered is None else unordered
    pointer = "/" + "/".join(str(p) for p in path)
    if type(old) is not type(new):
        return [pointer]
    if isinstance(old, dict):
        out = []
        for key in sorted(set(old) | set(new)):
            if key not in old or key not in new:
                out.append(f"{pointer.rstrip('/')}/{key}")
            else:
                out += differences(old[key], new[key], unordered, path + (key,))
        return out
    if isinstance(old, list):
        if _unordered(path, unordered):
            if sorted(map(canonical, old)) != sorted(map(canonical, new)):
                return [pointer]
            return []
        if len(old) != len(new):
            return [pointer]
        out = []
        for i, (a, b) in enumerate(zip(old, new)):
            out += differences(a, b, unordered, path + (i,))
        return out
    return [] if old == new else [pointer]


def compare(directory, name, value):
    """Problems found comparing `value` with the saved one."""
    saved = load(directory, name)
    if saved is None:
        return [f"no saved value for {name}"]
    return [f"value differs at {p}" for p in differences(saved, value)]
