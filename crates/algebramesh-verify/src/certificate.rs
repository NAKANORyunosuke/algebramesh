//! Checks shared by the operations: each decides a property from data alone,
//! in exact arithmetic (ADR 0019, class (a)).

use algebramesh_math_types::{CoefficientDomain, Polynomial, Rational, RationalMatrix};
use num_bigint::BigInt;
use num_integer::Integer;

/// Whether the coefficients of `p` have no common divisor but units: over ZZ
/// the gcd of the coefficients (the content) is 1. The zero polynomial is not
/// primitive.
pub fn is_primitive(p: &Polynomial) -> bool {
    let mut content: Option<BigInt> = None;
    for (_, c) in p.terms() {
        if !c.is_integer() {
            return false;
        }
        let n = c.0.numer();
        content = Some(match content {
            None => n.gcd(n),
            Some(g) => g.gcd(n),
        });
    }
    content.is_some_and(|g| g == BigInt::from(1))
}

/// Whether every coefficient of `p` lies in `domain`.
pub fn coefficients_in(p: &Polynomial, domain: CoefficientDomain) -> bool {
    p.terms().all(|(_, c)| domain.contains(c))
}

/// The index of the first nonzero entry of each column, from the top; none
/// for a zero column.
fn column_tops(k: &RationalMatrix) -> Vec<Option<usize>> {
    (0..k.ncols())
        .map(|j| (0..k.nrows()).find(|&i| !k.get(i, j).is_zero()))
        .collect()
}

/// Whether `k` is in column echelon form: no zero column, and the first
/// nonzero entries of the columns go strictly down from left to right. Such
/// columns are linearly independent.
pub fn is_column_echelon(k: &RationalMatrix) -> bool {
    let tops = column_tops(k);
    tops.iter().all(Option::is_some) && tops.windows(2).all(|w| w[0] < w[1])
}

/// The number of nonzero rows of `h` if it is in row echelon form: the first
/// nonzero entries of the nonzero rows go strictly right from top to bottom,
/// and the zero rows are below them. The nonzero rows are then linearly
/// independent.
pub fn row_echelon_rank(h: &RationalMatrix) -> Option<usize> {
    let leads: Vec<Option<usize>> = h
        .rows()
        .iter()
        .map(|r| r.iter().position(|v| !v.is_zero()))
        .collect();
    let nonzero = leads.iter().take_while(|l| l.is_some()).count();
    if leads[nonzero..].iter().any(Option::is_some) {
        return None;
    }
    let leads: Vec<usize> = leads[..nonzero].iter().flatten().copied().collect();
    leads.windows(2).all(|w| w[0] < w[1]).then_some(nonzero)
}

/// Whether `h` is in row Hermite normal form: integral, in row echelon form,
/// each pivot positive and each entry above a pivot at least 0 and below the
/// pivot.
pub fn is_hermite_form(h: &RationalMatrix) -> bool {
    if !h.is_integral() {
        return false;
    }
    let Some(nonzero) = row_echelon_rank(h) else {
        return false;
    };
    (0..nonzero).all(|i| {
        let Some(c) = h.rows()[i].iter().position(|v| !v.is_zero()) else {
            return false;
        };
        let pivot = h.get(i, c);
        *pivot > Rational::zero()
            && (0..i).all(|above| {
                let v = h.get(above, c);
                *v >= Rational::zero() && v < pivot
            })
    })
}

/// Whether `d` is the diagonal of a Smith normal form: non-negative integers,
/// each dividing the next (0 divides only 0).
pub fn is_smith_diagonal(d: &[Rational]) -> bool {
    d.iter().all(|v| v.is_integer() && *v >= Rational::zero())
        && d.windows(2).all(|w| {
            let (a, b) = (w[0].0.numer(), w[1].0.numer());
            let zero = BigInt::from(0);
            if *a == zero {
                *b == zero
            } else {
                b % a == zero
            }
        })
}

/// Whether the monomial with exponents `a` divides the one with exponents
/// `b`, of the same length: each exponent of `a` is at most that of `b`.
pub fn divides(a: &[u32], b: &[u32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x <= y)
}

