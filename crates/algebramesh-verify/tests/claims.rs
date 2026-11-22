//! The catalogue of claims, `docs/claims.md`, follows the rules of ADR 0018
//! (Issues #61, #62): every claim has its items, every premise is in the
//! catalogue and the effective levels follow from them, and every `checked`
//! claim names a mutation test: the one test function of the workspace with
//! that name, which neither `#[ignore]` nor a `cfg` leaves out (Issue #82).
//! That it runs rests on CI running `cargo test --workspace --all-features`;
//! the scan does not read the results of the tests. The features of the
//! research packages (`pkg-<name>`, Issue #100) are on in that run, so a test
//! that only they leave out counts.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use algebramesh_verify::catalogue::{self, Catalogue, Level};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_catalogue() -> Catalogue {
    let text = std::fs::read_to_string(workspace().join("docs/claims.md")).unwrap();
    catalogue::parse(&text)
}

/// A test function that the scan found.
struct Found {
    file: PathBuf,
    /// Whether `cargo test --all-features` runs it: no `#[ignore]` and no
    /// `cfg` but `cfg(test)` and those of the package features leaves it out.
    runs: bool,
}

/// Whether an attribute makes what it is on depend on a configuration other
/// than that of the tests: `cfg(...)` but `cfg(test)` and a package feature
/// (`cfg(feature = "pkg-<name>")`, on in CI's run with all features), or
/// `cfg_attr(...)`.
fn conditional(attribute: &str) -> bool {
    let a = attribute.trim_start_matches('#').trim_start_matches('!');
    let package = a
        .strip_prefix("[cfg(feature = \"pkg-")
        .and_then(|r| r.strip_suffix("\")]"))
        .is_some_and(|name| {
            !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
        });
    (a.starts_with("[cfg(") || a.starts_with("[cfg_attr("))
        && !a.starts_with("[cfg(test)]")
        && !package
}

/// The modules a Rust file declares in other files (`mod name;`) under a
/// [`conditional`] attribute: their files, and all below them, are left out.
fn left_out_modules(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut off = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with("#[") {
            off |= conditional(line);
            continue;
        }
        if line.starts_with("//") {
            continue;
        }
        if off {
            let declared = ["mod ", "pub mod ", "pub(crate) mod "]
                .iter()
                .find_map(|m| line.strip_prefix(m))
                .and_then(|r| r.strip_suffix(';'));
            if let Some(name) = declared {
                out.push(name.trim().to_owned());
            }
        }
        off = false;
    }
    out
}

/// The directory of the files of the modules `file` declares: its own for
/// `lib.rs`, `main.rs` and `mod.rs`, and `name/` for `name.rs`.
fn module_dir(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap().to_path_buf();
    match file.file_name().and_then(|n| n.to_str()) {
        Some("lib.rs" | "main.rs" | "mod.rs") => parent,
        _ => parent.join(file.file_stem().unwrap()),
    }
}

/// The test functions in the text of a Rust file, each with whether `cargo
/// test` runs it (Issue #82). A test function is a `fn` after an attribute
/// `#[test]` or `#[tokio::test(...)]`, with only attributes and comments
/// between them and the `fn`. It does not run when one of those attributes
/// is `#[ignore...]` or [`conditional`], or when the file is (`#![cfg(...)]`)
/// or a module above it is: a conditional module leaves out the rest of the
/// file, which errs on the safe side.
fn scan(text: &str) -> Vec<(String, bool)> {
    let mut found = Vec::new();
    let mut attributes: Vec<&str> = Vec::new();
    let mut rest_off = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with("#![") {
            rest_off |= conditional(line);
            continue;
        }
        if line.starts_with("#[") {
            attributes.push(line);
            continue;
        }
        if line.starts_with("//") {
            continue;
        }
        let test = attributes
            .iter()
            .any(|a| a.starts_with("#[test]") || a.starts_with("#[tokio::test"));
        let off = attributes
            .iter()
            .any(|a| a.starts_with("#[ignore") || conditional(a));
        if let Some(rest) = line
            .strip_prefix("fn ")
            .or_else(|| line.strip_prefix("async fn "))
        {
            if test {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                found.push((name, !off && !rest_off));
            }
        } else if off
            && ["mod ", "pub mod ", "pub(crate) mod "]
                .iter()
                .any(|m| line.starts_with(m))
        {
            rest_off = true;
        }
        attributes.clear();
    }
    found
}

/// The Rust files under `dir`.
fn rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}

