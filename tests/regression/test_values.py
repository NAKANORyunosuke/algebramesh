"""Tests of values.py: saving result values and comparing them across versions.

Runs with pytest or as a plain script; needs no cluster.
"""

import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import values  # noqa: E402

FACTORIZATION = {
    "type": "factorization",
    "ring": {"domain": "QQ", "variables": ["x", "y"], "order": "grevlex"},
    "constant": "1",
    "factors": [
        {"display": "x-y", "multiplicity": 1, "factor": {"type": "polynomial", "terms": []}},
        {"display": "x+y", "multiplicity": 1, "factor": {"type": "polynomial", "terms": []}},
    ],
    "verified_product": True,
}


def copy(value):
    return values.json.loads(values.canonical(value))


def test_canonical_json_sorts_keys_without_spaces():
    assert values.canonical({"b": 1, "a": [1, {"d": 2, "c": "é"}]}) == (
        '{"a":[1,{"c":"é","d":2}],"b":1}'
    )


def test_the_same_value_saved_twice_shows_no_difference():
    with tempfile.TemporaryDirectory() as d:
        values.save(d, "x4_minus_y4__asir", FACTORIZATION)
        assert values.compare(d, "x4_minus_y4__asir", copy(FACTORIZATION)) == []


def test_one_changed_place_is_reported_by_its_pointer():
    with tempfile.TemporaryDirectory() as d:
        values.save(d, "case__m2", FACTORIZATION)
        changed = copy(FACTORIZATION)
        changed["factors"][1]["multiplicity"] = 2
        assert values.compare(d, "case__m2", changed) == [
            "value differs at /factors/1/multiplicity"
        ]
        changed = copy(FACTORIZATION)
        del changed["verified_product"]
        changed["checks"] = {}
        assert values.compare(d, "case__m2", changed) == [
            "value differs at /checks",
            "value differs at /verified_product",
        ]


def test_a_missing_saved_value_is_a_problem():
    with tempfile.TemporaryDirectory() as d:
        assert values.compare(d, "nothing__asir", 1) == ["no saved value for nothing__asir"]


def test_unordered_lists_compare_as_multisets():
    swapped = copy(FACTORIZATION)
    swapped["factors"].reverse()
    assert values.differences(FACTORIZATION, swapped, unordered=[]) != []
    assert values.differences(FACTORIZATION, swapped, unordered=["/factors"]) == []
    # A different element is still a difference.
    other = copy(swapped)
    other["factors"][0]["display"] = "x^2+y^2"
    assert values.differences(FACTORIZATION, other, unordered=["/factors"]) == ["/factors"]
    # "*" stands for any index or key.
    nested = {"series": [{"terms": [1, 2]}, {"terms": [3]}]}
    reordered = {"series": [{"terms": [2, 1]}, {"terms": [3]}]}
    assert values.differences(nested, reordered, unordered=["/series/*/terms"]) == []


def test_names_become_safe_file_names():
    with tempfile.TemporaryDirectory() as d:
        values.save(d, "a/b c__m2", 1)
        assert [p.name for p in Path(d).iterdir()] == ["a_b_c__m2.json"]
        assert values.load(d, "a/b c__m2") == 1


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
