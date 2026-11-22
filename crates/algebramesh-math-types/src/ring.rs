use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::MathError;
use crate::scalar::Rational;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CoefficientDomain {
    #[serde(rename = "ZZ")]
    Integers,
    #[serde(rename = "QQ")]
    Rationals,
}

impl CoefficientDomain {
    pub fn name(self) -> &'static str {
        match self {
            Self::Integers => "ZZ",
            Self::Rationals => "QQ",
        }
    }

    pub fn contains(self, value: &Rational) -> bool {
        match self {
            Self::Integers => value.is_integer(),
            Self::Rationals => true,
        }
    }

    pub(crate) fn check(self, value: &Rational) -> Result<(), MathError> {
        if self.contains(value) {
            Ok(())
        } else {
            Err(MathError::CoefficientNotInDomain {
                coefficient: value.to_string(),
                domain: self.name(),
            })
        }
    }
}

impl fmt::Display for CoefficientDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// An order that a block of [`MonomialOrder::Block`], or the tie-break of
/// [`MonomialOrder::Weights`], may use (Issue #64). Its JSON form is the
/// string `"lex"`, `"grlex"` or `"grevlex"`.
///
/// For exponent vectors `a` and `b` of the variables `x_1, ..., x_n`, numbered
/// in the order the ring lists them (so `x_1 > ... > x_n` in each of these
/// orders), with `|a|` the sum of the entries of `a`:
/// - lex: `a > b` if the first nonzero entry of `a - b` is positive;
/// - grlex: `a > b` if `|a| > |b|`, or `|a| = |b|` and `a > b` in lex;
/// - grevlex: `a > b` if `|a| > |b|`, or `|a| = |b|` and the last nonzero
///   entry of `a - b` is negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BaseOrder {
    Lex,
    Grlex,
    Grevlex,
}

const BASE_ORDER_NAMES: &[&str] = &["lex", "grlex", "grevlex"];

/// The sum of the entries, in `u128`: fewer than `2^64` entries below `2^32`
/// cannot overflow it.
fn degree(a: &[u32]) -> u128 {
    a.iter().map(|&e| u128::from(e)).sum()
}

/// `w . a` in `u128`: each product is below `2^64`, so fewer than `2^64` of
/// them cannot overflow it.
fn weighted_degree(w: &[u32], a: &[u32]) -> u128 {
    w.iter()
        .zip(a)
        .map(|(&w, &e)| u128::from(w) * u128::from(e))
        .sum()
}

impl BaseOrder {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "lex" => Some(Self::Lex),
            "grlex" => Some(Self::Grlex),
            "grevlex" => Some(Self::Grevlex),
            _ => None,
        }
    }

    /// Compares the monomials with exponent vectors `a` and `b`, which have
    /// the same length.
    pub fn compare(self, a: &[u32], b: &[u32]) -> Ordering {
        match self {
            Self::Lex => a.cmp(b),
            Self::Grlex => degree(a).cmp(&degree(b)).then_with(|| a.cmp(b)),
            Self::Grevlex => degree(a).cmp(&degree(b)).then_with(|| {
                // The last entry where they differ decides: the smaller one
                // belongs to the larger monomial.
                a.iter()
                    .zip(b)
                    .rev()
                    .find(|(x, y)| x != y)
                    .map_or(Ordering::Equal, |(x, y)| y.cmp(x))
            }),
        }
    }
}

/// Accepts only the three strings: an object such as `{"lex": null}`, which
/// a derived implementation would take for the unit variant, is refused.
impl<'de> Deserialize<'de> for BaseOrder {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Name;
        impl Visitor<'_> for Name {
            type Value = BaseOrder;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(r#""lex", "grlex" or "grevlex""#)
            }

            fn visit_str<E: de::Error>(self, name: &str) -> Result<BaseOrder, E> {
                BaseOrder::from_name(name).ok_or_else(|| E::unknown_variant(name, BASE_ORDER_NAMES))
            }
        }
        deserializer.deserialize_str(Name)
    }
}

/// One block of [`MonomialOrder::Block`]: `count` consecutive variables
/// compared by `order`. Its JSON form is the object
/// `{"count": 2, "order": "grevlex"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct OrderBlock {
    pub count: usize,
    pub order: BaseOrder,
}

const BLOCK_ENTRY_FIELDS: &[&str] = &["count", "order"];

