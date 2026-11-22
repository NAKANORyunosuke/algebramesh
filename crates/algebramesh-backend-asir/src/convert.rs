//! Conversion between typed polynomials and Asir's CMO representation.
//!
//! Asir sends a polynomial as `CMO_RECURSIVE_POLYNOMIAL(variables, body)` where the body
//! is nested `CMO_UNIVARIATE_POLYNOMIAL` nodes with `CMO_ZZ`/`CMO_QQ` leaves, and sends
//! zero as `CMO_NULL` (`write_cmo` in asir2018/io/cio.c).

use std::collections::{BTreeMap, HashMap};

use algebramesh_math_types::{MathError, Polynomial, Rational, RingSpec};
use algebramesh_openxm_protocol::Cmo;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConvertError {
    #[error("unexpected CMO tag {tag} where {expected} was expected")]
    Unexpected { expected: &'static str, tag: u32 },
    #[error("variable {0:?} returned by the server is not in the ring")]
    UnknownVariable(String),
    #[error("variable {0:?} appears twice in the variable list")]
    DuplicateVariable(String),
    #[error("exponent overflow")]
    Overflow,
    #[error(transparent)]
    Math(#[from] MathError),
}

pub fn rational_to_cmo(value: &Rational) -> Cmo {
    if value.is_zero() {
        Cmo::Zero
    } else if value.is_integer() {
        Cmo::Zz(value.numer().clone())
    } else {
        Cmo::Qq {
            num: value.numer().clone(),
            den: value.denom().clone(),
        }
    }
}

pub fn cmo_to_rational(cmo: &Cmo) -> Result<Rational, ConvertError> {
    match cmo {
        Cmo::Null | Cmo::Zero => Ok(Rational::zero()),
        Cmo::Int32(v) => Ok(Rational::integer(*v)),
        Cmo::Zz(v) => Ok(Rational::integer(v.clone())),
        Cmo::Qq { num, den } => Ok(Rational::new(num.clone(), den.clone())?),
        other => Err(ConvertError::Unexpected {
            expected: "a rational number",
            tag: other.tag(),
        }),
    }
}

/// The most variables that may occur in a polynomial sent to Asir (Issue #32).
/// Each one that occurs is one level of nested univariate polynomials, and
/// nested CMOs are encoded, decoded and dropped recursively; the bound keeps
/// that depth far below what a thread's stack holds, and below the decoders'
/// nesting limit (512). Variables of the ring that do not occur cost nothing.
pub const MAX_USED_VARIABLES: usize = 256;

/// The number of variables that occur in `p` (with a positive exponent in some
/// term).
pub fn used_variables(p: &Polynomial) -> usize {
    (0..p.ring().nvars())
        .filter(|&i| p.terms().any(|(e, _)| e[i] > 0))
        .count()
}

/// Encodes `p` with the ring variables as the variable list; variable `0` is the
/// outermost level. Levels whose variable does not occur are omitted, so every
/// univariate node has a term of positive degree, as Asir's own encoder produces.
///
/// Levels are skipped in a loop, so a ring with many variables that do not
/// occur needs no recursion for them; the recursion follows the variables that
/// occur, at most [`MAX_USED_VARIABLES`] for a polynomial the backend accepts.
pub fn polynomial_to_cmo(p: &Polynomial) -> Cmo {
    let variables = p
        .ring()
        .variables
        .iter()
        .map(|v| Cmo::Indeterminate(Box::new(Cmo::string(v.clone()))))
        .collect();
    let terms: Vec<(&[u32], &Rational)> = p.terms().collect();
    let body = if terms.is_empty() {
        Cmo::Zero
    } else {
        encode_level(&terms, 0, p.ring().nvars())
    };
    Cmo::RecursivePolynomial {
        variables,
        body: Box::new(body),
    }
}

fn encode_level(terms: &[(&[u32], &Rational)], level: usize, nvars: usize) -> Cmo {
    // The next level whose variable occurs in these terms (Issue #32).
    let mut level = level;
    while level < nvars && terms.iter().all(|(e, _)| e[level] == 0) {
        level += 1;
    }
    if level == nvars {
        // Exponent vectors are unique, so exactly one term remains here.
        return terms
            .first()
            .map(|(_, c)| rational_to_cmo(c))
            .unwrap_or(Cmo::Zero);
    }
    let mut groups: BTreeMap<u32, Vec<(&[u32], &Rational)>> = BTreeMap::new();
    for term in terms {
        groups.entry(term.0[level]).or_default().push(*term);
    }
    Cmo::UnivariatePolynomial {
        variable: level as u32,
        terms: groups
            .into_iter()
            .rev()
            .map(|(e, sub)| (e, encode_level(&sub, level + 1, nvars)))
            .collect(),
    }
}

fn variable_name(cmo: &Cmo) -> Result<&str, ConvertError> {
    match cmo {
        Cmo::Indeterminate(inner) => variable_name(inner),
        Cmo::String(s) => Ok(s),
        other => Err(ConvertError::Unexpected {
            expected: "CMO_INDETERMINATE",
            tag: other.tag(),
        }),
    }
}

/// Decodes a number or recursive polynomial into `ring`. Variables are matched by
/// name; a variable outside the ring or a coefficient outside its domain is an error.
pub fn cmo_to_polynomial(cmo: &Cmo, ring: &RingSpec) -> Result<Polynomial, ConvertError> {
    let n = ring.nvars();
    let mut terms: BTreeMap<Vec<u32>, Rational> = BTreeMap::new();
    match cmo {
        Cmo::RecursivePolynomial { variables, body } => {
            // Positions by name, once: looking each name up in the ring's list
            // would take time quadratic in the number of variables.
            let positions: HashMap<&str, usize> = ring
                .variables
                .iter()
                .enumerate()
                .map(|(i, v)| (v.as_str(), i))
                .collect();
            let mut index = Vec::with_capacity(variables.len());
            let mut seen = HashMap::new();
            for v in variables {
                let name = variable_name(v)?;
                let i = *positions
                    .get(name)
                    .ok_or_else(|| ConvertError::UnknownVariable(name.to_owned()))?;
                if seen.insert(name.to_owned(), i).is_some() {
                    return Err(ConvertError::DuplicateVariable(name.to_owned()));
                }
                index.push(i);
            }
            collect_terms(body, &index, &mut vec![0; n], &mut terms)?;
        }
        number => {
            let c = cmo_to_rational(number)?;
            terms.insert(vec![0; n], c);
        }
    }
    Ok(Polynomial::from_terms(ring.clone(), terms)?)
}

fn collect_terms(
    node: &Cmo,
    index: &[usize],
    exponents: &mut Vec<u32>,
    out: &mut BTreeMap<Vec<u32>, Rational>,
) -> Result<(), ConvertError> {
    match node {
        Cmo::UnivariatePolynomial { variable, terms } => {
            // The protocol decoder has already checked the index range.
            let slot = index[*variable as usize];
            for (e, coefficient) in terms {
                let saved = exponents[slot];
                exponents[slot] = saved.checked_add(*e).ok_or(ConvertError::Overflow)?;
                collect_terms(coefficient, index, exponents, out)?;
                exponents[slot] = saved;
            }
            Ok(())
        }
        number => {
            let c = cmo_to_rational(number)?;
            let entry = out.entry(exponents.clone()).or_insert_with(Rational::zero);
            entry.0 += c.0;
            Ok(())
        }
    }
}

/// Reads an exponent or multiplicity sent as `CMO_ZZ` or `CMO_INT32`.
pub fn cmo_to_u32(cmo: &Cmo) -> Result<u32, ConvertError> {
    let unexpected = || ConvertError::Unexpected {
        expected: "a non-negative machine integer",
        tag: cmo.tag(),
    };
    match cmo {
        Cmo::Int32(v) => u32::try_from(*v).map_err(|_| unexpected()),
        Cmo::Zz(v) => u32::try_from(v).map_err(|_| unexpected()),
        _ => Err(unexpected()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring_with(n: usize) -> RingSpec {
        RingSpec::new(
            algebramesh_math_types::CoefficientDomain::Rationals,
            (0..n).map(|i| format!("x{i}")).collect(),
            algebramesh_math_types::MonomialOrder::Grevlex,
        )
        .unwrap()
    }

    /// Issue #32: a constant in a ring of 50,000 variables is encoded without
    /// recursing once per variable (which overflowed the stack), and decodes
    /// back.
    #[test]
    fn many_unused_variables_do_not_recurse() {
        let ring = ring_with(50_000);
        let one = Polynomial::constant(ring.clone(), Rational::one()).unwrap();
        let cmo = polynomial_to_cmo(&one);
        let Cmo::RecursivePolynomial { body, .. } = &cmo else {
            panic!("{cmo:?}");
        };
        assert_eq!(**body, Cmo::Zz(1.into()));
        assert_eq!(cmo_to_polynomial(&cmo, &ring).unwrap(), one);
        // One variable that occurs, far down the list.
        let mut e = vec![0u32; 50_000];
        e[49_999] = 3;
        let p = Polynomial::from_terms(ring.clone(), [(e, Rational::one())]).unwrap();
        assert_eq!(used_variables(&p), 1);
        assert_eq!(cmo_to_polynomial(&polynomial_to_cmo(&p), &ring).unwrap(), p);
    }

    #[test]
    fn used_variables_are_counted() {
        let ring = ring_with(300);
        let all: Vec<u32> = vec![1; 300];
        let p = Polynomial::from_terms(ring.clone(), [(all, Rational::one())]).unwrap();
        assert_eq!(used_variables(&p), 300);
        assert_eq!(used_variables(&Polynomial::zero(ring)), 0);
    }
    use algebramesh_math_types::{CoefficientDomain, MonomialOrder};
    use algebramesh_openxm_protocol::{DecodeLimits, WireOrder, decode_cmo, encode_cmo};
    use num_bigint::BigInt;

    fn ring(domain: CoefficientDomain, vars: &[&str]) -> RingSpec {
        RingSpec::new(
            domain,
            vars.iter().map(|s| s.to_string()).collect(),
            MonomialOrder::Lex,
        )
        .unwrap()
    }

    fn poly(r: &RingSpec, terms: &[(&str, &[u32])]) -> Polynomial {
        Polynomial::from_terms(
            r.clone(),
            terms.iter().map(|(c, e)| (e.to_vec(), c.parse().unwrap())),
        )
        .unwrap()
    }

    #[test]
    fn roundtrip_through_cmo_and_wire() {
        let r = ring(CoefficientDomain::Rationals, &["x", "y", "z"]);
        let cases = [
            poly(&r, &[("1", &[4, 0, 0]), ("-1", &[0, 4, 0])]),
            poly(
                &r,
                &[("1/2", &[2, 0, 1]), ("-3", &[0, 0, 0]), ("7", &[0, 1, 1])],
            ),
            poly(&r, &[("5", &[0, 0, 0])]),
            Polynomial::zero(r.clone()),
            poly(&r, &[("-123456789012345678901234567890", &[0, 0, 9])]),
        ];
        for p in cases {
            let cmo = polynomial_to_cmo(&p);
            let mut bytes = Vec::new();
            encode_cmo(&cmo, WireOrder::Big, &mut bytes).unwrap();
            let (decoded, _) = decode_cmo(&bytes, WireOrder::Big, DecodeLimits::default()).unwrap();
            assert_eq!(cmo_to_polynomial(&decoded, &r).unwrap(), p, "{p}");
        }
    }

    #[test]
    fn skips_absent_variables() {
        let r = ring(CoefficientDomain::Integers, &["x", "y"]);
        let p = poly(&r, &[("1", &[0, 2]), ("1", &[0, 0])]);
        let Cmo::RecursivePolynomial { body, .. } = polynomial_to_cmo(&p) else {
            panic!()
        };
        assert!(matches!(
            *body,
            Cmo::UnivariatePolynomial { variable: 1, .. }
        ));
    }

    #[test]
    fn server_variable_order_may_differ() {
        // y*x^2 as Asir might send it with variables [y, x].
        let r = ring(CoefficientDomain::Integers, &["x", "y"]);
        let cmo = Cmo::RecursivePolynomial {
            variables: vec![
                Cmo::Indeterminate(Box::new(Cmo::string("y"))),
                Cmo::Indeterminate(Box::new(Cmo::string("x"))),
            ],
            body: Box::new(Cmo::UnivariatePolynomial {
                variable: 0,
                terms: vec![(
                    1,
                    Cmo::UnivariatePolynomial {
                        variable: 1,
                        terms: vec![(2, Cmo::Zz(BigInt::from(1)))],
                    },
                )],
            }),
        };
        assert_eq!(
            cmo_to_polynomial(&cmo, &r).unwrap(),
            poly(&r, &[("1", &[2, 1])])
        );
    }

    #[test]
    fn rejects_foreign_variables_and_domains() {
        let r = ring(CoefficientDomain::Integers, &["x"]);
        let other = ring(CoefficientDomain::Rationals, &["t"]);
        let cmo = polynomial_to_cmo(&poly(&other, &[("1", &[1])]));
        assert_eq!(
            cmo_to_polynomial(&cmo, &r),
            Err(ConvertError::UnknownVariable("t".into()))
        );
        let half = Cmo::Qq {
            num: BigInt::from(1),
            den: BigInt::from(2),
        };
        assert!(matches!(
            cmo_to_polynomial(&half, &r),
            Err(ConvertError::Math(MathError::CoefficientNotInDomain { .. }))
        ));
        assert!(matches!(
            cmo_to_polynomial(&Cmo::string("x"), &r),
            Err(ConvertError::Unexpected { .. })
        ));
    }
}
