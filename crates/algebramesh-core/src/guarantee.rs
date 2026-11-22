//! What a result claims and how well each claim is supported (ADR 0018). Data
//! only: the checks themselves are in `algebramesh-verify`, which a
//! [`ResultVerifier`] brings in.

use serde::{Deserialize, Serialize};

use crate::backend::{CasError, CasResult};
use crate::check::Interrupt;
use crate::job::JobSpec;

/// The version of the shape of [`Guarantee`].
pub const GUARANTEE_SCHEMA: u32 = 1;

/// How well a claim is supported, from the weakest (ADR 0018).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceLevel {
    /// The answer of a named producer, recorded as it is.
    Asserted,
    /// Two producers whose implementations share no lineage agree.
    CrossChecked,
    /// A checker of AlgebraMesh found it true from the spec's input, the
    /// output and a certificate, in exact arithmetic.
    Checked,
}

/// How a claim was decided (ADR 0018).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    SelfCertifying,
    Certificate,
    /// Computed again by AlgebraMesh's own code, which is then trusted.
    Recomputation,
    Theorem,
    IndependentImplementation,
    Consistency,
    Reference,
}

/// What part of the mathematical object a claim covers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    #[default]
    Exact,
    /// A series up to `order` in the weight `weight`.
    Truncated { weight: Vec<i64>, order: u64 },
}

impl Scope {
    fn is_exact(&self) -> bool {
        *self == Self::Exact
    }
}

/// One claim about a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// The claim's name, e.g. `factor.product` (`docs/claims.md`).
    pub id: String,
    pub level: EvidenceLevel,
    /// How it was decided; none for a claim recorded as asserted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<Method>,
    /// The checker that decided it, or the producer that asserted it.
    pub by: String,
    #[serde(default, skip_serializing_if = "Scope::is_exact")]
    pub scope: Scope,
    /// Claims this one assumes; its effective level is the weakest of its
    /// own and theirs. A claim of another job's result is named
    /// `job:<job_id>#<claim>` ([`job_claim`], W2); within one result it
    /// counts as `asserted`, and a workflow's report looks it up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The guarantee of a whole result (ADR 0018).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Summary {
    /// The contract of the operation is confirmed, and every claim is
    /// effectively `checked`.
    Verified,
    /// Some claims are effectively `checked` and some are not, or the
    /// contract is not confirmed yet.
    PartiallyVerified,
    /// No claim is effectively `checked`.
    Unverified,
}

/// The checker that wrote a guarantee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierInfo {
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// What a result claims, with the level of each claim and a summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Guarantee {
    pub schema: u32,
    /// The contract (`docs/claims.md`) the claims follow, once the user has
    /// confirmed it; until then the summary is at most `partially_verified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    pub verifier: VerifierInfo,
    pub summary: Summary,
    pub claims: Vec<Claim>,
    /// Properties the result does not claim at all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_claimed: Vec<String>,
}

impl Guarantee {
    /// A guarantee of `claims`, with its summary.
    pub fn new(
        verifier: VerifierInfo,
        catalog: Option<String>,
        claims: Vec<Claim>,
        not_claimed: Vec<String>,
    ) -> Self {
        let summary = summarize(&claims, catalog.is_some());
        Self {
            schema: GUARANTEE_SCHEMA,
            catalog,
            verifier,
            summary,
            claims,
            not_claimed,
        }
    }

    /// The effective level of the claim `id`: the weakest of its own level and
    /// the effective levels of the claims it depends on. A claim that is
    /// missing, or depends on one that is missing or on itself, is asserted;
    /// so is a claim of another job's result (`job:<job_id>#<claim>`).
    pub fn effective_level(&self, id: &str) -> EvidenceLevel {
        effective(&self.claims, id, &mut Vec::new(), &|_, _| {
            EvidenceLevel::Asserted
        })
    }

    /// [`Self::effective_level`], with the claims of other jobs' results
    /// (`job:<job_id>#<claim>`) at the levels `other` gives (W2).
    pub fn effective_level_with(
        &self,
        id: &str,
        other: &dyn Fn(&str, &str) -> EvidenceLevel,
    ) -> EvidenceLevel {
        effective(&self.claims, id, &mut Vec::new(), other)
    }

    /// The summary of the claims, by their effective levels with the claims
    /// of other jobs' results at the levels `other` gives (W2).
    pub fn summary_with(&self, other: &dyn Fn(&str, &str) -> EvidenceLevel) -> Summary {
        summarize_with(&self.claims, self.catalog.is_some(), other)
    }
}

/// The prefix of a claim of another job's result in `depends_on` (W2).
pub const JOB_CLAIM_PREFIX: &str = "job:";

/// `job:<job_id>#<claim>`: the claim `claim` of the result of job `job_id`,
/// as a claim of another job names it in `depends_on` (W2).
pub fn job_claim(job_id: &str, claim: &str) -> String {
    format!("{JOB_CLAIM_PREFIX}{job_id}#{claim}")
}

/// The job and the claim a dependency `job:<job_id>#<claim>` names; `None`
/// for a claim of the same result.
pub fn parse_job_claim(dependency: &str) -> Option<(&str, &str)> {
    dependency
        .strip_prefix(JOB_CLAIM_PREFIX)?
        .split_once('#')
        .filter(|(job, claim)| !job.is_empty() && !claim.is_empty())
}

fn effective(
    claims: &[Claim],
    id: &str,
    path: &mut Vec<String>,
    other: &dyn Fn(&str, &str) -> EvidenceLevel,
) -> EvidenceLevel {
    let Some(claim) = claims.iter().find(|c| c.id == id) else {
        return EvidenceLevel::Asserted;
    };
    if path.iter().any(|p| p == id) {
        return EvidenceLevel::Asserted;
    }
    path.push(id.to_owned());
    let level = claim
        .depends_on
        .iter()
        .map(|d| match parse_job_claim(d) {
            Some((job, c)) => other(job, c),
            None if d.starts_with(JOB_CLAIM_PREFIX) => EvidenceLevel::Asserted,
            None => effective(claims, d, path, other),
        })
        .fold(claim.level, Ord::min);
    path.pop();
    level
}

