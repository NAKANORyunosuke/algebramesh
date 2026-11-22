//! Cases for comparing `MonomialOrder::compare` with the leading terms of the
//! CAS (Issue #64).
//!
//! Writes one JSON line for each order, over rings of 0, 1, 4 and 6 variables:
//! a ring with that order, the zero polynomial followed by polynomials over
//! it, and the exponents of their leading terms by `MonomialOrder::compare`
//! (`null` for the zero polynomial):
//!
//! ```text
//! {"ring": {...}, "polynomials": [{"type": "polynomial", "terms": [...]}, ...],
//!  "leading_exponents": [null, [3, 0, 1, 2], ...]}
//! ```
//!
//! For each order, COUNT polynomials are random, with exponents below 4, and
//! COUNT are drawn where the orders differ most, so that a driver that swaps
//! grlex and grevlex, or places the weights of a later block from the first
//! variable, gives many differences:
//! - for `Lex`, `Grlex`, `Grevlex` and `Weights`, the terms share one total
//!   degree, and for `Weights` one weighted degree as well;
//! - for `Block`, the terms share the exponents of the blocks before one
//!   grlex or grevlex block, and differ inside it: half of the time with one
//!   degree in that block, otherwise with exponents below 4.
//!
//! The polynomials depend only on the seed. A monomial whose weighted degree
//! would pass `MAX_WEIGHT` is not drawn, since Macaulay2 1.24.11 stops with an
//! error on such a monomial.
//!
//! Usage: `cargo run -p algebramesh-math-types --example order_cases [SEED [COUNT]]`
//! (by default seed 64 and 50 polynomials of each kind for each order).

use std::collections::BTreeMap;

use algebramesh_math_types::{
    BaseOrder, CoefficientDomain, MAX_WEIGHT, MonomialOrder, OrderBlock, Polynomial,
    PolynomialData, Rational, RingSpec,
};
use num_bigint::BigInt;
use serde::Serialize;

/// The monomials drawn for one polynomial; its terms are the first distinct
/// ones, so that a ring with few monomials (none or one variable) gives fewer
/// terms instead of drawing forever.
const DRAWS: usize = 64;

/// SplitMix64: a small generator whose output depends only on the seed.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number in `0..n`.
    fn below(&mut self, n: u32) -> u32 {
        u32::try_from(self.next() % u64::from(n)).unwrap_or(0)
    }

    /// An index in `0..n`, for `n > 0`.
    fn index(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap_or(1)).unwrap_or(0)
    }

    /// `k` exponents below 4.
    fn exponents(&mut self, k: usize) -> Vec<u32> {
        (0..k).map(|_| self.below(4)).collect()
    }

    /// `k` exponents of total degree `d`, each unit given to a random
    /// variable (none if `k` is 0).
    fn of_degree(&mut self, k: usize, d: u32) -> Vec<u32> {
        let mut e = vec![0; k];
        if k > 0 {
            for _ in 0..d {
                e[self.index(k)] += 1;
            }
        }
        e
    }
}

fn weights(weights: &[u32], then: BaseOrder) -> MonomialOrder {
    MonomialOrder::Weights {
        weights: weights.to_vec(),
        then,
    }
}

fn block(parts: &[(usize, BaseOrder)]) -> MonomialOrder {
    MonomialOrder::Block(
        parts
            .iter()
            .map(|&(count, order)| OrderBlock { count, order })
            .collect(),
    )
}

/// The number of variables of each ring and its orders.
fn rings() -> Vec<(usize, Vec<MonomialOrder>)> {
    use BaseOrder::{Grevlex, Grlex, Lex};
    vec![
        // The drivers keep a variable of their own (Issue #35).
        (0, vec![MonomialOrder::Lex, weights(&[], Grevlex)]),
        (
            1,
            vec![
                MonomialOrder::Grevlex,
                weights(&[2], Lex),
                block(&[(1, Grlex)]),
            ],
        ),
        (
            4,
            vec![
                MonomialOrder::Lex,
                MonomialOrder::Grlex,
                MonomialOrder::Grevlex,
                weights(&[3, 0, 1, 2], Grevlex),
                weights(&[0, 0, 0, 0], Lex),
                weights(&[1, 1, 1, 1], Lex),
                weights(&[0, 1, 0, 1], Grlex),
                weights(&[MAX_WEIGHT, 0, 1, 0], Grlex),
                block(&[(2, Grevlex), (2, Lex)]),
                block(&[(1, Lex), (3, Grlex)]),
                block(&[(2, Grlex), (2, Grlex)]),
                block(&[(3, Grevlex), (1, Grevlex)]),
                block(&[(1, Grevlex), (2, Grlex), (1, Lex)]),
            ],
        ),
        (
            6,
            vec![
                MonomialOrder::Grlex,
                MonomialOrder::Grevlex,
                weights(&[1, 0, 2, 0, 1, 1], Grlex),
                block(&[(2, Grevlex), (3, Grlex), (1, Lex)]),
                block(&[(1, Lex), (2, Grlex), (3, Grlex)]),
                block(&[(3, Grlex), (3, Grevlex)]),
            ],
        ),
    ]
}

/// The weighted degree of a monomial for a `Weights` order, its degree for
/// the others.
fn weighted_degree(order: &MonomialOrder, exponents: &[u32]) -> u128 {
    match order {
        MonomialOrder::Weights { weights, .. } => weights
            .iter()
            .zip(exponents)
            .map(|(&w, &e)| u128::from(w) * u128::from(e))
            .sum(),
        _ => exponents.iter().map(|&e| u128::from(e)).sum(),
    }
}

/// Whether the CAS can take the monomial (see the module documentation).
fn drawable(order: &MonomialOrder, exponents: &[u32]) -> bool {
    weighted_degree(order, exponents) <= u128::from(MAX_WEIGHT)
}