/// Accepts only the object form, each field once and no other field: a
/// derived implementation would also take the array `[2, "grevlex"]`, a
/// second spelling that is never written back.
impl<'de> Deserialize<'de> for OrderBlock {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entry;
        impl<'de> Visitor<'de> for Entry {
            type Value = OrderBlock;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(r#"a block {"count": ..., "order": ...}"#)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OrderBlock, A::Error> {
                let mut count = None;
                let mut order = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "count" if count.is_some() => {
                            return Err(de::Error::duplicate_field("count"));
                        }
                        "count" => count = Some(map.next_value()?),
                        "order" if order.is_some() => {
                            return Err(de::Error::duplicate_field("order"));
                        }
                        "order" => order = Some(map.next_value()?),
                        other => return Err(de::Error::unknown_field(other, BLOCK_ENTRY_FIELDS)),
                    }
                }
                Ok(OrderBlock {
                    count: count.ok_or_else(|| de::Error::missing_field("count"))?,
                    order: order.ok_or_else(|| de::Error::missing_field("order"))?,
                })
            }
        }
        deserializer.deserialize_map(Entry)
    }
}

/// The largest weight of [`MonomialOrder::Weights`], `2^31 - 1`. Macaulay2
/// 1.24.11 and OSCAR 1.8.2 both refuse larger weights.
pub const MAX_WEIGHT: u32 = (1 << 31) - 1;

/// A global monomial order on the variables `x_1, ..., x_n` of a ring, numbered
/// in the order the ring lists them (design §6, Issue #64). The base orders
/// have `x_1 > ... > x_n`; under `Weights` the weights rank the variables
/// first, so `{"weights": [0, 1], "then": "lex"}` has `x_2 > x_1`.
///
/// Negative weights, which give local and mixed orders, are out of scope: the
/// operations of A3 work in the polynomial ring, not in its localization
/// (ADR 0023).
///
/// The JSON form of `Lex`, `Grlex` and `Grevlex` is the string `"lex"`,
/// `"grlex"` or `"grevlex"`, of `Weights` the object
/// `{"weights": [1, 0, 2], "then": "grevlex"}` and of `Block` the object
/// `{"block": [{"count": 2, "order": "grevlex"}, {"count": 1, "order": "lex"}]}`.
/// Reading it refuses a negative weight and a weight above [`MAX_WEIGHT`];
/// [`MonomialOrder::validate`] checks the rest against the number of variables.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MonomialOrder {
    Lex,
    Grlex,
    Grevlex,
    /// By the weighted degree `w . a` first, ties broken by `then`. The
    /// weights are at most [`MAX_WEIGHT`] and there is one for each variable.
    /// They are non-negative, so the order is global and 1 is the smallest
    /// monomial.
    Weights {
        weights: Vec<u32>,
        then: BaseOrder,
    },
    /// The product (block) order: the exponents are cut into consecutive
    /// blocks of `count` variables, from the first variable on, and the first
    /// block where two monomials differ decides by its own order. The counts
    /// are positive and add up to the number of variables. The order
    /// eliminates the variables of the first blocks.
    Block(Vec<OrderBlock>),
}

impl From<BaseOrder> for MonomialOrder {
    fn from(order: BaseOrder) -> Self {
        match order {
            BaseOrder::Lex => Self::Lex,
            BaseOrder::Grlex => Self::Grlex,
            BaseOrder::Grevlex => Self::Grevlex,
        }
    }
}

impl MonomialOrder {
    /// Compares the monomials with exponent vectors `a` and `b`, which have
    /// the length the order was validated for (see [`MonomialOrder::validate`]).
    /// Vectors of other lengths give some ordering without panicking.
    pub fn compare(&self, a: &[u32], b: &[u32]) -> Ordering {
        match self {
            Self::Lex => BaseOrder::Lex.compare(a, b),
            Self::Grlex => BaseOrder::Grlex.compare(a, b),
            Self::Grevlex => BaseOrder::Grevlex.compare(a, b),
            Self::Weights { weights, then } => weighted_degree(weights, a)
                .cmp(&weighted_degree(weights, b))
                .then_with(|| then.compare(a, b)),
            Self::Block(blocks) => {
                let mut start = 0usize;
                for block in blocks {
                    let end = start.saturating_add(block.count).min(a.len()).min(b.len());
                    let order = block.order.compare(&a[start..end], &b[start..end]);
                    if order.is_ne() {
                        return order;
                    }
                    start = end;
                }
                Ordering::Equal
            }
        }
    }

