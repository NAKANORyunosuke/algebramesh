//! Issue #47: the core does not depend on the mathematical types, and the
//! checks do not depend on what they check, as `cargo metadata` resolves
//! them.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

/// The packages each workspace package depends on, transitively, by name
/// (normal dependencies only).
fn closure() -> BTreeMap<String, BTreeSet<String>> {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let name_of: BTreeMap<String, String> = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap().to_owned(),
                p["name"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let direct: BTreeMap<String, Vec<String>> = metadata["resolve"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            let deps = n["deps"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|d| {
                    d["dep_kinds"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|k| k["kind"].is_null())
                })
                .map(|d| d["pkg"].as_str().unwrap().to_owned())
                .collect();
            (n["id"].as_str().unwrap().to_owned(), deps)
        })
        .collect();
    let mut all = BTreeMap::new();
    for id in direct.keys() {
        let mut seen = BTreeSet::new();
        let mut stack = vec![id.clone()];
        while let Some(next) = stack.pop() {
            for d in &direct[&next] {
                if seen.insert(name_of[d].clone()) {
                    stack.push(d.clone());
                }
            }
        }
        all.insert(name_of[id].clone(), seen);
    }
    all
}

#[test]
fn the_core_and_the_checks_depend_on_what_they_may() {
    let closure = closure();
    let core = &closure["algebramesh-core"];
    assert!(!core.contains("algebramesh-math-types"), "{core:?}");
    assert!(!core.contains("algebramesh-ops"), "{core:?}");

    let verify = &closure["algebramesh-verify"];
    let ours: BTreeSet<&str> = verify
        .iter()
        .map(String::as_str)
        .filter(|n| n.starts_with("algebramesh-"))
        .collect();
    assert_eq!(
        ours,
        BTreeSet::from([
            "algebramesh-core",
            "algebramesh-math-types",
            "algebramesh-ops"
        ])
    );
}
