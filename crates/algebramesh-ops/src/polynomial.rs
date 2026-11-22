//! `polynomial.factor`.

use algebramesh_math_types::{Factorization, PolynomialData, Rational, RingSpec};
use serde::{Deserialize, Serialize};

pub const POLYNOMIAL_FACTOR: &str = "polynomial.factor";

/// The arguments of `polynomial.factor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactorArguments {
    pub ring: RingSpec,
    pub expression: PolynomialData,
}

/// The value of `polynomial.factor`: `constant * prod(factor ^ multiplicity)`
/// with irreducible factors, in the JSON every backend writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactorResult {
    #[serde(rename = "type")]
    pub kind: FactorizationKind,
    /// The ring of the input.
    pub ring: RingSpec,
    pub constant: Rational,
    pub factors: Vec<FactorEntry>,
    /// The factors multiply out to the input (ADR 0003): a result where they
    /// do not is refused by `algebramesh-verify`.
    pub verified_product: bool,
}

/// The `"type"` field of [`FactorResult`], which must be `"factorization"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactorizationKind {
    #[default]
    #[serde(rename = "factorization")]
    Factorization,
}

/// One irreducible factor with its multiplicity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactorEntry {
    pub factor: PolynomialData,
    pub multiplicity: u32,
    /// Display form for humans; not part of the mathematical value.
    pub display: String,
}

impl FactorResult {
    /// The value of `factorization`, as a backend writes it. Its product is
    /// compared with the input after the backend, by `algebramesh-verify`.
    pub fn new(factorization: Factorization) -> Self {
        Self {
            kind: FactorizationKind::Factorization,
            ring: factorization.ring,
            constant: factorization.constant,
            factors: factorization
                .factors
                .into_iter()
                .map(|f| FactorEntry {
                    factor: f.factor,
                    multiplicity: f.multiplicity,
                    display: f.display,
                })
                .collect(),
            verified_product: true,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use algebramesh_math_types::{CoefficientDomain, MonomialOrder, Polynomial};

    use super::*;

    fn ring() -> RingSpec {
        RingSpec {
            domain: CoefficientDomain::Rationals,
            variables: vec!["x".into(), "y".into()],
            order: MonomialOrder::Grevlex,
        }
    }

    fn poly(text: serde_json::Value) -> Polynomial {
        Polynomial::from_data(ring(), &serde_json::from_value(text).unwrap()).unwrap()
    }

    fn factorization() -> Factorization {
        let plus = poly(serde_json::json!({"type": "polynomial", "terms": [
            {"coefficient": "1", "exponents": [1, 0]}, {"coefficient": "1", "exponents": [0, 1]}]}));
        let minus = poly(serde_json::json!({"type": "polynomial", "terms": [
            {"coefficient": "1", "exponents": [1, 0]}, {"coefficient": "-1", "exponents": [0, 1]}]}));
        Factorization::new(ring(), Rational::integer(2), vec![(plus, 1), (minus, 2)]).unwrap()
    }

    /// The typed value writes the JSON the backends wrote before it: the
    /// factorization with `verified_product`.
    #[test]
    fn the_value_is_the_factorization_with_its_check() {
        let f = factorization();
        let mut before = serde_json::to_value(&f).unwrap();
        before["verified_product"] = serde_json::Value::Bool(true);
        let typed = FactorResult::new(f);
        assert_eq!(serde_json::to_value(&typed).unwrap(), before);
        let back: FactorResult = serde_json::from_value(before).unwrap();
        assert_eq!(back, typed);
    }

    #[test]
    fn unknown_keys_and_other_types_are_refused() {
        let mut value = serde_json::to_value(FactorResult::new(factorization())).unwrap();
        value["extra"] = serde_json::json!(1);
        assert!(serde_json::from_value::<FactorResult>(value.clone()).is_err());
        value.as_object_mut().unwrap().remove("extra");
        value["type"] = serde_json::json!("gkz_system");
        assert!(serde_json::from_value::<FactorResult>(value).is_err());

        let args = serde_json::json!({
            "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
            "expression": {"type": "polynomial", "terms": []},
            "script": "quit;",
        });
        assert!(serde_json::from_value::<FactorArguments>(args).is_err());
    }
}