/// The test functions in the Rust files under `dir`, by name. A file of a
/// module that its parent declares under a [`conditional`] attribute does
/// not run, nor does any file below it.
fn tests(dir: &Path, found: &mut BTreeMap<String, Vec<Found>>) {
    let mut files = Vec::new();
    rust_files(dir, &mut files);
    let texts: Vec<(PathBuf, String)> = files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            (path, text)
        })
        .collect();
    let mut off: Vec<PathBuf> = Vec::new();
    for (path, text) in &texts {
        for name in left_out_modules(text) {
            let dir = module_dir(path);
            off.push(dir.join(format!("{name}.rs")));
            off.push(dir.join(&name));
        }
    }
    for (path, text) in texts {
        let left_out = off.iter().any(|o| path == *o || path.starts_with(o));
        for (name, runs) in scan(&text) {
            found.entry(name).or_default().push(Found {
                file: path.clone(),
                runs: runs && !left_out,
            });
        }
    }
}

fn all_tests() -> BTreeMap<String, Vec<Found>> {
    let mut found = BTreeMap::new();
    tests(&workspace().join("crates"), &mut found);
    found
}

/// The claims of each operation, as Issue #61 and the milestones that added
/// operations name them.
const CLAIMS: [(&str, &[&str]); 16] = [
    (
        "polynomial.factor",
        &[
            "factor.product",
            "factor.domain",
            "factor.primitive",
            "factor.non_associate",
            "factor.irreducible",
        ],
    ),
    (
        "gkz.generate",
        &[
            "gkz.euler",
            "gkz.toric_in_IA",
            "gkz.toric_generates_IA",
            "gkz.generated_by_toric_and_euler",
            "gkz.membership[j]",
        ],
    ),
    (
        "gkz.indicial",
        &[
            "local.exponent_equations",
            "local.exponent_roots",
            "local.indicial_ideal",
            "local.roots_complete",
            "local.rank",
            "local.rank_sum",
            "local.parameters",
        ],
    ),
    (
        "gkz.series",
        &[
            "gkz.toric_in_IA",
            "gkz.toric_generates_IA",
            "gkz.generated_by_toric_and_euler",
            "local.exponent_equations",
            "local.exponent_roots",
            "local.indicial_ideal",
            "local.roots_complete",
            "local.rank",
            "local.rank_sum",
            "local.parameters",
            "series.shape",
            "series.euler_exact",
            "series.toric_to_order",
            "series.not_a_solution",
            "series.canonical",
            "series.log_independent",
            "series.missing",
        ],
    ),
    ("matrix.rank", &["rank.lower", "rank.upper"]),
    (
        "matrix.kernel",
        &["kernel.contained", "kernel.independent", "kernel.spanning"],
    ),
    ("matrix.solve", &["solve.solution", "solve.no_solution"]),
    (
        "matrix.hermite_normal_form",
        &["hnf.form", "hnf.equivalent"],
    ),
    ("matrix.smith_normal_form", &["snf.form", "snf.equivalent"]),
    (
        "ideal.groebner_basis",
        &[
            "gb.reduced_form",
            "gb.contained",
            "gb.contains",
            "gb.groebner",
        ],
    ),
    (
        "ideal.normal_form",
        &[
            "gb.reduced_form",
            "gb.contained",
            "gb.contains",
            "gb.groebner",
            "normal_form.reduced",
            "normal_form.unique",
        ],
    ),
    (
        "ideal.membership",
        &[
            "gb.reduced_form",
            "gb.contains",
            "gb.groebner",
            "normal_form.reduced",
            "membership.in",
            "membership.not_in",
        ],
    ),
    ("module.syzygies", &["syz.annihilates", "syz.generates"]),
    (
        "complex.free_resolution",
        &[
            "res.complex",
            "res.image",
            "res.input_homogeneous",
            "res.differentials_homogeneous",
            "res.entries_in_maximal_ideal",
            "res.exact",
            "res.minimal",
            "res.betti",
        ],
    ),
    ("ideal.hilbert_series", &["hilbert.series"]),
    (
        "simplicial.homology",
        &[
            "homology.boundary",
            "homology.snf_form",
            "homology.snf_equivalent",
            "homology.groups",
            "homology.euler_characteristic",
        ],
    ),
];

