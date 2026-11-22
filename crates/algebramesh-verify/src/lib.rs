//! The checks of AlgebraMesh (ADR 0018, ADR 0019): after a backend has
//! answered, a [`Registry`] checks the result of each job against the job's
//! specification and writes its guarantee.
//!
//! * An operation with an [`OperationVerifier`] gets the claims it gives:
//!   `checked` for what it decided itself, `asserted` for what it takes from
//!   the backend. A result it finds wrong is refused (`UNSUPPORTED_RESULT`).
//! * Any other operation gets one claim, its value, `asserted` by the backend.
//!
//! A checker reads the input from the specification, never from the result,
//! so that a backend cannot pass by answering another question.
//!
//! A recorded result is read again in two ways (V4, ADR 0031):
//! [`Registry::legacy_guarantee`] maps a record without a guarantee to the
//! claims its fields record, and [`recheck`] checks a record again from its
//! specification and value.

pub mod catalogue;
pub mod certificate;
mod factor;
pub mod ideal;
pub mod matrix;
pub mod module;
pub mod recheck;

use std::collections::BTreeMap;

use algebramesh_core::check::Interrupt;
use algebramesh_core::guarantee::{
    Claim, EvidenceLevel, Guarantee, Method, ResultVerifier, Scope, VerifierInfo,
};
use algebramesh_core::{BackendInfo, CasError, CasResult, JobSpec};
use algebramesh_ops::certificate::{CERTIFICATE_TOO_LARGE, Certificate};

pub use factor::FactorVerifier;

/// The name this crate gives itself in guarantees.
pub const NAME: &str = "algebramesh-verify";

/// What a checker knows besides the specification and the value.
pub struct Context<'a> {
    /// Raised past the job's deadline; long checks stop at its checkpoints.
    pub interrupt: &'a Interrupt,
    /// The backend that produced the result, for the claims it asserts.
    pub producer: String,
    /// This checker, for the claims it decides.
    pub checker: String,
    /// The checker left the certificate of the value out for its size, after
    /// checking it: the claims it supported are asserted.
    pub certificate_left_out: bool,
    /// The value is a recorded one, checked again (V4): a field that writers
    /// of the time did not write yet (such as `evidence_terms`, ADR 0028)
    /// makes the claims it supports asserted instead of refusing the value.
    pub recheck: bool,
}

impl Context<'_> {
    /// A claim this checker decided by `method`.
    pub fn checked(&self, id: &str, method: Method) -> Claim {
        Claim {
            id: id.to_owned(),
            level: EvidenceLevel::Checked,
            method: Some(method),
            by: self.checker.clone(),
            scope: Scope::Exact,
            depends_on: Vec::new(),
            reason: None,
        }
    }

    /// A claim this checker derived by a theorem from the claims
    /// `premises` of the same result: its own level is `checked`, and its
    /// effective level is the weakest of its own and theirs (ADR 0018).
    pub fn theorem(&self, id: &str, premises: &[&str]) -> Claim {
        Claim {
            depends_on: premises.iter().map(|p| (*p).to_owned()).collect(),
            ..self.checked(id, Method::Theorem)
        }
    }

    /// A claim of a record without a guarantee that the field `field` of its
    /// value records as checked by the record's writer before it was written
    /// (V4): checked, by the producer that wrote it.
    pub fn recorded(&self, id: &str, method: Method, field: &str) -> Claim {
        Claim {
            id: id.to_owned(),
            level: EvidenceLevel::Checked,
            method: Some(method),
            by: self.producer.clone(),
            scope: Scope::Exact,
            depends_on: Vec::new(),
            reason: Some(format!(
                "the field {field} of the record shows that its writer checked it; mapped by \
                 {}",
                self.checker
            )),
        }
    }

    /// A claim taken from the backend as it is.
    pub fn asserted(&self, id: &str, reason: &str) -> Claim {
        Claim {
            id: id.to_owned(),
            level: EvidenceLevel::Asserted,
            method: None,
            by: self.producer.clone(),
            scope: Scope::Exact,
            depends_on: Vec::new(),
            reason: Some(reason.to_owned()),
        }
    }
}

