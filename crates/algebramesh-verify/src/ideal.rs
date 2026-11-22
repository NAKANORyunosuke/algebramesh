//! `ideal.groebner_basis` (Issue #66), `ideal.normal_form` and
//! `ideal.membership` (Issue #67): the values are checked against the
//! generators F, the polynomial f and the monomial order of the job's
//! specification, with products of polynomials and comparisons and
//! divisibility of monomials only (ADR 0019, class (a)).
//!
//! | claim | statement | level |
//! |---|---|---|
//! | `gb.reduced_form` | no element of G is 0; each is monic, its leading monomials strictly increase, and no term of one is divisible by the leading monomial of another | checked |
//! | `gb.contained` | (G) ⊆ (F): G = F C | checked |
//! | `gb.contains` | (F) ⊆ (G): F = G N | checked |
//! | `gb.groebner` | G is a Groebner basis of (G) in the order of the job | checked with `s_pairs` (#68), asserted without |
//! | `normal_form.reduced` | f - r = sum_k q_k g_k, and no term of r is divisible by any lm(g_k) | checked |
//! | `normal_form.unique` | r is the normal form of f modulo (F) | checked by a theorem; effective level that of its premises |
//! | `membership.in` | f ∈ (F): f = sum_i c_i f_i | checked |
//! | `membership.not_in` | f ∉ (F): r != 0 | checked by a theorem; effective level that of its premises |
//!
//! The claims a certificate supports are asserted when the job asked for no
//! certificate or the certificate was left out for its size; `gb.groebner`
//! also when the job asked for the certificate `representation`, which has
//! no S-pairs. Whatever the certificate, a value whose data is wrong is
//! refused: a basis not in reduced form, a term of the remainder divisible
//! by a leading monomial of the basis, `member: false` with the remainder 0.

use std::cmp::Ordering;

use algebramesh_core::guarantee::{Claim, Method};
use algebramesh_core::{CasError, JobSpec};
use algebramesh_math_types::{
    MathError, Polynomial, PolynomialData, PolynomialMatrix, PolynomialMatrixData, Rational,
    RingSpec,
};
use algebramesh_ops::certificate::Certificate;
use algebramesh_ops::ideal::{
    GroebnerBasisResult, IDEAL_GROEBNER_BASIS, IDEAL_MEMBERSHIP, IDEAL_NORMAL_FORM,
    IdealCertificateRequest, IdealRequest, MembershipCertificate, MembershipResult,
    NormalFormResult, S_PAIRS_UNREQUESTED, SPairsCertificate,
};
use serde::de::DeserializeOwned;

use crate::certificate::PairDefect;
use crate::{Context, OperationVerifier, certificate, missing};

/// Why `gb.groebner` is asserted when the job asked for no S-pairs.
const NO_S_PAIRS: &str = "the job asked for no certificate of the S-pairs";

/// Why a value is refused that has no S-pairs though the job asked for
/// them.
const S_PAIRS_MISSING: &str =
    "the job asked for the certificate of the S-pairs and the value has none";

/// The premises of `normal_form.unique`: r is the normal form of f modulo
/// (F) when f - r lies in (G) with r reduced modulo lm(G), (G) = (F), and G
/// is a Groebner basis of (G).
const UNIQUE_PREMISES: [&str; 4] = [
    "normal_form.reduced",
    "gb.contained",
    "gb.contains",
    "gb.groebner",
];

/// The premises of `membership.not_in`: f ∉ (G) ⊇ (F) when f - r lies in
/// (G), r != 0 is reduced modulo lm(G), and G is a Groebner basis of (G).
/// (G) ⊆ (F) is not needed.
const NOT_IN_PREMISES: [&str; 3] = ["normal_form.reduced", "gb.contains", "gb.groebner"];

fn refuse(operation: &str, message: impl std::fmt::Display) -> CasError {
    CasError::UnsupportedResult(format!("{operation}: {message}"))
}

/// A product stopped at a checkpoint, or failed on the data.
enum Stop {
    Interrupted(CasError),
    Math(MathError),
}

impl From<MathError> for Stop {
    fn from(e: MathError) -> Self {
        Self::Math(e)
    }
}

impl Stop {
    fn into_error(self, operation: &str) -> CasError {
        match self {
            Self::Interrupted(e) => e,
            Self::Math(e) => refuse(operation, e),
        }
    }
}

