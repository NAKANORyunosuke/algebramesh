//! Allow-listed operations. Each one validates its typed arguments before any
//! connection is made and maps to a fixed Asir function; no user text reaches the
//! Asir parser (design §5.4, §11).

use algebramesh_core::CasError;
use algebramesh_core::check::Interrupt;
use algebramesh_math_types::{Factorization, Polynomial, PowerBudget, Rational};
use algebramesh_openxm_protocol::{Cmo, cmo_tag};
use algebramesh_ops::{FactorArguments, FactorResult};

use crate::convert::{
    MAX_USED_VARIABLES, cmo_to_polynomial, cmo_to_rational, cmo_to_u32, polynomial_to_cmo,
    used_variables,
};

pub use algebramesh_ops::POLYNOMIAL_FACTOR;

pub const OPERATIONS: &[&str] = &[POLYNOMIAL_FACTOR];

/// CMO tags that must appear in the server's mathcap for the operations here.
pub const REQUIRED_CMO_TAGS: &[u32] = &[
    cmo_tag::LIST,
    cmo_tag::ZZ,
    cmo_tag::QQ,
    cmo_tag::RECURSIVE_POLYNOMIAL,
    cmo_tag::UNIVARIATE_POLYNOMIAL,
    cmo_tag::INDETERMINATE,
];

#[derive(Debug, Clone)]
pub enum Operation {
    /// `fctr(p)` over QQ (or ZZ): irreducible factors with multiplicities.
    Factor(Polynomial),
}

impl Operation {
    pub fn parse(name: &str, arguments: &serde_json::Value) -> Result<Self, CasError> {
        match name {
            POLYNOMIAL_FACTOR => {
                let args: FactorArguments = serde_json::from_value(arguments.clone())
                    .map_err(|e| CasError::InvalidArguments(e.to_string()))?;
                let p = Polynomial::from_data(args.ring, &args.expression)
                    .map_err(|e| CasError::InvalidArguments(e.to_string()))?;
                let used = used_variables(&p);
                if used > MAX_USED_VARIABLES {
                    return Err(CasError::InvalidArguments(format!(
                        "{used} variables occur in the polynomial; at most {MAX_USED_VARIABLES} \
                         can be sent to Asir"
                    )));
                }
                Ok(Self::Factor(p))
            }
            other => Err(CasError::UnsupportedOperation {
                backend: crate::NAME.into(),
                operation: other.into(),
            }),
        }
    }

    pub fn function(&self) -> (&'static str, Vec<Cmo>) {
        match self {
            Self::Factor(p) => ("fctr", vec![polynomial_to_cmo(p)]),
        }
    }

    /// Converts the raw CMO result into the typed JSON result, stopping at the
    /// checkpoints once `interrupt` is raised. The product of the factors is
    /// compared with the input after the backend, by `algebramesh-verify`.
    pub fn interpret(
        &self,
        result: &Cmo,
        interrupt: &Interrupt,
    ) -> Result<serde_json::Value, CasError> {
        match self {
            Self::Factor(input) => {
                let factorization = interpret_fctr(input, result, interrupt)?;
                interrupt.checkpoint()?;
                serde_json::to_value(FactorResult::new(factorization))
                    .map_err(|e| CasError::UnsupportedResult(e.to_string()))
            }
        }
    }
}