#[test]
fn the_catalogue_follows_the_rules() {
    let catalogue = read_catalogue();
    let found = all_tests();
    // A mutation test counts when exactly one test function has its name and
    // `cargo test` runs it (Issue #82); say why one does not.
    let mut why = Vec::new();
    for claim in catalogue.claims() {
        if claim.level() != Some(Level::Checked) {
            continue;
        }
        for test in claim.names("変異の試験") {
            match found.get(&test).map(Vec::as_slice) {
                Some([one]) if !one.runs => why.push(format!(
                    "{}: {test}, in {}, is ignored or left out by a cfg",
                    claim.id,
                    one.file.display()
                )),
                Some(many) if many.len() > 1 => why.push(format!(
                    "{}: {} test functions are named {test}: {:?}",
                    claim.id,
                    many.len(),
                    many.iter()
                        .map(|f| f.file.display().to_string())
                        .collect::<Vec<_>>()
                )),
                _ => {}
            }
        }
    }
    assert!(why.is_empty(), "{why:#?}");
    let runs = |test: &str| matches!(found.get(test).map(Vec::as_slice), Some([one]) if one.runs);
    let problems = catalogue::check(&catalogue, runs);
    assert!(problems.is_empty(), "{problems:#?}");
    // Each operation has its claims, no more and no fewer.
    let listed: Vec<&str> = catalogue
        .operations
        .iter()
        .map(|o| o.name.as_str())
        .collect();
    let expected: Vec<&str> = CLAIMS.iter().map(|(o, _)| *o).collect();
    assert_eq!(listed, expected);
    for (operation, claims) in CLAIMS {
        let listed: Vec<&str> = catalogue
            .operation(operation)
            .unwrap()
            .claims
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(listed, claims, "{operation}");
    }
}

/// The scan finds test functions, not helpers or the methods of the checker.
#[test]
fn only_test_functions_count_as_mutation_tests() {
    let names = all_tests();
    assert!(names.contains_key("the_catalogue_follows_the_rules"));
    assert!(names.contains_key("a_rank_too_high_is_refused"));
    assert!(!names.contains_key("asserted") && !names.contains_key("checked"));
    assert!(!names.contains_key("read_catalogue") && !names.contains_key("tests"));
}

/// Issue #82: a test that `cargo test` leaves out does not count, whether
/// `#[ignore]` or a `cfg` leaves it out, on the function, its module or its
/// file.
#[test]
fn a_test_that_does_not_run_does_not_count() {
    let one = |text: &str, expected: bool| {
        assert_eq!(scan(text), [("t".to_string(), expected)], "{text}");
    };
    one("#[test]\nfn t() {}", true);
    one("#[test]\n#[should_panic]\n// why\nfn t() {}", true);
    one(
        "#[tokio::test(flavor = \"multi_thread\")]\nasync fn t() {}",
        true,
    );
    one(
        "#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}",
        true,
    );
    one("#[test]\n#[ignore]\nfn t() {}", false);
    one("#[ignore = \"by hand\"]\n#[test]\nfn t() {}", false);
    one("#[test]\n#[cfg(unix)]\nfn t() {}", false);
    one("#[test]\n#[cfg_attr(windows, ignore)]\nfn t() {}", false);
    one("#![cfg(feature = \"x\")]\n#[test]\nfn t() {}", false);
    one(
        "#[cfg(feature = \"x\")]\nmod m {\n    #[test]\n    fn t() {}\n}",
        false,
    );
    // Issue #100: CI runs the tests with all features, those of the
    // packages too.
    one(
        "#[cfg(feature = \"pkg-gkz\")]\nmod m {\n    #[test]\n    fn t() {}\n}",
        true,
    );
    one("#![cfg(feature = \"pkg-gkz\")]\n#[test]\nfn t() {}", true);
    one("#![cfg(feature = \"pkg-gkz-x\")]\n#[test]\nfn t() {}", true);
    one("#![cfg(feature = \"pkg-\")]\n#[test]\nfn t() {}", false);
    one(
        "#![cfg(not(feature = \"pkg-gkz\"))]\n#[test]\nfn t() {}",
        false,
    );
    // A module in a file of its own, declared under a cfg.
    assert_eq!(
        left_out_modules(
            "#[cfg(unix)]\nmod a;\n#[cfg(feature = \"pkg-gkz\")]\npub(crate) mod b;\nmod c;\n#[cfg(windows)]\nmod d {}"
        ),
        ["a"]
    );
    assert_eq!(
        module_dir(Path::new("src/packages/mod.rs")),
        Path::new("src/packages")
    );
    assert_eq!(module_dir(Path::new("src/lib.rs")), Path::new("src"));
    assert_eq!(module_dir(Path::new("src/a.rs")), Path::new("src/a"));
    // A helper is no test, and a line between the attribute and the `fn`
    // ends the attributes.
    assert!(scan("fn t() {}").is_empty());
    assert!(scan("#[test]\nstruct S;\nfn t() {}").is_empty());
    // The mutation of the review: a registered test with #[ignore] added.
    let path = workspace().join("crates/algebramesh-verify/tests/matrix.rs");
    let text = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
    let name = "a_rank_too_high_is_refused";
    assert!(scan(&text).contains(&(name.to_string(), true)));
    let ignored = text.replace(
        &format!("#[test]\nfn {name}()"),
        &format!("#[test]\n#[ignore]\nfn {name}()"),
    );
    assert_ne!(ignored, text);
    assert!(scan(&ignored).contains(&(name.to_string(), false)));
}