/// The job of `operation`: F, f and the order come from the specification,
/// under the rules the backends apply before any CAS runs.
fn request(operation: &str, spec: &JobSpec) -> Result<IdealRequest, CasError> {
    IdealRequest::parse(operation, &spec.arguments)
        .map_err(|e| refuse(operation, format!("the arguments of the job: {e}")))
}

/// The polynomial f of a job of `ideal.normal_form` or `ideal.membership`.
fn polynomial<'a>(operation: &str, request: &'a IdealRequest) -> Result<&'a Polynomial, CasError> {
    request
        .polynomial
        .as_ref()
        .ok_or_else(|| refuse(operation, "the job has no polynomial"))
}

fn read_value<T: DeserializeOwned>(
    operation: &str,
    value: &serde_json::Value,
) -> Result<T, CasError> {
    serde_json::from_value(value.clone())
        .map_err(|e| refuse(operation, format!("the value cannot be read: {e}")))
}

/// `data` read in `ring`; `what` names it in a refusal.
fn read_polynomial(
    operation: &str,
    ring: &RingSpec,
    what: &str,
    data: &PolynomialData,
) -> Result<Polynomial, CasError> {
    Polynomial::from_data(ring.clone(), data).map_err(|e| refuse(operation, format!("{what}: {e}")))
}

/// Each polynomial of `list` read in `ring`; `name_j` names the j-th in a
/// refusal.
fn read_polynomials(
    operation: &str,
    ring: &RingSpec,
    name: &str,
    list: &[PolynomialData],
) -> Result<Vec<Polynomial>, CasError> {
    list.iter()
        .enumerate()
        .map(|(j, p)| read_polynomial(operation, ring, &format!("{name}_{j}"), p))
        .collect()
}

/// `gb.reduced_form`: reads the basis G in `ring` and checks that no element
/// is zero, each is monic, the leading monomials strictly increase, and no
/// term of one element is divisible by the leading monomial of another.
fn check_reduced_basis(
    operation: &str,
    ring: &RingSpec,
    basis: &[PolynomialData],
    context: &Context<'_>,
) -> Result<Vec<Polynomial>, CasError> {
    let g = read_polynomials(operation, ring, "the basis element g", basis)?;
    if let Some(j) = g.iter().position(Polynomial::is_zero) {
        return Err(refuse(
            operation,
            format!("the basis element g_{j} is zero"),
        ));
    }
    let leading = leading_monomials(&g);
    for (j, p) in g.iter().enumerate() {
        if let Some(coefficient) = p.leading_coefficient()
            && *coefficient != Rational::one()
        {
            return Err(refuse(
                operation,
                format!("g_{j} is not monic: its leading coefficient is {coefficient}"),
            ));
        }
    }
    for (j, pair) in leading.windows(2).enumerate() {
        if ring.order.compare(pair[0], pair[1]) != Ordering::Less {
            return Err(refuse(
                operation,
                format!(
                    "the leading monomials of g_{j} and g_{} are not in strictly increasing order",
                    j + 1
                ),
            ));
        }
    }
    let defect =
        certificate::reduced_form_defect(&g, &leading, &mut || context.interrupt.checkpoint())?;
    if let Some((i, j)) = defect {
        return Err(refuse(
            operation,
            format!("a term of g_{i} is divisible by the leading monomial of g_{j}"),
        ));
    }
    Ok(g)
}

/// The leading monomials of the nonzero elements of `g`.
fn leading_monomials(g: &[Polynomial]) -> Vec<&[u32]> {
    g.iter().filter_map(Polynomial::leading_monomial).collect()
}

/// The part of `normal_form.reduced` that needs no certificate: no term of
/// the remainder r is divisible by the leading monomial of an element of G.
fn check_reduced_remainder(
    operation: &str,
    r: &Polynomial,
    g: &[Polynomial],
    context: &Context<'_>,
) -> Result<(), CasError> {
    let leading = leading_monomials(g);
    let found = certificate::divisible_term(r, &leading, &mut || context.interrupt.checkpoint())?;
    match found {
        Some(k) => Err(refuse(
            operation,
            format!("a term of the remainder is divisible by the leading monomial of g_{k}"),
        )),
        None => Ok(()),
    }
}