    /// Checks the order for a ring of `nvars` variables: one weight for each
    /// variable, each at most [`MAX_WEIGHT`]; at least one block, each of at
    /// least one variable, with as many variables in all as the ring has.
    pub fn validate(&self, nvars: usize) -> Result<(), MathError> {
        match self {
            Self::Lex | Self::Grlex | Self::Grevlex => Ok(()),
            Self::Weights { weights, .. } => {
                if weights.len() != nvars {
                    return Err(MathError::Invalid(format!(
                        "{} weights for {nvars} variables",
                        weights.len()
                    )));
                }
                match weights.iter().find(|&&w| w > MAX_WEIGHT) {
                    Some(w) => Err(MathError::Invalid(format!(
                        "the weight {w} is above {MAX_WEIGHT}"
                    ))),
                    None => Ok(()),
                }
            }
            Self::Block(blocks) => {
                if blocks.is_empty() || blocks.iter().any(|b| b.count == 0) {
                    return Err(MathError::Invalid(
                        "a block order needs at least one block, of at least one variable each"
                            .into(),
                    ));
                }
                let total = blocks
                    .iter()
                    .try_fold(0usize, |sum, b| sum.checked_add(b.count));
                if total != Some(nvars) {
                    return Err(MathError::Invalid(format!(
                        "the blocks do not cover the {nvars} variables exactly"
                    )));
                }
                Ok(())
            }
        }
    }
}

const ORDER_FORMS: &str = r#"an order is "lex", "grlex", "grevlex", {"weights": [...], "then": ...} or {"block": [{"count": ..., "order": ...}, ...]}"#;

#[derive(Serialize)]
struct WeightsOut<'a> {
    weights: &'a [u32],
    then: BaseOrder,
}

#[derive(Serialize)]
struct BlockOut<'a> {
    block: &'a [OrderBlock],
}

/// One weight in the JSON form: an integer from 0 to [`MAX_WEIGHT`].
struct Weight(u32);

impl<'de> Deserialize<'de> for Weight {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Integer;
        impl Visitor<'_> for Integer {
            type Value = Weight;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a weight, an integer from 0 to {MAX_WEIGHT}")
            }

            fn visit_u64<E: de::Error>(self, w: u64) -> Result<Weight, E> {
                u32::try_from(w)
                    .ok()
                    .filter(|&w| w <= MAX_WEIGHT)
                    .map(Weight)
                    .ok_or_else(|| E::custom(format!("the weight {w} is above {MAX_WEIGHT}")))
            }

            fn visit_i64<E: de::Error>(self, w: i64) -> Result<Weight, E> {
                match u64::try_from(w) {
                    Ok(w) => self.visit_u64(w),
                    Err(_) => Err(E::custom(format!(
                        "the weight {w} is negative: only global orders are accepted"
                    ))),
                }
            }
        }
        deserializer.deserialize_u64(Integer)
    }
}

impl Serialize for MonomialOrder {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Lex => BaseOrder::Lex.serialize(serializer),
            Self::Grlex => BaseOrder::Grlex.serialize(serializer),
            Self::Grevlex => BaseOrder::Grevlex.serialize(serializer),
            Self::Weights { weights, then } => WeightsOut {
                weights,
                then: *then,
            }
            .serialize(serializer),
            Self::Block(blocks) => BlockOut { block: blocks }.serialize(serializer),
        }
    }
}

const WEIGHTS_FIELDS: &[&str] = &["weights", "then"];
const BLOCK_FIELDS: &[&str] = &["block"];

/// Chooses the form by the type of the value: a string is one of the three
/// orders, an object is read key by key, each key once. An error inside a
/// form is reported as it is (an untagged enum would only say that no variant
/// matched). This needs a self-describing format such as JSON.
impl<'de> Deserialize<'de> for MonomialOrder {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Order;
        impl<'de> Visitor<'de> for Order {
            type Value = MonomialOrder;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a monomial order: {ORDER_FORMS}")
            }