/// Asir's `fctr` returns `[[c, 1], [f_1, m_1], ...]` where the constants multiply
/// to the content and `f_i` are irreducible over QQ.
fn interpret_fctr(
    input: &Polynomial,
    result: &Cmo,
    interrupt: &Interrupt,
) -> Result<Factorization, CasError> {
    let bad = |detail: String| CasError::UnsupportedResult(format!("fctr result: {detail}"));
    let entries = result
        .as_list()
        .ok_or_else(|| bad(format!("expected a list, got CMO tag {}", result.tag())))?;
    let ring = input.ring().clone();
    let mut constant = Rational::one();
    let mut budget = PowerBudget::for_coefficients(input.terms().map(|(_, c)| c));
    let mut factors = Vec::new();
    for entry in entries {
        let pair = entry
            .as_list()
            .filter(|p| p.len() == 2)
            .ok_or_else(|| bad("entry is not a pair".into()))?;
        let multiplicity = cmo_to_u32(&pair[1]).map_err(|e| bad(e.to_string()))?;
        match &pair[0] {
            Cmo::RecursivePolynomial { .. } => {
                let f = cmo_to_polynomial(&pair[0], &ring).map_err(|e| bad(e.to_string()))?;
                factors.push((f, multiplicity));
            }
            number => {
                let c = cmo_to_rational(number).map_err(|e| bad(e.to_string()))?;
                constant = constant
                    .mul_pow_with(&c, multiplicity, &mut budget, &mut || {
                        interrupt.checkpoint()
                    })?
                    .map_err(|e| bad(e.to_string()))?;
            }
        }
    }
    Factorization::new(ring, constant, factors).map_err(|e| bad(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigInt;

    fn x4_minus_y4() -> serde_json::Value {
        serde_json::json!({
            "ring": {"domain": "QQ", "variables": ["x", "y"], "order": "grevlex"},
            "expression": {"type": "polynomial", "terms": [
                {"coefficient": "1", "exponents": [4, 0]},
                {"coefficient": "-1", "exponents": [0, 4]}
            ]}
        })
    }

    #[test]
    fn rejects_unknown_operation_and_bad_arguments() {
        assert!(matches!(
            Operation::parse("asir.eval", &serde_json::json!({})),
            Err(CasError::UnsupportedOperation { .. })
        ));
        let mut args = x4_minus_y4();
        args["expression"]["terms"][0]["exponents"] = serde_json::json!([4]);
        assert!(matches!(
            Operation::parse(POLYNOMIAL_FACTOR, &args),
            Err(CasError::InvalidArguments(_))
        ));
        let mut args = x4_minus_y4();
        args["script"] = serde_json::json!("shell(\"id\")");
        assert!(matches!(
            Operation::parse(POLYNOMIAL_FACTOR, &args),
            Err(CasError::InvalidArguments(_))
        ));
    }

    /// Issue #32: a polynomial in which too many variables occur is refused
    /// before it is encoded; many variables that do not occur are fine.
    #[test]
    fn too_many_used_variables_are_refused() {
        let n = MAX_USED_VARIABLES + 1;
        let variables: Vec<String> = (0..n).map(|i| format!("x{i}")).collect();
        let args = |exponents: Vec<u32>| {
            serde_json::json!({
                "ring": {"domain": "QQ", "variables": variables, "order": "grevlex"},
                "expression": {"type": "polynomial", "terms": [
                    {"coefficient": "1", "exponents": exponents}]},
            })
        };
        let err = Operation::parse(POLYNOMIAL_FACTOR, &args(vec![1; n])).unwrap_err();
        assert!(
            matches!(&err, CasError::InvalidArguments(m) if m.contains("at most")),
            "{err:?}"
        );
        let mut one = vec![0; n];
        one[0] = 2;
        assert!(Operation::parse(POLYNOMIAL_FACTOR, &args(one)).is_ok());
    }

    /// The value of a wrong factorization is written as it is, and refused by
    /// the check after the backend (Issue #47).
    #[test]
    fn a_wrong_factorization_is_refused_after_the_backend() {
        use algebramesh_core::guarantee::ResultVerifier;
        let op = Operation::parse(POLYNOMIAL_FACTOR, &x4_minus_y4()).unwrap();
        let Operation::Factor(p) = &op;
        let wrong = Cmo::List(vec![
            Cmo::List(vec![Cmo::Zz(BigInt::from(1)), Cmo::Zz(BigInt::from(1))]),
            Cmo::List(vec![polynomial_to_cmo(p), Cmo::Zz(BigInt::from(2))]),
        ]);
        let value = op.interpret(&wrong, &Interrupt::none()).unwrap();
        let spec = algebramesh_core::JobSpec {
            schema_version: 1,
            backend: crate::NAME.into(),
            operation: POLYNOMIAL_FACTOR.into(),
            arguments: x4_minus_y4(),
            limits: Default::default(),
            retry_policy: Default::default(),
            environment: Default::default(),
        };
        let result = algebramesh_core::CasResult {
            value,
            backend: algebramesh_core::BackendInfo::default(),
            execution_host: None,
            log: vec![],
            guarantee: None,
        };
        let refused = algebramesh_verify::Registry::standard()
            .verify(&spec, result, &Interrupt::none())
            .unwrap_err();
        assert!(
            matches!(&refused, CasError::UnsupportedResult(m) if m.contains("multiply to")),
            "{refused:?}"
        );
    }

    /// Issue #91: a constant factor of multiplicity 2^32 - 1 costs no
    /// multiplication per unit of multiplicity. A constant 1 is absorbed, and
    /// a constant 3, whose power has more than 2^16 bits, is refused; both
    /// within seconds.
    #[test]
    fn a_constant_of_huge_multiplicity_is_absorbed_or_refused_at_once() {
        let args = serde_json::json!({
            "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
            "expression": {"type": "polynomial", "terms": [
                {"coefficient": "1", "exponents": [1]}]}
        });
        let op = Operation::parse(POLYNOMIAL_FACTOR, &args).unwrap();
        let Operation::Factor(x) = &op;
        let reply = |c: i64| {
            Cmo::List(vec![
                Cmo::List(vec![
                    Cmo::Zz(BigInt::from(c)),
                    Cmo::Zz(BigInt::from(u32::MAX)),
                ]),
                Cmo::List(vec![polynomial_to_cmo(x), Cmo::Zz(BigInt::from(1))]),
            ])
        };
        let (one, three) = (reply(1), reply(3));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let one = op.interpret(&one, &Interrupt::none());
            let three = op.interpret(&three, &Interrupt::none());
            let _ = tx.send((one, three));
        });
        let (one, three) = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the constant is not absorbed within 10 s");
        assert_eq!(one.unwrap()["constant"], "1");
        assert!(
            matches!(&three, Err(CasError::UnsupportedResult(m)) if m.contains("too large")),
            "{three:?}"
        );
    }
}