/// Checks that `row` times `m` is `expected`, column by column: for each
/// column j, `sum_i row_i m_ij = expected_j`. `m` must have one row for each
/// entry of `row` and one column for each entry of `expected`. The product
/// stops at the checkpoints of the job; so does the comparison, once per
/// column. `what(j)` names the equation of column j in a refusal. The row
/// is taken in the ring of `m`.
fn check_row_times(
    operation: &str,
    name: &str,
    row: &[Polynomial],
    m: &PolynomialMatrix,
    expected: &[Polynomial],
    what: impl Fn(usize) -> String,
    context: &Context<'_>,
) -> Result<(), CasError> {
    if m.nrows() != row.len() || m.ncols() != expected.len() {
        return Err(refuse(
            operation,
            format!(
                "{name} is {} x {}, not {} x {}",
                m.nrows(),
                m.ncols(),
                row.len(),
                expected.len()
            ),
        ));
    }
    let row =
        PolynomialMatrix::from_row(m.ring().clone(), row).map_err(|e| refuse(operation, e))?;
    let product = row
        .mul_with(m, &mut || {
            context.interrupt.checkpoint().map_err(Stop::Interrupted)
        })
        .map_err(|e| e.into_error(operation))?;
    for (j, e) in expected.iter().enumerate() {
        context.interrupt.checkpoint()?;
        let got = product.get(0, j);
        let equal = match got {
            Some(p) => p == e,
            None => e.is_zero(),
        };
        if !equal {
            return Err(refuse(operation, what(j)));
        }
    }
    Ok(())
}

/// `gb.contained`: G = F C.
fn check_basis_in_generators(
    operation: &str,
    ring: &RingSpec,
    f: &[Polynomial],
    g: &[Polynomial],
    c: &PolynomialMatrixData,
    context: &Context<'_>,
) -> Result<(), CasError> {
    let c = PolynomialMatrix::from_data(ring.clone(), c)
        .map_err(|e| refuse(operation, format!("C: {e}")))?;
    check_row_times(
        operation,
        "C",
        f,
        &c,
        g,
        |j| format!("g_{j} is not sum_i f_i C_i{j}"),
        context,
    )
}

/// `gb.contains`: F = G N.
fn check_generators_in_basis(
    operation: &str,
    ring: &RingSpec,
    f: &[Polynomial],
    g: &[Polynomial],
    n: &PolynomialMatrixData,
    context: &Context<'_>,
) -> Result<(), CasError> {
    let n = PolynomialMatrix::from_data(ring.clone(), n)
        .map_err(|e| refuse(operation, format!("N: {e}")))?;
    check_row_times(
        operation,
        "N",
        g,
        &n,
        f,
        |i| format!("f_{i} is not sum_k g_k N_k{i}"),
        context,
    )
}

/// The part of `normal_form.reduced` that the quotients certify:
/// `f - r = sum_k q_k g_k`, with one quotient for each element of G.
fn check_quotients(
    operation: &str,
    ring: &RingSpec,
    f: &Polynomial,
    r: &Polynomial,
    g: &[Polynomial],
    quotients: &[PolynomialData],
    context: &Context<'_>,
) -> Result<(), CasError> {
    if quotients.len() != g.len() {
        return Err(refuse(
            operation,
            format!("{} quotients, not {}", quotients.len(), g.len()),
        ));
    }
    let q = read_polynomials(operation, ring, "the quotient q", quotients)?;
    let q = PolynomialMatrix::from_column(ring.clone(), &q).map_err(|e| refuse(operation, e))?;
    let difference = f.sub(r).map_err(|e| refuse(operation, e))?;
    check_row_times(
        operation,
        "the column of the quotients",
        g,
        &q,
        &[difference],
        |_| "f - r is not sum_k q_k g_k".into(),
        context,
    )
}

/// `membership.in`: `f = sum_i c_i f_i`, with one coefficient for each
/// generator.
fn check_coefficients(
    operation: &str,
    ring: &RingSpec,
    generators: &[Polynomial],
    f: &Polynomial,
    coefficients: &[PolynomialData],
    context: &Context<'_>,
) -> Result<(), CasError> {
    if coefficients.len() != generators.len() {
        return Err(refuse(
            operation,
            format!(
                "{} coefficients, not {}",
                coefficients.len(),
                generators.len()
            ),
        ));
    }
    let c = read_polynomials(operation, ring, "the coefficient c", coefficients)?;
    let c = PolynomialMatrix::from_column(ring.clone(), &c).map_err(|e| refuse(operation, e))?;
    check_row_times(
        operation,
        "the column of the coefficients",
        generators,
        &c,
        std::slice::from_ref(f),
        |_| "f is not sum_i c_i f_i".into(),
        context,
    )
}