/// Issue #61: the premises that decide the effective levels of the series.
#[test]
fn the_series_take_the_levels_of_their_premises() {
    let catalogue = read_catalogue();
    let premises = |id: &str| -> Vec<String> {
        let premises = catalogue.claim(id).unwrap().premises().unwrap();
        premises.into_iter().map(|(p, _)| p).collect()
    };
    assert!(premises("series.toric_to_order").contains(&"gkz.toric_generates_IA".into()));
    assert_eq!(
        catalogue.effective_level("series.toric_to_order"),
        Some(Level::Asserted)
    );
    assert!(premises("local.exponent_roots").contains(&"local.indicial_ideal".into()));
    assert_eq!(premises("series.not_a_solution"), ["gkz.toric_in_IA"]);
    assert_eq!(
        catalogue.effective_level("series.not_a_solution"),
        Some(Level::Checked)
    );
}

/// Issue #67: the normal form takes the weakest level of its four premises,
/// and `membership.not_in` does without `gb.contained`; each reads its
/// premises in its own operation. Issue #68: `gb.groebner` is checked by
/// the S-pairs in each of the three operations, so both are checked in
/// effect.
#[test]
fn normal_forms_and_memberships_take_the_levels_of_their_premises() {
    let catalogue = read_catalogue();
    let premises = |operation: &str, id: &str| -> Vec<(String, bool)> {
        let entry = catalogue.claim_in(operation, id).unwrap();
        assert_eq!(entry.operation, operation);
        entry.premises().unwrap()
    };
    let unique = premises("ideal.normal_form", "normal_form.unique");
    assert_eq!(
        unique,
        [
            ("normal_form.reduced".into(), false),
            ("gb.contained".into(), false),
            ("gb.contains".into(), false),
            ("gb.groebner".into(), false),
        ]
    );
    assert_eq!(
        catalogue.effective_level_in("ideal.normal_form", "normal_form.unique"),
        Some(Level::Checked)
    );
    // Only for a value with a certificate, and without gb.contained.
    let not_in = premises("ideal.membership", "membership.not_in");
    assert_eq!(
        not_in,
        [
            ("normal_form.reduced".into(), true),
            ("gb.contains".into(), true),
            ("gb.groebner".into(), true),
        ]
    );
    assert_eq!(
        catalogue.effective_level_in("ideal.membership", "membership.not_in"),
        Some(Level::Checked)
    );
    // gb.groebner is checked by a certificate in each operation, and each
    // listing names a mutation test that runs the check in that operation.
    let groebner = |operation: &str| catalogue.claim_in(operation, "gb.groebner").unwrap();
    for operation in [
        "ideal.groebner_basis",
        "ideal.normal_form",
        "ideal.membership",
    ] {
        assert_eq!(groebner(operation).operation, operation);
        assert_eq!(groebner(operation).level(), Some(Level::Checked));
        assert_eq!(groebner(operation).methods(), ["certificate"]);
    }
    assert!(
        groebner("ideal.groebner_basis")
            .names("変異の試験")
            .contains(&"an_order_mix_up_is_refused_by_its_s_pairs".into())
    );
    assert_ne!(
        groebner("ideal.normal_form").names("変異の試験"),
        groebner("ideal.membership").names("変異の試験")
    );
    assert_eq!(
        catalogue.effective_level_in("ideal.membership", "membership.in"),
        Some(Level::Checked)
    );
    // Each listing of gb.reduced_form names a mutation test of its own
    // operation.
    let tests = |operation: &str| {
        catalogue
            .claim_in(operation, "gb.reduced_form")
            .unwrap()
            .names("変異の試験")
    };
    assert_ne!(tests("ideal.groebner_basis"), tests("ideal.normal_form"));
    assert_ne!(tests("ideal.normal_form"), tests("ideal.membership"));
}