/// A term of an element of `basis` that the leading monomial of another
/// element divides, as `(i, j)`: a term of `basis[i]` is divisible by
/// `leading[j]`, with `i != j`; none if there is no such term. `leading[j]`
/// is the leading monomial of `basis[j]`. With monic elements, no such term
/// is the condition of a reduced Groebner basis on the terms (Cox, Little and
/// O'Shea, Ideals, Varieties, and Algorithms, Chapter 2, §7).
/// `checkpoint` is called after every 256 tests of divisibility; an error
/// from it ends the search.
pub fn reduced_form_defect<E>(
    basis: &[Polynomial],
    leading: &[&[u32]],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<(usize, usize)>, E> {
    let mut count = 0usize;
    for (i, g) in basis.iter().enumerate() {
        for (e, _) in g.terms() {
            for (j, m) in leading.iter().enumerate() {
                count += 1;
                if count.is_multiple_of(256) {
                    checkpoint()?;
                }
                if j != i && divides(m, e) {
                    return Ok(Some((i, j)));
                }
            }
        }
    }
    Ok(None)
}

/// The index of a monomial of `leading` that divides a term of `p`; none if
/// no term of `p` is divisible by any of them. With `leading` the leading
/// monomials of a Groebner basis G, no such term is the condition on a
/// remainder of the division by G that makes it the normal form (Cox, Little
/// and O'Shea, Ideals, Varieties, and Algorithms, Chapter 2, §6).
/// `checkpoint` is called after every 256 tests of divisibility; an error
/// from it ends the search.
pub fn divisible_term<E>(
    p: &Polynomial,
    leading: &[&[u32]],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<usize>, E> {
    let mut count = 0usize;
    for (e, _) in p.terms() {
        for (k, m) in leading.iter().enumerate() {
            count += 1;
            if count.is_multiple_of(256) {
                checkpoint()?;
            }
            if divides(m, e) {
                return Ok(Some(k));
            }
        }
    }
    Ok(None)
}

/// Whether two monomials of the same length are coprime: no variable has a
/// positive exponent in both.
pub fn coprime(a: &[u32], b: &[u32]) -> bool {
    a.iter().zip(b).all(|(x, y)| *x == 0 || *y == 0)
}

/// The least common multiple of two monomials of the same length: the
/// larger exponent of each variable.
pub fn lcm(a: &[u32], b: &[u32]) -> Vec<u32> {
    a.iter().zip(b).map(|(x, y)| *x.max(y)).collect()
}

/// Where a list of S-pairs differs from the pairs (i, j), i < j, whose
/// leading monomials are not coprime, in increasing lexicographic order of
/// (i, j) (Issue #68). Indices are 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairDefect {
    /// Entry `index` is not a pair (i, j) of elements with i < j < t.
    NotAPair { index: usize, pair: [usize; 2] },
    /// Entry `index` is a pair whose leading monomials are coprime.
    Coprime { index: usize, pair: [usize; 2] },
    /// Entry `index` is `given`, where `expected` comes; none when the list
    /// ends before `expected` or goes on after the last pair.
    Unexpected {
        index: usize,
        given: Option<[usize; 2]>,
        expected: Option<[usize; 2]>,
    },
}

/// Where `pairs` differs from the S-pairs of a basis with the leading
/// monomials `leading`: each (i, j), i < j, with `leading[i]` and
/// `leading[j]` not coprime, in increasing lexicographic order of (i, j).
/// Pairs with coprime leading monomials need no representation (Buchberger's
/// first criterion; Cox, Little and O'Shea, Ideals, Varieties, and
/// Algorithms, Chapter 2, §9). The pairs are enumerated, not collected, so a
/// basis of many elements costs no memory; `checkpoint` is called after
/// every 256 pairs, and an error from it ends the walk.
pub fn s_pair_defect<E>(
    leading: &[&[u32]],
    pairs: &[[usize; 2]],
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Option<PairDefect>, E> {
    let t = leading.len();
    // A wrong entry `pair` at `index`, where `expected` comes.
    let wrong = |index: usize, pair: [usize; 2], expected: Option<[usize; 2]>| {
        let [a, b] = pair;
        if a >= b || b >= t {
            PairDefect::NotAPair { index, pair }
        } else if coprime(leading[a], leading[b]) {
            PairDefect::Coprime { index, pair }
        } else {
            PairDefect::Unexpected {
                index,
                given: Some(pair),
                expected,
            }
        }
    };
    let mut index = 0;
    let mut count = 0usize;
    for i in 0..t {
        for j in i + 1..t {
            count += 1;
            if count.is_multiple_of(256) {
                checkpoint()?;
            }
            if coprime(leading[i], leading[j]) {
                continue;
            }
            match pairs.get(index) {
                Some(&pair) if pair == [i, j] => index += 1,
                Some(&pair) => return Ok(Some(wrong(index, pair, Some([i, j])))),
                None => {
                    return Ok(Some(PairDefect::Unexpected {
                        index,
                        given: None,
                        expected: Some([i, j]),
                    }));
                }
            }
        }
    }
    Ok(pairs.get(index).map(|&pair| wrong(index, pair, None)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use algebramesh_math_types::{MonomialOrder, Rational, RingSpec};

    use super::*;

    fn poly(domain: CoefficientDomain, terms: &[(&str, u32)]) -> Polynomial {
        let ring = RingSpec::new(domain, vec!["x".into()], MonomialOrder::Lex).unwrap();
        Polynomial::from_terms(
            ring,
            terms
                .iter()
                .map(|(c, e)| (vec![*e], c.parse::<Rational>().unwrap())),
        )
        .unwrap()
    }

    #[test]
    fn content_one_is_primitive() {
        use CoefficientDomain::*;
        assert!(is_primitive(&poly(Integers, &[("2", 1), ("3", 0)])));
        assert!(is_primitive(&poly(Integers, &[("-1", 1)])));
        assert!(!is_primitive(&poly(Integers, &[("2", 1), ("4", 0)])));
        assert!(!is_primitive(&poly(Integers, &[("-2", 1)])));
        assert!(!is_primitive(&poly(Integers, &[])));
        assert!(!is_primitive(&poly(Rationals, &[("1/2", 1), ("1", 0)])));
    }

    fn m(rows: &[&[i64]]) -> RationalMatrix {
        RationalMatrix::new(
            rows.iter()
                .map(|r| r.iter().map(|&v| Rational::integer(v)).collect())
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn echelon_forms() {
        assert!(is_column_echelon(&m(&[&[1, 0], &[5, 0], &[0, 2]])));
        assert!(!is_column_echelon(&m(&[&[0, 1], &[1, 0]])), "tops go up");
        assert!(
            !is_column_echelon(&m(&[&[1, 0], &[0, 0]])),
            "a zero column last"
        );
        // Only the check for zero columns refuses these: a zero column has no
        // top, which the order of the tops alone lets pass when it comes first
        // or alone.
        assert!(
            !is_column_echelon(&m(&[&[0, 1], &[0, 0]])),
            "a zero column first"
        );
        assert!(
            !is_column_echelon(&m(&[&[0], &[0]])),
            "a single zero column"
        );
        assert!(is_column_echelon(&RationalMatrix::zeros(3, 0)));

        assert_eq!(
            row_echelon_rank(&m(&[&[2, 1, 3], &[0, 0, 4], &[0, 0, 0]])),
            Some(2)
        );
        assert_eq!(
            row_echelon_rank(&m(&[&[0, 0], &[1, 0]])),
            None,
            "zero row above"
        );
        assert_eq!(row_echelon_rank(&m(&[&[0, 1], &[1, 0]])), None);
        assert_eq!(row_echelon_rank(&RationalMatrix::zeros(2, 2)), Some(0));
    }

    #[test]
    fn hermite_and_smith_forms() {
        assert!(is_hermite_form(&m(&[&[2, 1, 3], &[0, 0, 4], &[0, 0, 0]])));
        assert!(
            !is_hermite_form(&m(&[&[2, 1, 5], &[0, 0, 4]])),
            "5 >= pivot 4"
        );
        assert!(
            !is_hermite_form(&m(&[&[2, 1, -1], &[0, 0, 4]])),
            "negative above"
        );
        assert!(!is_hermite_form(&m(&[&[-2, 1], &[0, 1]])), "negative pivot");
        let r = |v: &[i64]| v.iter().map(|&x| Rational::integer(x)).collect::<Vec<_>>();
        assert!(is_smith_diagonal(&r(&[2, 4])));
        assert!(is_smith_diagonal(&r(&[1, 3, 0])));
        assert!(!is_smith_diagonal(&r(&[2, 3])));
        assert!(!is_smith_diagonal(&r(&[0, 1])));
        assert!(!is_smith_diagonal(&r(&[-2, 4])));
    }

    #[test]
    fn divisibility_of_monomials() {
        assert!(divides(&[1, 0, 2], &[1, 1, 2]));
        assert!(divides(&[0, 0, 0], &[3, 0, 1]));
        assert!(!divides(&[1, 0, 2], &[0, 1, 2]));
        assert!(!divides(&[1, 0], &[1, 0, 0]), "lengths differ");
    }

    #[test]
    fn a_term_divisible_by_another_leading_monomial_is_found() {
        let ring = RingSpec::new(
            CoefficientDomain::Rationals,
            vec!["x".into(), "y".into()],
            MonomialOrder::Grevlex,
        )
        .unwrap();
        let p = |terms: &[(i64, [u32; 2])]| {
            Polynomial::from_terms(
                ring.clone(),
                terms
                    .iter()
                    .map(|(c, e)| (e.to_vec(), Rational::integer(*c))),
            )
            .unwrap()
        };
        let none = &mut || Ok::<(), ()>(());
        // x^2 + y and y^2: interreduced.
        let basis = [p(&[(1, [2, 0]), (1, [0, 1])]), p(&[(1, [0, 2])])];
        let leading: [&[u32]; 2] = [&[2, 0], &[0, 2]];
        assert_eq!(reduced_form_defect(&basis, &leading, none), Ok(None));
        // x^2 + y^3 and y^2: y^2 divides y^3; its own leading monomial does
        // not count.
        let basis = [p(&[(1, [2, 0]), (1, [0, 3])]), p(&[(1, [0, 2])])];
        assert_eq!(
            reduced_form_defect(&basis, &leading, none),
            Ok(Some((0, 1)))
        );
        // Two elements with one leading monomial.
        let basis = [p(&[(1, [0, 2])]), p(&[(1, [0, 2]), (1, [1, 0])])];
        let leading: [&[u32]; 2] = [&[0, 2], &[0, 2]];
        assert_eq!(
            reduced_form_defect(&basis, &leading, none),
            Ok(Some((0, 1)))
        );
    }

    #[test]
    fn a_term_of_a_remainder_divisible_by_a_leading_monomial_is_found() {
        let ring = RingSpec::new(
            CoefficientDomain::Rationals,
            vec!["x".into(), "y".into()],
            MonomialOrder::Grevlex,
        )
        .unwrap();
        let p = |terms: &[[u32; 2]]| {
            Polynomial::from_terms(
                ring.clone(),
                terms.iter().map(|e| (e.to_vec(), Rational::integer(1))),
            )
            .unwrap()
        };
        let none = &mut || Ok::<(), ()>(());
        let leading: [&[u32]; 2] = [&[2, 0], &[0, 2]];
        // x y + x + 1 is reduced modulo x^2 and y^2; x y^3 is not, by y^2.
        assert_eq!(
            divisible_term(&p(&[[1, 1], [1, 0], [0, 0]]), &leading, none),
            Ok(None)
        );
        assert_eq!(
            divisible_term(&p(&[[1, 3], [0, 0]]), &leading, none),
            Ok(Some(1))
        );
        // Every term is divisible by 1; nothing by the empty basis.
        let one: [&[u32]; 1] = [&[0, 0]];
        assert_eq!(divisible_term(&p(&[[0, 0]]), &one, none), Ok(Some(0)));
        assert_eq!(divisible_term(&p(&[[3, 3]]), &[], none), Ok(None));
        // The checkpoint stops a long search.
        let many: Vec<[u32; 2]> = (0..300).map(|k| [k, 1]).collect();
        let stop = &mut || Err::<(), &str>("stop");
        assert_eq!(divisible_term(&p(&many), &[&[0, 2]], stop), Err("stop"));
    }

    #[test]
    fn coprime_monomials_and_their_lcm() {
        assert!(coprime(&[2, 0, 1], &[0, 3, 0]));
        assert!(!coprime(&[2, 0, 1], &[0, 3, 1]));
        assert!(coprime(&[0, 0], &[4, 1]), "1 is coprime to everything");
        assert_eq!(lcm(&[2, 0, 1], &[1, 3, 0]), [2, 3, 1]);
    }

    /// Issue #68: the S-pairs of z^2, yz, y^2 (the twisted cubic in grevlex,
    /// in x, y, z, w) are (0, 1) and (1, 2); z^2 and y^2 are coprime.
    #[test]
    fn the_s_pairs_are_the_pairs_whose_leading_monomials_are_not_coprime() {
        let none = &mut || Ok::<(), ()>(());
        let leading: [&[u32]; 3] = [&[0, 0, 2, 0], &[0, 1, 1, 0], &[0, 2, 0, 0]];
        let mut defect = |pairs: &[[usize; 2]]| s_pair_defect(&leading, pairs, none).unwrap();
        assert_eq!(defect(&[[0, 1], [1, 2]]), None);
        // One missing, at the end and before another.
        assert_eq!(
            defect(&[[0, 1]]),
            Some(PairDefect::Unexpected {
                index: 1,
                given: None,
                expected: Some([1, 2])
            })
        );
        assert_eq!(
            defect(&[[1, 2]]),
            Some(PairDefect::Unexpected {
                index: 0,
                given: Some([1, 2]),
                expected: Some([0, 1])
            })
        );
        // A coprime pair, one repeated, the order reversed, one too many.
        assert_eq!(
            defect(&[[0, 1], [0, 2], [1, 2]]),
            Some(PairDefect::Coprime {
                index: 1,
                pair: [0, 2]
            })
        );
        assert_eq!(
            defect(&[[0, 1], [0, 1], [1, 2]]),
            Some(PairDefect::Unexpected {
                index: 1,
                given: Some([0, 1]),
                expected: Some([1, 2])
            })
        );
        assert_eq!(
            defect(&[[1, 2], [0, 1]]),
            Some(PairDefect::Unexpected {
                index: 0,
                given: Some([1, 2]),
                expected: Some([0, 1])
            })
        );
        assert_eq!(
            defect(&[[0, 1], [1, 2], [1, 2]]),
            Some(PairDefect::Unexpected {
                index: 2,
                given: Some([1, 2]),
                expected: None
            })
        );
        // Not pairs i < j < t.
        for pair in [[1, 0], [1, 1], [1, 3]] {
            assert_eq!(
                defect(&[[0, 1], pair]),
                Some(PairDefect::NotAPair { index: 1, pair })
            );
        }
        // No pairs for no element or one.
        assert_eq!(s_pair_defect(&[], &[], none), Ok(None));
        assert_eq!(s_pair_defect(&leading[..1], &[], none), Ok(None));
        assert_eq!(
            s_pair_defect(&leading[..1], &[[0, 1]], none),
            Ok(Some(PairDefect::NotAPair {
                index: 0,
                pair: [0, 1]
            }))
        );
        // The checkpoint stops a long walk: 30 variables, pairwise coprime,
        // have 435 pairs, and none needs a representation.
        let many: Vec<Vec<u32>> = (0..30)
            .map(|i| (0..30).map(|j| u32::from(i == j)).collect())
            .collect();
        let many: Vec<&[u32]> = many.iter().map(Vec::as_slice).collect();
        assert_eq!(s_pair_defect(&many, &[], none), Ok(None));
        let stop = &mut || Err::<(), &str>("stop");
        assert_eq!(s_pair_defect(&many, &[], stop), Err("stop"));
    }

    #[test]
    fn coefficients_lie_in_the_domain() {
        use CoefficientDomain::*;
        let half = poly(Rationals, &[("1/2", 1)]);
        assert!(coefficients_in(&half, Rationals));
        assert!(!coefficients_in(&half, Integers));
    }
}
