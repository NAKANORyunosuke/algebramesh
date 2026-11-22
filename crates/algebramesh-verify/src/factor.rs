//! `polynomial.factor`: the factorization in the value is checked against the
//! polynomial of the job's specification (Issues #47, #48).
//!
//! | claim | statement | level |
//! |---|---|---|
//! | `factor.product` | the input = constant × Π f_i^{m_i}, each m_i ≥ 1 and each f_i not constant | checked |
//! | `factor.domain` | the coefficients lie in the declared domain | checked |
//! | `factor.primitive` | over ZZ, the coefficients of each f_i have gcd 1 | checked |
//! | `factor.non_associate` | no two f_i are associates | checked |
//! | `factor.irreducible` | each f_i is irreducible | asserted |
//!
//! A record without a guarantee (V4) gets the same claims: `factor.product`
//! from its field `verified_product`, which a result has only once its
//! product was checked, and the others from its value alone.

use algebramesh_core::guarantee::{Claim, Method};
use algebramesh_core::{CasError, JobSpec};
use algebramesh_math_types::RingSpec;
use algebramesh_math_types::{CoefficientDomain, Factorization, MathError, Polynomial};
use algebramesh_ops::{FactorArguments, FactorResult, POLYNOMIAL_FACTOR};

use crate::{Context, OperationVerifier, certificate};

/// The checks of `polynomial.factor`.
pub struct FactorVerifier;

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

fn refuse(message: impl std::fmt::Display) -> CasError {
    CasError::UnsupportedResult(format!("{POLYNOMIAL_FACTOR}: {message}"))
}

impl OperationVerifier for FactorVerifier {
    fn operation(&self) -> &'static str {
        POLYNOMIAL_FACTOR
    }

    fn verify(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError> {
        // The input comes from the specification, not from the value.
        let args: FactorArguments = serde_json::from_value(spec.arguments.clone())
            .map_err(|e| refuse(format!("the arguments of the job cannot be read: {e}")))?;
        let input = Polynomial::from_data(args.ring.clone(), &args.expression)
            .map_err(|e| refuse(format!("the polynomial of the job: {e}")))?;
        let result = read(value)?;
        if result.ring != args.ring {
            return Err(refuse("the ring of the value is not the ring of the job"));
        }
        let factorization = factorization(&args.ring, &result)?;

        // factor.product.
        let product = factorization
            .expand_with(|| context.interrupt.checkpoint().map_err(Stop::Interrupted))
            .map_err(|e| match e {
                Stop::Interrupted(e) => e,
                Stop::Math(e) => refuse(e),
            })?;
        if product != input {
            return Err(refuse(format!(
                "the factors multiply to {product}, not to the input {input}"
            )));
        }
        let mut claims = vec![context.checked("factor.product", Method::SelfCertifying)];
        claims.extend(value_claims(&args.ring, &factorization, context)?);
        Ok(claims)
    }

    /// `factor.product` from `verified_product`, which a backend wrote only
    /// once the product was checked before guarantees existed, and which a
    /// recorded value has only if its job succeeded; the other claims from
    /// the value, in the ring it names.
    fn legacy_claims(
        &self,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Option<Result<Vec<Claim>, CasError>> {
        Some(legacy(value, context))
    }
}

fn legacy(value: &serde_json::Value, context: &Context<'_>) -> Result<Vec<Claim>, CasError> {
    let result = read(value)?;
    if !result.verified_product {
        return Err(refuse(
            "the field verified_product of the value is not true",
        ));
    }
    let factorization = factorization(&result.ring, &result)?;
    let mut claims =
        vec![context.recorded("factor.product", Method::SelfCertifying, "verified_product")];
    claims.extend(value_claims(&result.ring, &factorization, context)?);
    Ok(claims)
}

fn read(value: &serde_json::Value) -> Result<FactorResult, CasError> {
    serde_json::from_value(value.clone())
        .map_err(|e| refuse(format!("the value is not a factorization: {e}")))
}

/// The factorization of `result` in `ring`; factor.domain, and the factors
/// of multiplicity 0 and the constant ones that the product alone would let
/// through.
fn factorization(ring: &RingSpec, result: &FactorResult) -> Result<Factorization, CasError> {
    // factor.domain: the constant and every coefficient of every factor.
    if !ring.domain.contains(&result.constant) {
        return Err(refuse(format!(
            "the constant {} is not in {}",
            result.constant,
            ring.domain.name()
        )));
    }
    let mut factors = Vec::with_capacity(result.factors.len());
    for f in &result.factors {
        let p = Polynomial::from_data(ring.clone(), &f.factor)
            .map_err(|e| refuse(format!("the factor {}: {e}", f.display)))?;
        // A factor of multiplicity 0 multiplies to 1, so that the product
        // alone would let it through (Issue #48); a constant one belongs in
        // the constant.
        if f.multiplicity == 0 {
            return Err(refuse(format!(
                "the factor {} has multiplicity 0",
                f.display
            )));
        }
        if p.is_constant() {
            return Err(refuse(format!(
                "the factor {} is a constant; constants belong in the constant",
                f.display
            )));
        }
        factors.push((p, f.multiplicity));
    }
    Factorization::new(ring.clone(), result.constant.clone(), factors).map_err(refuse)
}

/// The claims after `factor.product`, decided from the factorization alone.
fn value_claims(
    ring: &RingSpec,
    factorization: &Factorization,
    context: &Context<'_>,
) -> Result<Vec<Claim>, CasError> {
    let mut claims = vec![context.checked("factor.domain", Method::SelfCertifying)];
    let integers = ring.domain == CoefficientDomain::Integers;
    if integers {
        for f in &factorization.factors {
            let p = Polynomial::from_data(ring.clone(), &f.factor).map_err(refuse)?;
            if !certificate::is_primitive(&p) {
                return Err(refuse(format!(
                    "over ZZ the factor {} is not primitive",
                    f.display
                )));
            }
        }
        claims.push(context.checked("factor.primitive", Method::SelfCertifying));
    }
    // factor.non_associate (Issue #102): f_i = u f_j for a unit u of K
    // exactly when f_i lc(f_j) = f_j lc(f_i), lc in the order of the ring.
    // Over ZZ the units are those of QQ restricted to ZZ, so factors not
    // proportional over QQ are not associates over ZZ either.
    let polynomials = factorization
        .factors
        .iter()
        .map(|f| Polynomial::from_data(ring.clone(), &f.factor).map_err(refuse))
        .collect::<Result<Vec<_>, _>>()?;
    for (i, f) in polynomials.iter().enumerate() {
        for (j, g) in polynomials.iter().enumerate().skip(i + 1) {
            context.interrupt.checkpoint()?;
            let (Some(lf), Some(lg)) = (f.leading_coefficient(), g.leading_coefficient()) else {
                return Err(refuse("a factor is zero"));
            };
            if f.scale(lg).map_err(refuse)? == g.scale(lf).map_err(refuse)? {
                return Err(refuse(format!(
                    "the factors {} and {} are associates",
                    factorization.factors[i].display, factorization.factors[j].display
                )));
            }
        }
    }
    claims.push(context.checked("factor.non_associate", Method::SelfCertifying));
    let mut irreducible = context.asserted("factor.irreducible", "decided by the backend");
    // The factors are not constant (factor.product).
    irreducible.depends_on = vec!["factor.product".into()];
    if integers {
        // Over ZZ, irreducibility reduces to QQ by Gauss's lemma for
        // primitive factors.
        irreducible.depends_on.push("factor.primitive".into());
    }
    claims.push(irreducible);
    Ok(claims)
}
