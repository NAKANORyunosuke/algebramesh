//! The operations of AlgebraMesh in one place (Issue #54, ADR 0022): their
//! names, the types of their arguments and results, and a descriptor of each.
//!
//! Backends read their arguments with these types, clients submit with them,
//! and verifiers (A1) read the results with them. A backend declares the
//! signature of each operation it offers from [`OperationDescriptor`]
//! (`Capabilities.signatures`), so that a client can tell what a result is
//! before it passes the result to another operation.
//!
//! The results of the `gkz.*` operations embed reports of `algebramesh-gkz`
//! (`ParameterReport`, `Subtraction`), which this crate cannot depend on; their
//! typed results come with the GKZ package (K0 in `docs/roadmap.md`). Until
//! then [`GkzGenerate`], [`GkzIndicial`] and [`GkzSeries`] give their results
//! as JSON. So does [`SimplicialHomology`], whose typed result comes with the
//! topology package (`algebramesh-topology`, Issue #103).

pub mod certificate;
mod gkz;
pub mod ideal;
pub mod matrix;
pub mod module;
mod polynomial;
pub mod topology;
pub mod types;

pub use gkz::{
    GKZ_GENERATE, GKZ_INDICIAL, GKZ_SERIES, GkzGenerateArguments, GkzIndicialArguments,
    GkzSeriesArguments,
};
pub use polynomial::{
    FactorArguments, FactorEntry, FactorResult, FactorizationKind, POLYNOMIAL_FACTOR,
};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// What an operation takes and gives, by type name ([`types`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationDescriptor {
    /// The operation's name, e.g. `polynomial.factor`.
    pub name: &'static str,
    /// The type of each argument, by its key in `arguments`.
    pub arguments: &'static [(&'static str, &'static str)],
    /// The type of the result value.
    pub result: &'static str,
    /// Whether the value depends on the monomial order of a ring among the
    /// arguments, so that only a value computed in the same order may stand
    /// for it.
    pub order_dependent: bool,
    /// Whether the same arguments give the same value on the same build of a
    /// backend.
    pub deterministic: bool,
}

/// An operation with its typed arguments and result.
pub trait Operation {
    const DESCRIPTOR: OperationDescriptor;
    type Arguments: Serialize + DeserializeOwned;
    type Result: Serialize + DeserializeOwned;
}

/// `polynomial.factor`: the irreducible factors of a polynomial over the
/// coefficient domain of its ring, with their multiplicities.
pub struct PolynomialFactor;

impl Operation for PolynomialFactor {
    const DESCRIPTOR: OperationDescriptor = OperationDescriptor {
        name: POLYNOMIAL_FACTOR,
        arguments: &[("ring", types::RING), ("expression", types::POLYNOMIAL)],
        result: types::FACTORIZATION,
        order_dependent: false,
        deterministic: true,
    };
    type Arguments = FactorArguments;
    type Result = FactorResult;
}

/// `gkz.generate`: generators of the GKZ system `H_A(beta)`.
pub struct GkzGenerate;

impl Operation for GkzGenerate {
    const DESCRIPTOR: OperationDescriptor = OperationDescriptor {
        name: GKZ_GENERATE,
        arguments: &[
            ("A", types::INTEGER_MATRIX),
            ("beta", types::RATIONAL_VECTOR),
            ("expected", types::WEYL_OPERATOR_LIST),
        ],
        result: types::GKZ_SYSTEM,
        order_dependent: false,
        deterministic: true,
    };
    type Arguments = GkzGenerateArguments;
    type Result = serde_json::Value;
}

/// `gkz.indicial`: the indicial ideal of `H_A(beta)` for a weight vector, its
/// roots with their multiplicities, and the holonomic rank.
pub struct GkzIndicial;

impl Operation for GkzIndicial {
    const DESCRIPTOR: OperationDescriptor = OperationDescriptor {
        name: GKZ_INDICIAL,
        arguments: &[
            ("A", types::INTEGER_MATRIX),
            ("beta", types::RATIONAL_VECTOR),
            ("weight", types::INTEGER_VECTOR),
        ],
        result: types::GKZ_INDICIAL,
        order_dependent: false,
        deterministic: true,
    };
    type Arguments = GkzIndicialArguments;
    type Result = serde_json::Value;
}