/// Why a list of S-pairs is refused.
fn pair_defect_message(defect: PairDefect, t: usize) -> String {
    match defect {
        PairDefect::NotAPair {
            index,
            pair: [a, b],
        } => format!(
            "s_pairs.pairs[{index}] = ({a}, {b}) is not a pair (i, j) of basis elements with \
             i < j < {t}"
        ),
        PairDefect::Coprime {
            index,
            pair: [a, b],
        } => format!(
            "s_pairs.pairs[{index}] = ({a}, {b}): the leading monomials of g_{a} and g_{b} are \
             coprime, and such pairs are not listed"
        ),
        PairDefect::Unexpected {
            index,
            given: Some([a, b]),
            expected: Some([i, j]),
        } => format!(
            "s_pairs.pairs[{index}] is ({a}, {b}), not ({i}, {j}): the pairs are those whose \
             leading monomials are not coprime, in increasing order"
        ),
        PairDefect::Unexpected {
            given: None,
            expected: Some([i, j]),
            ..
        } => format!(
            "s_pairs.pairs lacks the pair ({i}, {j}), whose leading monomials are not coprime"
        ),
        PairDefect::Unexpected {
            index,
            given: Some([a, b]),
            expected: None,
        } => format!("s_pairs.pairs[{index}] = ({a}, {b}) comes after the last pair"),
        PairDefect::Unexpected { index, .. } => format!("s_pairs.pairs[{index}] is wrong"),
    }
}

/// `(L / lt(g)) g` for a monomial L that lm(g) divides: the multiple of `g`
/// whose leading term is the monomial L. The product stops at the
/// checkpoints of the job; an exponent of the product beyond `u32::MAX`
/// refuses the value.
fn multiple_with_leading_monomial(
    operation: &str,
    g: &Polynomial,
    l: &[u32],
    context: &Context<'_>,
) -> Result<Polynomial, CasError> {
    let Some((m, c)) = g.leading_term() else {
        return Err(refuse(operation, "a zero basis element has no S-pair"));
    };
    let quotient: Vec<u32> = l.iter().zip(m).map(|(a, b)| a - b).collect();
    let factor = Polynomial::from_terms(g.ring().clone(), [(quotient, Rational(c.0.recip()))])
        .map_err(|e| refuse(operation, e))?;
    g.mul_with(&factor, &mut || {
        context.interrupt.checkpoint().map_err(Stop::Interrupted)
    })
    .map_err(|e| e.into_error(operation))
}