/// A coefficient: an integer from -9 to 9 or such an integer over 2 to 5,
/// never zero.
fn coefficient(random: &mut Random) -> Result<Rational, String> {
    let p = i64::from(random.below(18)) - 9;
    let p = if p >= 0 { p + 1 } else { p };
    let q = if random.below(3) == 0 {
        random.below(4) + 2
    } else {
        1
    };
    Rational::new(BigInt::from(p), BigInt::from(q)).map_err(|e| e.to_string())
}

/// The polynomial of the first `wanted` distinct monomials among `drawn`
/// (fewer if there are not as many), each with a random coefficient.
fn polynomial(
    random: &mut Random,
    ring: &RingSpec,
    wanted: usize,
    drawn: Vec<Vec<u32>>,
) -> Result<Polynomial, String> {
    let mut terms = BTreeMap::new();
    for exponents in drawn {
        if terms.len() == wanted {
            break;
        }
        if drawable(&ring.order, &exponents) && !terms.contains_key(&exponents) {
            terms.insert(exponents, coefficient(random)?);
        }
    }
    Polynomial::from_terms(ring.clone(), terms).map_err(|e| e.to_string())
}

/// One to six terms with exponents below 4.
fn random_polynomial(random: &mut Random, ring: &RingSpec) -> Result<Polynomial, String> {
    let wanted = usize::try_from(random.below(6) + 1).unwrap_or(1);
    let drawn = (0..DRAWS).map(|_| random.exponents(ring.nvars())).collect();
    polynomial(random, ring, wanted, drawn)
}

/// Two to six terms of one total degree from 2 to 7, where grlex and grevlex
/// differ; for `Weights`, of one weighted degree as well: the largest group of
/// such monomials among the drawn ones.
fn level_polynomial(random: &mut Random, ring: &RingSpec) -> Result<Polynomial, String> {
    let wanted = usize::try_from(random.below(5) + 2).unwrap_or(2);
    let d = random.below(6) + 2;
    let mut groups: BTreeMap<u128, Vec<Vec<u32>>> = BTreeMap::new();
    for _ in 0..DRAWS {
        let e = random.of_degree(ring.nvars(), d);
        if drawable(&ring.order, &e) {
            groups
                .entry(weighted_degree(&ring.order, &e))
                .or_default()
                .push(e);
        }
    }
    let group = groups
        .into_values()
        .max_by_key(Vec::len)
        .unwrap_or_default();
    polynomial(random, ring, wanted, group)
}

/// Two to six terms that share the exponents of the blocks before one block
/// (a grlex or grevlex one if there is one) and differ inside it, with one
/// degree from 2 to 7 in that block half of the time; otherwise the last
/// variable of the block adds 0 to 3 to that degree, so that the degree in
/// the block and the exponents of its first variables disagree often. The
/// exponents after that block are below 4.
fn block_polynomial(
    random: &mut Random,
    ring: &RingSpec,
    blocks: &[OrderBlock],
) -> Result<Polynomial, String> {
    let wanted = usize::try_from(random.below(5) + 2).unwrap_or(2);
    let graded: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.order != BaseOrder::Lex)
        .map(|(i, _)| i)
        .collect();
    let chosen = if graded.is_empty() {
        random.index(blocks.len())
    } else {
        graded[random.index(graded.len())]
    };
    let before: usize = blocks[..chosen].iter().map(|b| b.count).sum();
    let size = blocks[chosen].count;
    let after = ring.nvars() - before - size;
    let prefix = random.exponents(before);
    let d = random.below(6) + 2;
    let level = random.below(2) == 0;
    let drawn = (0..DRAWS)
        .map(|_| {
            let mut inside = random.of_degree(size, d);
            let extra = if level { 0 } else { random.below(4) };
            if let Some(last) = inside.last_mut() {
                *last += extra;
            }
            let mut e = prefix.clone();
            e.extend(inside);
            e.extend(random.exponents(after));
            e
        })
        .collect();
    polynomial(random, ring, wanted, drawn)
}

#[derive(Serialize)]
struct Case {
    ring: RingSpec,
    polynomials: Vec<PolynomialData>,
    leading_exponents: Vec<Option<Vec<u32>>>,
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut number = |default: u64| -> Result<u64, String> {
        args.next().map_or(Ok(default), |a| {
            a.parse().map_err(|e| format!("{a:?}: {e}"))
        })
    };
    let seed = number(64)?;
    let count = number(50)?;
    let mut random = Random(seed);
    for (nvars, orders) in rings() {
        let variables: Vec<String> = (1..=nvars).map(|i| format!("x{i}")).collect();
        for order in orders {
            let ring = RingSpec::new(CoefficientDomain::Rationals, variables.clone(), order)
                .map_err(|e| e.to_string())?;
            let mut polynomials = vec![Polynomial::zero(ring.clone())];
            for _ in 0..count {
                polynomials.push(random_polynomial(&mut random, &ring)?);
            }
            for _ in 0..count {
                polynomials.push(match &ring.order {
                    MonomialOrder::Block(blocks) => block_polynomial(&mut random, &ring, blocks)?,
                    _ => level_polynomial(&mut random, &ring)?,
                });
            }
            let leading_exponents = polynomials
                .iter()
                .map(|p| {
                    p.terms()
                        .map(|(e, _)| e)
                        .max_by(|a, b| ring.order.compare(a, b))
                        .map(<[u32]>::to_vec)
                })
                .collect();
            let case = Case {
                polynomials: polynomials.iter().map(Polynomial::to_data).collect(),
                ring,
                leading_exponents,
            };
            println!(
                "{}",
                serde_json::to_string(&case).map_err(|e| e.to_string())?
            );
        }
    }
    Ok(())
}