/// `gkz.series`: the series solutions of `H_A(beta)` for a weight vector, up
/// to a weight order.
pub struct GkzSeries;

impl Operation for GkzSeries {
    const DESCRIPTOR: OperationDescriptor = OperationDescriptor {
        name: GKZ_SERIES,
        arguments: &[
            ("A", types::INTEGER_MATRIX),
            ("beta", types::RATIONAL_VECTOR),
            ("weight", types::INTEGER_VECTOR),
            ("order", types::NONNEGATIVE_INTEGER),
        ],
        result: types::GKZ_SERIES,
        order_dependent: false,
        deterministic: true,
    };
    type Arguments = GkzSeriesArguments;
    type Result = serde_json::Value;
}

/// The arguments of the `matrix.*` operations but `matrix.solve`.
const MATRIX_ARGUMENTS: &[(&str, &str)] = &[
    ("domain", types::COEFFICIENT_DOMAIN),
    ("matrix", types::MATRIX),
    ("certificate", types::CERTIFICATE_REQUEST),
];

macro_rules! matrix_operation {
    ($(#[$doc:meta])* $marker:ident, $name:expr, $arguments:expr, $args:ty, $result_type:expr, $result:ty) => {
        $(#[$doc])*
        pub struct $marker;

        impl Operation for $marker {
            const DESCRIPTOR: OperationDescriptor = OperationDescriptor {
                name: $name,
                arguments: $arguments,
                result: $result_type,
                order_dependent: false,
                deterministic: true,
            };
            type Arguments = $args;
            type Result = $result;
        }
    };
}

matrix_operation!(
    /// `matrix.rank`: the rank of A over QQ.
    MatrixRank,
    matrix::MATRIX_RANK,
    MATRIX_ARGUMENTS,
    matrix::MatrixArguments,
    types::MATRIX_RANK,
    matrix::RankResult
);
matrix_operation!(
    /// `matrix.kernel`: a basis of {x : A x = 0} over the domain.
    MatrixKernel,
    matrix::MATRIX_KERNEL,
    MATRIX_ARGUMENTS,
    matrix::MatrixArguments,
    types::MATRIX_KERNEL,
    matrix::KernelResult
);
matrix_operation!(
    /// `matrix.solve`: a solution of A x = b over the domain, or a proof that
    /// there is none.
    MatrixSolve,
    matrix::MATRIX_SOLVE,
    &[
        ("domain", types::COEFFICIENT_DOMAIN),
        ("matrix", types::MATRIX),
        ("rhs", types::RATIONAL_VECTOR),
        ("certificate", types::CERTIFICATE_REQUEST),
    ],
    matrix::SolveArguments,
    types::MATRIX_SOLUTION,
    matrix::SolveResult
);
matrix_operation!(
    /// `matrix.hermite_normal_form`: the row Hermite normal form over ZZ.
    MatrixHermiteNormalForm,
    matrix::MATRIX_HERMITE_NORMAL_FORM,
    MATRIX_ARGUMENTS,
    matrix::MatrixArguments,
    types::HERMITE_NORMAL_FORM,
    matrix::HermiteResult
);
matrix_operation!(
    /// `matrix.smith_normal_form`: the Smith normal form over ZZ.
    MatrixSmithNormalForm,
    matrix::MATRIX_SMITH_NORMAL_FORM,
    MATRIX_ARGUMENTS,
    matrix::MatrixArguments,
    types::SMITH_NORMAL_FORM,
    matrix::SmithResult
);

/// `ideal.groebner_basis` (Issue #66): the reduced Groebner basis of an
/// ideal over QQ in the monomial order of its ring, with the basis and the
/// generators in terms of each other.
pub struct IdealGroebnerBasis;

impl Operation for IdealGroebnerBasis {
    const DESCRIPTOR: OperationDescriptor = OperationDescriptor {
        name: ideal::IDEAL_GROEBNER_BASIS,
        arguments: &[
            ("ideal", types::IDEAL),
            ("certificate", types::IDEAL_CERTIFICATE_REQUEST),
        ],
        result: types::GROEBNER_BASIS,
        order_dependent: true,
        deterministic: true,
    };
    type Arguments = ideal::GroebnerBasisArguments;
    type Result = ideal::GroebnerBasisResult;
}

/// The arguments of `ideal.normal_form` and `ideal.membership`.
const IDEAL_POLYNOMIAL_ARGUMENTS: &[(&str, &str)] = &[
    ("ideal", types::IDEAL),
    ("polynomial", types::POLYNOMIAL),
    ("certificate", types::IDEAL_CERTIFICATE_REQUEST),
];

/// `ideal.normal_form` (Issue #67): the normal form of a polynomial modulo
/// an ideal over QQ in the monomial order of its ring, with the reduced
/// Groebner basis it is taken with and the quotients.
pub struct IdealNormalForm;

impl Operation for IdealNormalForm {
    const DESCRIPTOR: OperationDescriptor = OperationDescriptor {
        name: ideal::IDEAL_NORMAL_FORM,
        arguments: IDEAL_POLYNOMIAL_ARGUMENTS,
        result: types::NORMAL_FORM,
        order_dependent: true,
        deterministic: true,
    };
    type Arguments = ideal::IdealPolynomialArguments;
    type Result = ideal::NormalFormResult;
}

/// `ideal.membership` (Issue #67): whether a polynomial lies in an ideal
/// over QQ, with its coefficients in the generators when it does and its
/// normal form when it does not.
pub struct IdealMembership;

impl Operation for IdealMembership {
    const DESCRIPTOR: OperationDescriptor = OperationDescriptor {
        name: ideal::IDEAL_MEMBERSHIP,
        arguments: IDEAL_POLYNOMIAL_ARGUMENTS,
        result: types::IDEAL_MEMBERSHIP,
        // Whether f lies in I does not depend on the order; the certificate
        // of `member: false` does.
        order_dependent: true,
        deterministic: true,
    };
    type Arguments = ideal::IdealPolynomialArguments;
    type Result = ideal::MembershipResult;
}

/// `module.syzygies` (Issue #97): generators of the syzygies of the
/// generators of a submodule of a free module over QQ.
pub struct ModuleSyzygies;

impl Operation for ModuleSyzygies {
    const DESCRIPTOR: OperationDescriptor = OperationDescriptor {
        name: module::MODULE_SYZYGIES,
        arguments: &[("module", types::SUBMODULE)],
        result: types::SYZYGIES,
        // Any generators of the syzygies stand for them.
        order_dependent: false,
        deterministic: true,
    };
    type Arguments = module::SyzygiesArguments;
    type Result = module::SyzygiesResult;
}

/// `complex.free_resolution` (Issue #97): a free resolution of R/I for an
/// ideal I over QQ, graded with its Betti numbers when it is minimal.
pub struct ComplexFreeResolution;

impl Operation for ComplexFreeResolution {
    const DESCRIPTOR: OperationDescriptor = OperationDescriptor {
        name: module::COMPLEX_FREE_RESOLUTION,
        arguments: &[
            ("ideal", types::IDEAL),
            ("certificate", types::CERTIFICATE_REQUEST),
        ],
        result: types::FREE_RESOLUTION,
        order_dependent: false,
        deterministic: true,
    };
    type Arguments = module::FreeResolutionArguments;
    type Result = module::FreeResolutionResult;
}

/// `ideal.hilbert_series` (Issue #98): the Hilbert series of R/I for a
/// homogeneous ideal I over QQ.
pub struct IdealHilbertSeries;

impl Operation for IdealHilbertSeries {
    const DESCRIPTOR: OperationDescriptor = OperationDescriptor {
        name: module::IDEAL_HILBERT_SERIES,
        arguments: &[("ideal", types::IDEAL), ("betti_table", types::BOOLEAN)],
        result: types::HILBERT_SERIES,
        order_dependent: false,
        deterministic: true,
    };
    type Arguments = module::HilbertSeriesArguments;
    type Result = module::HilbertSeriesResult;
}

/// `simplicial.homology` (Issue #103): the homology over ZZ of a simplicial
/// complex, from the Smith normal forms of its boundary matrices.
pub struct SimplicialHomology;

impl Operation for SimplicialHomology {
    const DESCRIPTOR: OperationDescriptor = OperationDescriptor {
        name: topology::SIMPLICIAL_HOMOLOGY,
        arguments: &[
            ("complex", types::SIMPLICIAL_COMPLEX),
            ("domain", types::COEFFICIENT_DOMAIN),
            ("certificate", types::CERTIFICATE_REQUEST),
        ],
        result: types::SIMPLICIAL_HOMOLOGY,
        order_dependent: false,
        deterministic: true,
    };
    type Arguments = topology::SimplicialHomologyArguments;
    type Result = serde_json::Value;
}

/// Every operation of this crate.
pub const DESCRIPTORS: &[OperationDescriptor] = &[
    PolynomialFactor::DESCRIPTOR,
    GkzGenerate::DESCRIPTOR,
    GkzIndicial::DESCRIPTOR,
    GkzSeries::DESCRIPTOR,
    MatrixRank::DESCRIPTOR,
    MatrixKernel::DESCRIPTOR,
    MatrixSolve::DESCRIPTOR,
    MatrixHermiteNormalForm::DESCRIPTOR,
    MatrixSmithNormalForm::DESCRIPTOR,
    IdealGroebnerBasis::DESCRIPTOR,
    IdealNormalForm::DESCRIPTOR,
    IdealMembership::DESCRIPTOR,
    ModuleSyzygies::DESCRIPTOR,
    ComplexFreeResolution::DESCRIPTOR,
    IdealHilbertSeries::DESCRIPTOR,
    SimplicialHomology::DESCRIPTOR,
];

/// The descriptor of the operation `name`, if this crate defines it.
pub fn descriptor(name: &str) -> Option<&'static OperationDescriptor> {
    DESCRIPTORS.iter().find(|d| d.name == name)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::BTreeSet;

    use super::*;

    /// The keys of a typed value of `T`, as serde writes them.
    fn keys<T: Serialize>(value: &T) -> BTreeSet<String> {
        serde_json::to_value(value)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    fn declared(d: &OperationDescriptor) -> BTreeSet<String> {
        d.arguments.iter().map(|(k, _)| (*k).to_owned()).collect()
    }

    #[test]
    fn names_are_unique_and_found() {
        let names: BTreeSet<&str> = DESCRIPTORS.iter().map(|d| d.name).collect();
        assert_eq!(names.len(), DESCRIPTORS.len());
        for d in DESCRIPTORS {
            assert_eq!(descriptor(d.name), Some(d));
        }
        assert_eq!(descriptor("polynomial.expand"), None);
    }

    /// The descriptors name exactly the keys the argument types read.
    #[test]
    fn descriptors_name_the_keys_of_the_arguments() {
        let factor: FactorArguments = serde_json::from_value(serde_json::json!({
            "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
            "expression": {"type": "polynomial", "terms": [
                {"coefficient": "1", "exponents": [1]}]},
        }))
        .unwrap();
        assert_eq!(keys(&factor), declared(&PolynomialFactor::DESCRIPTOR));

        let a = serde_json::json!([[1, 1, 1], [0, 1, 2]]);
        let generate: GkzGenerateArguments = serde_json::from_value(serde_json::json!({
            "A": a, "beta": ["1", "1/2"],
            "expected": [[{"coefficient": "1", "x_exponents": [0, 0, 0],
                           "d_exponents": [1, 0, 0]}]],
        }))
        .unwrap();
        assert_eq!(keys(&generate), declared(&GkzGenerate::DESCRIPTOR));

        let indicial: GkzIndicialArguments = serde_json::from_value(
            serde_json::json!({"A": a, "beta": ["1", "1/2"], "weight": [1, 0, 1]}),
        )
        .unwrap();
        assert_eq!(keys(&indicial), declared(&GkzIndicial::DESCRIPTOR));

        let series: GkzSeriesArguments = serde_json::from_value(serde_json::json!({
            "A": a, "beta": ["1", "1/2"], "weight": [1, 0, 1], "order": 4,
        }))
        .unwrap();
        assert_eq!(keys(&series), declared(&GkzSeries::DESCRIPTOR));

        let matrix: matrix::MatrixArguments =
            serde_json::from_value(serde_json::json!({"domain": "ZZ", "matrix": [[2, 4], [6, 8]]}))
                .unwrap();
        for d in [
            MatrixRank::DESCRIPTOR,
            MatrixKernel::DESCRIPTOR,
            MatrixHermiteNormalForm::DESCRIPTOR,
            MatrixSmithNormalForm::DESCRIPTOR,
        ] {
            assert_eq!(keys(&matrix), declared(&d), "{}", d.name);
        }
        let solve: matrix::SolveArguments = serde_json::from_value(
            serde_json::json!({"domain": "QQ", "matrix": [[1, 2]], "rhs": ["1/2"]}),
        )
        .unwrap();
        assert_eq!(keys(&solve), declared(&MatrixSolve::DESCRIPTOR));

        let groebner: ideal::GroebnerBasisArguments = serde_json::from_value(serde_json::json!({
            "ideal": {"type": "ideal",
                      "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
                      "generators": []},
        }))
        .unwrap();
        assert_eq!(keys(&groebner), declared(&IdealGroebnerBasis::DESCRIPTOR));

        let polynomial: ideal::IdealPolynomialArguments =
            serde_json::from_value(serde_json::json!({
                "ideal": {"type": "ideal",
                          "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
                          "generators": []},
                "polynomial": {"type": "polynomial", "terms": []},
            }))
            .unwrap();
        for d in [IdealNormalForm::DESCRIPTOR, IdealMembership::DESCRIPTOR] {
            assert_eq!(keys(&polynomial), declared(&d), "{}", d.name);
        }

        let ideal = serde_json::json!({"type": "ideal",
            "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"}, "generators": []});
        let syzygies: module::SyzygiesArguments = serde_json::from_value(serde_json::json!({
            "module": {"type": "submodule",
                       "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
                       "rank": 1,
                       "generators": {"type": "polynomial_matrix", "rows": 1, "columns": []}},
        }))
        .unwrap();
        assert_eq!(keys(&syzygies), declared(&ModuleSyzygies::DESCRIPTOR));
        let resolution: module::FreeResolutionArguments =
            serde_json::from_value(serde_json::json!({"ideal": ideal})).unwrap();
        assert_eq!(
            keys(&resolution),
            declared(&ComplexFreeResolution::DESCRIPTOR)
        );
        let series: module::HilbertSeriesArguments =
            serde_json::from_value(serde_json::json!({"ideal": ideal})).unwrap();
        assert_eq!(keys(&series), declared(&IdealHilbertSeries::DESCRIPTOR));

        let homology: topology::SimplicialHomologyArguments =
            serde_json::from_value(serde_json::json!({
                "complex": {"type": "simplicial_complex", "facets": [[0, 1]]},
                "domain": "ZZ",
            }))
            .unwrap();
        assert_eq!(keys(&homology), declared(&SimplicialHomology::DESCRIPTOR));
    }

    /// Only a value computed in the same order stands for a Groebner basis,
    /// a normal form, or a certificate of membership.
    #[test]
    fn a_groebner_basis_depends_on_the_order() {
        let ordered: Vec<&str> = DESCRIPTORS
            .iter()
            .filter(|d| d.order_dependent)
            .map(|d| d.name)
            .collect();
        assert_eq!(ordered, ideal::OPERATIONS);
    }

    #[test]
    fn type_names_are_known() {
        for d in DESCRIPTORS {
            for (_, ty) in d.arguments {
                assert!(types::ALL.contains(ty), "{} takes {ty}", d.name);
            }
            assert!(
                types::ALL.contains(&d.result),
                "{} gives {}",
                d.name,
                d.result
            );
        }
    }
}