/// `gb.groebner` (Issue #68): G = (g_1, ..., g_t), each element nonzero, is
/// a Groebner basis of (G) if and only if every S-pair
/// `S(g_i, g_j) = (L / lt(g_i)) g_i - (L / lt(g_j)) g_j`, with
/// `L = lcm(lm(g_i), lm(g_j))` and i < j, has an lcm representation
/// `S(g_i, g_j) = sum_k h_k g_k` with `lm(h_k g_k) < L` whenever
/// `h_k g_k != 0` (Cox, Little and O'Shea, Ideals, Varieties, and
/// Algorithms, Chapter 2, §9). A pair with coprime leading monomials always
/// has one (Buchberger's first criterion), and is not listed.
///
/// Checks that the list of pairs is the one G and the order give, that H is
/// t x P, that `S(g_i, g_j) = sum_k g_k H_kp` for the p-th pair (i, j), and
/// that `lm(H_kp) + lm(g_k) < L` for each nonzero entry. The equations and
/// inequalities are checked a group of pairs at a time, in the order of the
/// list, so that a wrong pair is refused before the S-polynomials of later
/// groups are built. Each step stops at
/// the checkpoints of the job. The check adds no element to G, chooses no
/// reducer, divides nothing, and goes through every pair.
fn check_s_pairs(
    operation: &str,
    ring: &RingSpec,
    g: &[Polynomial],
    s_pairs: &SPairsCertificate,
    context: &Context<'_>,
) -> Result<(), CasError> {
    let leading = leading_monomials(g);
    let t = g.len();
    let pairs = &s_pairs.pairs;
    let defect =
        certificate::s_pair_defect(&leading, pairs, &mut || context.interrupt.checkpoint())?;
    if let Some(defect) = defect {
        return Err(refuse(operation, pair_defect_message(defect, t)));
    }
    let h = PolynomialMatrix::from_data(ring.clone(), &s_pairs.representations)
        .map_err(|e| refuse(operation, format!("H: {e}")))?;
    if h.nrows() != t || h.ncols() != pairs.len() {
        return Err(refuse(
            operation,
            format!(
                "H is {} x {}, not {t} x {}",
                h.nrows(),
                h.ncols(),
                pairs.len()
            ),
        ));
    }
    drop(h);
    // The pairs are checked in groups, so that the S-polynomials and the
    // columns of G H held at once stay near S_PAIR_GROUP_TERMS terms however
    // many pairs the reply lists. A group closes once the terms of its
    // S-polynomials (at most |g_i| + |g_j| each) and of its columns of H
    // reach the bound, or the terms of G if they are more: each group
    // brings the columns of G to a common denominator once.
    let sizes: Vec<usize> = g.iter().map(|p| p.terms().count()).collect();
    let bound = S_PAIR_GROUP_TERMS.max(sizes.iter().sum());
    let columns = &s_pairs.representations.columns;
    let mut start = 0;
    while start < pairs.len() {
        let mut end = start;
        let mut terms = 0usize;
        while end < pairs.len() && terms < bound {
            let [i, j] = pairs[end];
            let h_terms: usize = columns[end].iter().map(|e| e.entry.terms.len()).sum();
            terms = terms
                .saturating_add(sizes[i] + sizes[j])
                .saturating_add(h_terms);
            end += 1;
        }
        check_s_pair_group(operation, ring, g, &leading, s_pairs, start..end, context)?;
        start = end;
    }
    Ok(())
}

/// The most terms of S-polynomials and of entries of H that a group of
/// pairs in [`check_s_pairs`] holds, unless G alone has more.
const S_PAIR_GROUP_TERMS: usize = 1 << 16;