/// Why the claims a certificate supports are asserted, if they are: the job
/// asked for no certificate (`none_requested`), or the checker itself left
/// the checked certificate out for its size. A certificate missing
/// otherwise, or left out by the backend, refuses the result: a level is
/// never lowered silently (ADR 0018). Packages use it for certificates
/// placed as `certificate` in their values.
pub fn missing<T>(
    operation: &str,
    certificate: &Option<Certificate<T>>,
    none_requested: bool,
    context: &Context<'_>,
) -> Result<Option<&'static str>, CasError> {
    let refuse = |message: &str| CasError::UnsupportedResult(format!("{operation}: {message}"));
    match certificate {
        Some(Certificate::Given(_)) => Ok(None),
        None if none_requested => Ok(Some("the job asked for no certificate")),
        None => Err(refuse(
            "the job asked for a certificate and the value has none",
        )),
        Some(Certificate::Omitted { .. }) if context.certificate_left_out => {
            Ok(Some(CERTIFICATE_TOO_LARGE))
        }
        Some(Certificate::Omitted { .. }) => Err(refuse(
            "the backend left the certificate out; only the checker does, for its size",
        )),
    }
}

/// The checks of one operation.
pub trait OperationVerifier: Send + Sync {
    /// The operation it checks, e.g. `polynomial.factor`.
    fn operation(&self) -> &'static str;

    /// Checks `value`, the result of `spec`, and gives the claims of the
    /// result; an error refuses it.
    fn verify(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Result<Vec<Claim>, CasError>;

    /// The entry of `docs/claims.md` whose claims the user has confirmed;
    /// none until then, which keeps the summary below `verified`.
    fn catalog(&self) -> Option<&'static str> {
        None
    }

    /// Properties the result does not claim.
    fn not_claimed(&self) -> Vec<String> {
        Vec::new()
    }

    /// Properties the result of `spec` with `value` does not claim, for an
    /// operation whose claims depend on its input; [`Self::not_claimed`]
    /// otherwise.
    fn not_claimed_for(&self, spec: &JobSpec, value: &serde_json::Value) -> Vec<String> {
        let _ = (spec, value);
        self.not_claimed()
    }

    /// The claims of `value`, a result recorded without a guarantee, from the
    /// value alone (V4, ADR 0031): a claim whose check a field of the value
    /// records ([`Context::recorded`]), or that this checker decides from the
    /// value without the specification, is `checked`; the others are
    /// `asserted`. `None` for an operation whose values record no checks:
    /// its record is then asserted as a whole. An error refuses the value,
    /// as [`Self::verify`] would.
    fn legacy_claims(
        &self,
        value: &serde_json::Value,
        context: &Context<'_>,
    ) -> Option<Result<Vec<Claim>, CasError>> {
        let _ = (value, context);
        None
    }
}

/// Why a record without a guarantee claims its value only as asserted.
pub const NOT_RECORDED: &str = "the record has no guarantee, and the values of this operation \
                                record no checks; algebramesh verify checks it again";

/// The checkers by operation; a [`ResultVerifier`] for the runner.
pub struct Registry {
    verifiers: BTreeMap<&'static str, Box<dyn OperationVerifier>>,
}

impl Registry {
    /// No checker: every result is asserted by its backend.
    pub fn empty() -> Self {
        Self {
            verifiers: BTreeMap::new(),
        }
    }

    /// The checkers of this crate.
    pub fn standard() -> Self {
        let mut registry = Self::empty();
        registry.register(Box::new(FactorVerifier));
        registry.register(Box::new(matrix::RankVerifier));
        registry.register(Box::new(matrix::KernelVerifier));
        registry.register(Box::new(matrix::SolveVerifier));
        registry.register(Box::new(matrix::HermiteVerifier));
        registry.register(Box::new(matrix::SmithVerifier));
        registry.register(Box::new(ideal::GroebnerVerifier));
        registry.register(Box::new(ideal::NormalFormVerifier));
        registry.register(Box::new(ideal::MembershipVerifier));
        registry.register(Box::new(module::SyzygiesVerifier));
        registry.register(Box::new(module::FreeResolutionVerifier));
        registry.register(Box::new(module::HilbertSeriesVerifier));
        registry
    }