            fn visit_str<E: de::Error>(self, name: &str) -> Result<MonomialOrder, E> {
                BaseOrder::from_name(name)
                    .map(MonomialOrder::from)
                    .ok_or_else(|| E::custom(format!("unknown order {name:?}: {ORDER_FORMS}")))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<MonomialOrder, A::Error> {
                let mut weights: Option<Vec<Weight>> = None;
                let mut then: Option<BaseOrder> = None;
                let mut block: Option<Vec<OrderBlock>> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "weights" if weights.is_some() => {
                            return Err(de::Error::duplicate_field("weights"));
                        }
                        "weights" => weights = Some(map.next_value()?),
                        "then" if then.is_some() => {
                            return Err(de::Error::duplicate_field("then"));
                        }
                        "then" => then = Some(map.next_value()?),
                        "block" if block.is_some() => {
                            return Err(de::Error::duplicate_field("block"));
                        }
                        "block" => block = Some(map.next_value()?),
                        other => {
                            return Err(de::Error::custom(format!(
                                "unknown field `{other}`: {ORDER_FORMS}"
                            )));
                        }
                    }
                }
                match (weights, block) {
                    (Some(weights), None) => Ok(MonomialOrder::Weights {
                        weights: weights.into_iter().map(|w| w.0).collect(),
                        then: then.ok_or_else(|| de::Error::missing_field("then"))?,
                    }),
                    (None, Some(_)) if then.is_some() => {
                        Err(de::Error::unknown_field("then", BLOCK_FIELDS))
                    }
                    (None, Some(block)) => Ok(MonomialOrder::Block(block)),
                    (Some(_), Some(_)) => Err(de::Error::unknown_field("block", WEIGHTS_FIELDS)),
                    (None, None) => Err(de::Error::custom(ORDER_FORMS)),
                }
            }
        }
        deserializer.deserialize_any(Order)
    }
}

/// A polynomial ring: coefficient domain, ordered variables and monomial order.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RingSpec {
    pub domain: CoefficientDomain,
    pub variables: Vec<String>,
    pub order: MonomialOrder,
}

/// Variable names are restricted to `[a-z][A-Za-z0-9_]*`, which every supported CAS
/// accepts as an indeterminate without quoting.
pub(crate) fn validate_variable(name: &str) -> Result<(), MathError> {
    let mut chars = name.chars();
    let ok = matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 64;
    if ok {
        Ok(())
    } else {
        Err(MathError::InvalidVariable(name.to_owned()))
    }
}

pub(crate) fn validate_variables<'a>(
    names: impl IntoIterator<Item = &'a String>,
) -> Result<(), MathError> {
    let mut seen = HashSet::new();
    for name in names {
        validate_variable(name)?;
        if !seen.insert(name.as_str()) {
            return Err(MathError::DuplicateVariable(name.clone()));
        }
    }
    Ok(())
}

impl RingSpec {
    pub fn new(
        domain: CoefficientDomain,
        variables: Vec<String>,
        order: MonomialOrder,
    ) -> Result<Self, MathError> {
        let ring = Self {
            domain,
            variables,
            order,
        };
        ring.validate()?;
        Ok(ring)
    }

    /// Checks the variable names and the monomial order
    /// ([`MonomialOrder::validate`]).
    pub fn validate(&self) -> Result<(), MathError> {
        validate_variables(&self.variables)?;
        self.order.validate(self.nvars())
    }

    pub fn nvars(&self) -> usize {
        self.variables.len()
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.variables.iter().position(|v| v == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_form_matches_design() {
        let ring: RingSpec = serde_json::from_str(
            r#"{"domain": "QQ", "variables": ["x", "y"], "order": "grevlex"}"#,
        )
        .unwrap();
        assert_eq!(ring.domain, CoefficientDomain::Rationals);
        assert_eq!(ring.order, MonomialOrder::Grevlex);
        ring.validate().unwrap();
    }

    #[test]
    fn rejects_bad_variables() {
        let bad = |vars: &[&str]| {
            RingSpec::new(
                CoefficientDomain::Integers,
                vars.iter().map(|s| s.to_string()).collect(),
                MonomialOrder::Lex,
            )
            .is_err()
        };
        assert!(bad(&["x", "x"]));
        assert!(bad(&["X"]));
        assert!(bad(&["x y"]));
        assert!(bad(&["1x"]));
        assert!(bad(&["x)"]));
        assert!(!bad(&["x1", "y_2", "dx"]));
    }
}