/// The checks of [`check_s_pairs`] for the pairs in `range`: the equation
/// and the inequality of each, with the S-polynomials and the columns of
/// G H of these pairs only. The pairs are a list the checker computed, and
/// H has been read and found t x P.
fn check_s_pair_group(
    operation: &str,
    ring: &RingSpec,
    g: &[Polynomial],
    leading: &[&[u32]],
    s_pairs: &SPairsCertificate,
    range: std::ops::Range<usize>,
    context: &Context<'_>,
) -> Result<(), CasError> {
    let pairs = &s_pairs.pairs[range.clone()];
    let mut lcms = Vec::with_capacity(pairs.len());
    let mut s = Vec::with_capacity(pairs.len());
    for &[i, j] in pairs {
        context.interrupt.checkpoint()?;
        let l = certificate::lcm(leading[i], leading[j]);
        let si = multiple_with_leading_monomial(operation, &g[i], &l, context)?;
        let sj = multiple_with_leading_monomial(operation, &g[j], &l, context)?;
        s.push(si.sub(&sj).map_err(|e| refuse(operation, e))?);
        lcms.push(l);
    }
    let data = &s_pairs.representations;
    let h = PolynomialMatrix::from_data(
        ring.clone(),
        &PolynomialMatrixData {
            kind: data.kind,
            rows: data.rows,
            columns: data.columns[range.clone()].to_vec(),
        },
    )
    .map_err(|e| refuse(operation, format!("H: {e}")))?;
    check_row_times(
        operation,
        "H",
        g,
        &h,
        &s,
        |q| {
            let p = range.start + q;
            let [i, j] = pairs[q];
            format!("S(g_{i}, g_{j}) is not sum_k g_k H_k{p}")
        },
        context,
    )?;
    let mut count = 0usize;
    for (q, (&[i, j], l)) in pairs.iter().zip(&lcms).enumerate() {
        let p = range.start + q;
        for (k, entry) in h.column(q).into_iter().flatten() {
            count += 1;
            if count.is_multiple_of(256) {
                context.interrupt.checkpoint()?;
            }
            let Some(m) = entry.leading_monomial() else {
                continue;
            };
            let product: Option<Vec<u32>> = m
                .iter()
                .zip(leading[k])
                .map(|(a, b)| a.checked_add(*b))
                .collect();
            let Some(product) = product else {
                return Err(refuse(operation, MathError::ExponentOverflow));
            };
            if ring.order.compare(&product, l) != Ordering::Less {
                return Err(refuse(
                    operation,
                    format!(
                        "H_{k}{p} gives no lcm representation of S(g_{i}, g_{j}): \
                         lm(H_{k}{p} g_{k}) is not below lcm(lm(g_{i}), lm(g_{j}))"
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// `gb.groebner` for a value whose certificate is given: checked by the
/// S-pairs when the job asked for them, asserted when it did not. S-pairs
/// missing when asked for, or given when not, refuse the value.
fn groebner_claim(
    operation: &str,
    ring: &RingSpec,
    g: &[Polynomial],
    requested: IdealCertificateRequest,
    s_pairs: Option<&SPairsCertificate>,
    context: &Context<'_>,
) -> Result<Claim, CasError> {
    match (requested, s_pairs) {
        (IdealCertificateRequest::SPairs, Some(s)) => {
            check_s_pairs(operation, ring, g, s, context)?;
            Ok(context.checked("gb.groebner", Method::Certificate))
        }
        (IdealCertificateRequest::SPairs, None) => Err(refuse(operation, S_PAIRS_MISSING)),
        (_, Some(_)) => Err(refuse(operation, S_PAIRS_UNREQUESTED)),
        (_, None) => Ok(context.asserted("gb.groebner", NO_S_PAIRS)),
    }
}

/// Why `gb.groebner` is asserted for a value without its certificate: the
/// reason the certificate is missing when the job asked for the S-pairs,
/// and otherwise that it asked for none.
fn groebner_reason(requested: IdealCertificateRequest, reason: &'static str) -> &'static str {
    if requested == IdealCertificateRequest::SPairs {
        reason
    } else {
        NO_S_PAIRS
    }
}

/// The checks of `ideal.groebner_basis`.
pub struct GroebnerVerifier;

impl OperationVerifier for GroebnerVerifier {
    fn operation(&self) -> &'static str {
        IDEAL_GROEBNER_BASIS
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        const OP: &str = IDEAL_GROEBNER_BASIS;
        let request = request(OP, spec)?;
        let ring = request.ring().clone();
        let f = &request.ideal.generators;
        let result: GroebnerBasisResult = read_value(OP, value)?;

        // gb.reduced_form, from the value alone.
        let g = check_reduced_basis(OP, &ring, &result.basis, context)?;
        let mut claims = vec![context.checked("gb.reduced_form", Method::SelfCertifying)];

        let requested = request.certificate;
        if let Some(reason) = missing(OP, &result.certificate, !requested.any(), context)? {
            claims.push(context.asserted("gb.contained", reason));
            claims.push(context.asserted("gb.contains", reason));
            claims.push(context.asserted("gb.groebner", groebner_reason(requested, reason)));
            return Ok(claims);
        }
        let Some(Certificate::Given(c)) = &result.certificate else {
            unreachable!("a missing certificate is handled above")
        };
        check_basis_in_generators(OP, &ring, f, &g, &c.basis_in_generators, context)?;
        claims.push(context.checked("gb.contained", Method::Certificate));
        check_generators_in_basis(OP, &ring, f, &g, &c.generators_in_basis, context)?;
        claims.push(context.checked("gb.contains", Method::Certificate));
        claims.push(groebner_claim(
            OP,
            &ring,
            &g,
            requested,
            c.s_pairs.as_ref(),
            context,
        )?);
        Ok(claims)
    }
}

/// The checks of `ideal.normal_form`.
pub struct NormalFormVerifier;

impl OperationVerifier for NormalFormVerifier {
    fn operation(&self) -> &'static str {
        IDEAL_NORMAL_FORM
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        const OP: &str = IDEAL_NORMAL_FORM;
        let request = request(OP, spec)?;
        let ring = request.ring().clone();
        let generators = &request.ideal.generators;
        let f = polynomial(OP, &request)?;
        let result: NormalFormResult = read_value(OP, value)?;

        // gb.reduced_form and the reduced remainder, from the value alone.
        let g = check_reduced_basis(OP, &ring, &result.basis, context)?;
        let r = read_polynomial(OP, &ring, "the remainder", &result.remainder)?;
        check_reduced_remainder(OP, &r, &g, context)?;
        let mut claims = vec![context.checked("gb.reduced_form", Method::SelfCertifying)];

        let requested = request.certificate;
        if let Some(reason) = missing(OP, &result.certificate, !requested.any(), context)? {
            claims.push(context.asserted("gb.contained", reason));
            claims.push(context.asserted("gb.contains", reason));
            claims.push(context.asserted("gb.groebner", groebner_reason(requested, reason)));
            claims.push(context.asserted("normal_form.reduced", reason));
            claims.push(context.theorem("normal_form.unique", &UNIQUE_PREMISES));
            return Ok(claims);
        }
        let Some(Certificate::Given(c)) = &result.certificate else {
            unreachable!("a missing certificate is handled above")
        };
        check_basis_in_generators(OP, &ring, generators, &g, &c.basis_in_generators, context)?;
        claims.push(context.checked("gb.contained", Method::Certificate));
        check_generators_in_basis(OP, &ring, generators, &g, &c.generators_in_basis, context)?;
        claims.push(context.checked("gb.contains", Method::Certificate));
        claims.push(groebner_claim(
            OP,
            &ring,
            &g,
            requested,
            c.s_pairs.as_ref(),
            context,
        )?);
        check_quotients(OP, &ring, f, &r, &g, &c.quotients, context)?;
        claims.push(context.checked("normal_form.reduced", Method::Certificate));
        claims.push(context.theorem("normal_form.unique", &UNIQUE_PREMISES));
        Ok(claims)
    }
}

/// The checks of `ideal.membership`.
pub struct MembershipVerifier;

impl OperationVerifier for MembershipVerifier {
    fn operation(&self) -> &'static str {
        IDEAL_MEMBERSHIP
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        const OP: &str = IDEAL_MEMBERSHIP;
        let request = request(OP, spec)?;
        let ring = request.ring().clone();
        let generators = &request.ideal.generators;
        let f = polynomial(OP, &request)?;
        let result: MembershipResult = read_value(OP, value)?;
        let claim = if result.member {
            "membership.in"
        } else {
            "membership.not_in"
        };

        // The kind of the certificate is the kind `member` says.
        match (&result.certificate, result.member) {
            (Some(Certificate::Given(MembershipCertificate::NotInIdeal { .. })), true) => {
                return Err(refuse(
                    OP,
                    "member is true and the certificate is not one of coefficients",
                ));
            }
            (Some(Certificate::Given(MembershipCertificate::InIdeal { .. })), false) => {
                return Err(refuse(
                    OP,
                    "member is false and the certificate is one of coefficients",
                ));
            }
            _ => {}
        }
        let requested = request.certificate;
        if let Some(reason) = missing(OP, &result.certificate, !requested.any(), context)? {
            return Ok(vec![context.asserted(claim, reason)]);
        }
        let Some(Certificate::Given(c)) = &result.certificate else {
            unreachable!("a missing certificate is handled above")
        };
        match c {
            MembershipCertificate::InIdeal { coefficients } => {
                check_coefficients(OP, &ring, generators, f, coefficients, context)?;
                Ok(vec![context.checked(claim, Method::Certificate)])
            }
            MembershipCertificate::NotInIdeal {
                basis,
                generators_in_basis,
                quotients,
                remainder,
                s_pairs,
            } => {
                let g = check_reduced_basis(OP, &ring, basis, context)?;
                let r = read_polynomial(OP, &ring, "the remainder", remainder)?;
                if r.is_zero() {
                    return Err(refuse(OP, "member is false and the remainder is 0"));
                }
                check_reduced_remainder(OP, &r, &g, context)?;
                let mut claims = vec![context.checked("gb.reduced_form", Method::SelfCertifying)];
                check_generators_in_basis(OP, &ring, generators, &g, generators_in_basis, context)?;
                claims.push(context.checked("gb.contains", Method::Certificate));
                claims.push(groebner_claim(
                    OP,
                    &ring,
                    &g,
                    requested,
                    s_pairs.as_ref(),
                    context,
                )?);
                check_quotients(OP, &ring, f, &r, &g, quotients, context)?;
                claims.push(context.checked("normal_form.reduced", Method::Certificate));
                claims.push(context.theorem(claim, &NOT_IN_PREMISES));
                Ok(claims)
            }
        }
    }
}