    pub fn register(&mut self, verifier: Box<dyn OperationVerifier>) {
        self.verifiers.insert(verifier.operation(), verifier);
    }

    /// Whether `operation` has a checker.
    pub fn checks(&self, operation: &str) -> bool {
        self.verifiers.contains_key(operation)
    }

    /// The guarantee of a result of `operation` recorded without one (V4,
    /// ADR 0031), from its value alone, written by the driver
    /// `driver_version`; nothing is checked against the specification.
    ///
    /// * The four operations whose values record their checks
    ///   (`polynomial.factor`, `gkz.generate`, `gkz.indicial`, `gkz.series`)
    ///   get the claims and the levels their checker gives a value of the
    ///   same shape ([`OperationVerifier::legacy_claims`]). A field that
    ///   writers of the time did not write (`evidence_terms`, `certificate`)
    ///   lowers the claims it supports to `asserted`.
    /// * Any other operation gets one claim, its value, `asserted` with the
    ///   reason [`NOT_RECORDED`]: without the specification, which of its
    ///   claims a value makes is not known (`solve.solution` or
    ///   `solve.no_solution`, for one).
    ///
    /// An error refuses the value: a field records a check that failed, or
    /// the value cannot be read.
    pub fn legacy_guarantee(
        &self,
        operation: &str,
        value: &serde_json::Value,
        driver_version: &str,
    ) -> Result<Guarantee, CasError> {
        let info = info();
        let interrupt = Interrupt::none();
        let context = Context {
            interrupt: &interrupt,
            producer: if driver_version.is_empty() {
                "the record's writer".to_owned()
            } else {
                driver_version.to_owned()
            },
            checker: format!("{} {} (legacy_guarantee)", info.name, info.version),
            certificate_left_out: false,
            recheck: true,
        };
        let verifier = self.verifiers.get(operation).map(Box::as_ref);
        match verifier.and_then(|v| v.legacy_claims(value, &context)) {
            Some(claims) => Ok(Guarantee::new(
                info,
                verifier.and_then(|v| v.catalog()).map(str::to_owned),
                claims?,
                verifier.map(|v| v.not_claimed()).unwrap_or_default(),
            )),
            None => Ok(Guarantee::new(
                info,
                None,
                vec![context.asserted("value", NOT_RECORDED)],
                Vec::new(),
            )),
        }
    }

    /// The guarantee of `value`, a recorded result of `spec` produced by
    /// `backend`, checked again (V4): the checks of [`ResultVerifier::verify`]
    /// with [`Context::recheck`] set, and the value is never changed. An
    /// error refuses the value.
    pub fn check_again(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        backend: &BackendInfo,
        interrupt: &Interrupt,
    ) -> Result<Guarantee, CasError> {
        let info = info();
        let context = Context {
            interrupt,
            producer: producer(backend),
            checker: format!("{} {}", info.name, info.version),
            certificate_left_out: false,
            recheck: true,
        };
        self.guarantee(spec, value, info, &context)
    }

    /// The guarantee the checker of `spec`'s operation gives `value`, or the
    /// one of an operation without a checker.
    fn guarantee(
        &self,
        spec: &JobSpec,
        value: &serde_json::Value,
        info: VerifierInfo,
        context: &Context<'_>,
    ) -> Result<Guarantee, CasError> {
        Ok(match self.verifiers.get(spec.operation.as_str()) {
            Some(verifier) => Guarantee::new(
                info,
                verifier.catalog().map(str::to_owned),
                verifier.verify(spec, value, context)?,
                verifier.not_claimed_for(spec, value),
            ),
            None => Guarantee::new(
                info,
                None,
                vec![context.asserted("value", "no checker for this operation")],
                Vec::new(),
            ),
        })
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::standard()
    }
}