/// The summary of `claims` (ADR 0018), all by effective level.
pub fn summarize(claims: &[Claim], contract_confirmed: bool) -> Summary {
    summarize_with(claims, contract_confirmed, &|_, _| EvidenceLevel::Asserted)
}

/// [`summarize`], with the claims of other jobs' results at the levels
/// `other` gives (W2).
pub fn summarize_with(
    claims: &[Claim],
    contract_confirmed: bool,
    other: &dyn Fn(&str, &str) -> EvidenceLevel,
) -> Summary {
    let checked = claims
        .iter()
        .filter(|c| effective(claims, &c.id, &mut Vec::new(), other) == EvidenceLevel::Checked)
        .count();
    if checked == 0 {
        Summary::Unverified
    } else if contract_confirmed && checked == claims.len() {
        Summary::Verified
    } else {
        Summary::PartiallyVerified
    }
}

/// Checks a result against its job's specification and writes its
/// [`Guarantee`]; an error refuses the result. Runs inside
/// [`crate::check::check_result`], under the job's deadline: a long check
/// stops at the checkpoints of `interrupt`.
///
/// Only JSON passes here, so that the core does not depend on the
/// mathematical types; `algebramesh-verify` implements it.
pub trait ResultVerifier: Send + Sync {
    fn verify(
        &self,
        spec: &JobSpec,
        result: CasResult,
        interrupt: &Interrupt,
    ) -> Result<CasResult, CasError>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn claim(id: &str, level: EvidenceLevel, depends_on: &[&str]) -> Claim {
        Claim {
            id: id.into(),
            level,
            method: None,
            by: "test".into(),
            scope: Scope::Exact,
            depends_on: depends_on.iter().map(|d| (*d).to_owned()).collect(),
            reason: None,
        }
    }

    fn verifier() -> VerifierInfo {
        VerifierInfo {
            name: "test".into(),
            version: "1".into(),
            commit: None,
        }
    }

    #[test]
    fn a_claim_is_no_stronger_than_what_it_depends_on() {
        use EvidenceLevel::*;
        let g = Guarantee::new(
            verifier(),
            None,
            vec![
                claim("a", Checked, &["b"]),
                claim("b", Asserted, &[]),
                claim("c", Checked, &["c"]),
                claim("d", Checked, &["missing"]),
                claim("e", Checked, &[]),
            ],
            vec![],
        );
        assert_eq!(g.effective_level("a"), Asserted);
        assert_eq!(g.effective_level("c"), Asserted, "a cycle supports nothing");
        assert_eq!(g.effective_level("d"), Asserted);
        assert_eq!(g.effective_level("e"), Checked);
        assert_eq!(g.effective_level("nothing"), Asserted);
    }

    #[test]
    fn claims_of_other_jobs_count_at_the_level_given_for_them() {
        use EvidenceLevel::*;
        let dep = job_claim("j1", "gb.contained");
        assert_eq!(parse_job_claim(&dep), Some(("j1", "gb.contained")));
        assert_eq!(parse_job_claim("gb.contained"), None);
        assert_eq!(parse_job_claim("job:#x"), None);
        let g = Guarantee::new(
            verifier(),
            None,
            vec![
                claim("membership.in", Checked, &[&dep]),
                claim("other", Checked, &["job:malformed"]),
            ],
            vec![],
        );
        // Within the result alone, another job's claim is not known.
        assert_eq!(g.effective_level("membership.in"), Asserted);
        let checked = |_: &str, _: &str| Checked;
        let asserted = |job: &str, c: &str| {
            assert_eq!((job, c), ("j1", "gb.contained"));
            Asserted
        };
        assert_eq!(g.effective_level_with("membership.in", &checked), Checked);
        assert_eq!(g.effective_level_with("membership.in", &asserted), Asserted);
        assert_eq!(g.effective_level_with("other", &checked), Asserted);
        assert_eq!(g.summary_with(&checked), Summary::PartiallyVerified);
    }

    #[test]
    fn the_summary_needs_a_confirmed_contract_for_verified() {
        use EvidenceLevel::*;
        let all = vec![claim("a", Checked, &[]), claim("b", Checked, &["a"])];
        assert_eq!(summarize(&all, true), Summary::Verified);
        assert_eq!(summarize(&all, false), Summary::PartiallyVerified);
        let some = vec![claim("a", Checked, &[]), claim("b", Asserted, &[])];
        assert_eq!(summarize(&some, true), Summary::PartiallyVerified);
        let none = vec![claim("a", Checked, &["b"]), claim("b", Asserted, &[])];
        assert_eq!(summarize(&none, true), Summary::Unverified);
        assert_eq!(summarize(&[], true), Summary::Unverified);
    }

    #[test]
    fn the_json_names_levels_and_scopes_in_snake_case() {
        let mut c = claim("factor.product", EvidenceLevel::CrossChecked, &[]);
        c.method = Some(Method::IndependentImplementation);
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["level"], "cross_checked");
        assert_eq!(json["method"], "independent_implementation");
        assert!(json.get("scope").is_none());
        c.scope = Scope::Truncated {
            weight: vec![1, 0, 1],
            order: 6,
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(
            json["scope"],
            serde_json::json!({"truncated": {"weight": [1, 0, 1], "order": 6}})
        );
        let back: Claim = serde_json::from_value(json).unwrap();
        assert_eq!(back, c);
    }
}