/// This checker's name, version and build.
pub fn info() -> VerifierInfo {
    let commit = option_env!("ALGEBRAMESH_BUILD_COMMIT")
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(|c| {
            if option_env!("ALGEBRAMESH_BUILD_CLEAN") == Some("1") {
                c.to_owned()
            } else {
                format!("{c}-modified")
            }
        });
    VerifierInfo {
        name: NAME.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        commit,
    }
}

/// The most bytes the value of a result may take, leaving room in the report
/// for the rest of the result.
pub const VALUE_BUDGET: usize = algebramesh_core::MAX_RESULT_BYTES - 256 * 1024;

fn json_size(value: &serde_json::Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |v| v.len())
}

/// Whether `value` carries a certificate and is larger than a result may be.
fn certificate_too_large(value: &serde_json::Value) -> bool {
    value
        .get("certificate")
        .is_some_and(|c| c.get("omitted").is_none())
        && json_size(value) > VALUE_BUDGET
}

/// The backend as it names itself in the result.
fn producer(backend: &BackendInfo) -> String {
    let mut name = backend.name.clone();
    if let Some(version) = &backend.backend_version {
        name.push(' ');
        name.push_str(version);
    }
    if !backend.driver_version.is_empty() {
        name.push_str(&format!(" ({})", backend.driver_version));
    }
    name
}

impl ResultVerifier for Registry {
    fn verify(
        &self,
        spec: &JobSpec,
        mut result: CasResult,
        interrupt: &Interrupt,
    ) -> Result<CasResult, CasError> {
        let info = info();
        let mut context = Context {
            interrupt,
            producer: producer(&result.backend),
            checker: format!("{} {}", info.name, info.version),
            certificate_left_out: false,
            recheck: false,
        };
        let mut guarantee = self.guarantee(spec, &result.value, info.clone(), &context)?;
        if self.checks(&spec.operation) && certificate_too_large(&result.value) {
            // Before #36 a result must fit a report: the checked certificate
            // is left out, and the claims it supports become asserted (ADR
            // 0018).
            result.value["certificate"] = serde_json::json!({
                "omitted": CERTIFICATE_TOO_LARGE
            });
            context.certificate_left_out = true;
            guarantee = self.guarantee(spec, &result.value, info, &context)?;
            let size = json_size(&result.value);
            if size > VALUE_BUDGET {
                return Err(CasError::ResourceLimit(format!(
                    "the value takes {size} bytes without its certificate, more than the \
                     {VALUE_BUDGET} a result may"
                )));
            }
        }
        result.guarantee = Some(guarantee);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use algebramesh_core::guarantee::Summary;

    use super::*;

    fn spec(operation: &str) -> JobSpec {
        JobSpec {
            schema_version: 1,
            backend: "test".into(),
            operation: operation.into(),
            arguments: serde_json::json!({}),
            limits: Default::default(),
            retry_policy: Default::default(),
            environment: Default::default(),
        }
    }

    /// Issue #47: an operation without a checker claims only its value, as
    /// asserted by the backend.
    #[test]
    fn an_operation_without_a_checker_is_asserted() {
        let result = CasResult {
            value: serde_json::json!({"anything": 1}),
            backend: BackendInfo {
                name: "test".into(),
                backend_version: Some("2".into()),
                driver_version: "test-driver 1".into(),
                ..BackendInfo::default()
            },
            execution_host: None,
            log: vec![],
            guarantee: None,
        };
        let checked = Registry::standard()
            .verify(&spec("test.sleep"), result, &Interrupt::default())
            .unwrap();
        let g = checked.guarantee.unwrap();
        assert_eq!(g.summary, Summary::Unverified);
        assert!(g.claims.iter().all(|c| c.level == EvidenceLevel::Asserted));
        assert_eq!(g.claims[0].by, "test 2 (test-driver 1)");
        assert_eq!(g.verifier.name, NAME);
    }
}
