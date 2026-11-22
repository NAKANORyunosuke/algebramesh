//! Macaulay2 backend (design §8, §9).
//!
//! Each job runs `M2 --script drivers/macaulay2/algebramesh.m2 request.json` as a
//! subprocess. The request is built here from validated, typed arguments; the driver
//! never receives variable names or source text, and answers with one JSON line that
//! is converted back into typed values and checked before it is returned.
//!
//! An order-dependent operation (`ideal.groebner_basis`, Issue #66;
//! `ideal.normal_form` and `ideal.membership`, Issue #67) sends the monomial
//! order of its ring as the items of Macaulay2's `MonomialOrder`
//! ([`order_items`]); the driver builds its ring with them.
//!
//! The GKZ operations are a package (Issues #99, #100): `packages::gkz`, built
//! with the Cargo feature `pkg-gkz`. Without it the backend does not offer
//! them. So is `simplicial.homology` (Issue #103): `packages::topology`, with
//! the feature `pkg-topology`.

use std::path::PathBuf;

use algebramesh_core::check::{Interrupt, check_result};
use algebramesh_core::driver::{DriverCommand, run_driver};
use algebramesh_core::{
    BackendInfo, Capabilities, CasBackend, CasError, CasRequest, CasResult, ExecutionContext,
};
use algebramesh_math_types::{
    BaseOrder, Factorization, MAX_WEIGHT, MonomialOrder, Polynomial, PowerBudget, Rational,
    RingSpec,
};
use algebramesh_ops::ideal::{self, IdealRequest};
use algebramesh_ops::matrix::{self, MatrixRequest};
use algebramesh_ops::module::{self, ModuleRequest};
use algebramesh_ops::{FactorArguments, FactorResult};
use serde::{Deserialize, Serialize};

pub const NAME: &str = "macaulay2";
const DRIVER_VERSION: &str = concat!("algebramesh-backend-macaulay2 ", env!("CARGO_PKG_VERSION"));

pub use algebramesh_ops::{GKZ_GENERATE, GKZ_INDICIAL, GKZ_SERIES, POLYNOMIAL_FACTOR};

mod packages;

/// The operations this backend offers without its packages.
const OPERATIONS: [&str; 12] = [
    POLYNOMIAL_FACTOR,
    matrix::MATRIX_RANK,
    matrix::MATRIX_KERNEL,
    matrix::MATRIX_SOLVE,
    matrix::MATRIX_HERMITE_NORMAL_FORM,
    matrix::MATRIX_SMITH_NORMAL_FORM,
    ideal::IDEAL_GROEBNER_BASIS,
    ideal::IDEAL_NORMAL_FORM,
    ideal::IDEAL_MEMBERSHIP,
    module::MODULE_SYZYGIES,
    module::COMPLEX_FREE_RESOLUTION,
    module::IDEAL_HILBERT_SERIES,
];

/// The operations this backend offers: [`OPERATIONS`] and those of the
/// packages it was built with.
fn operations() -> Vec<&'static str> {
    #[allow(unused_mut)]
    let mut operations = OPERATIONS.to_vec();
    #[cfg(feature = "pkg-gkz")]
    operations.extend(packages::gkz::OPERATIONS);
    #[cfg(feature = "pkg-topology")]
    operations.extend(packages::topology::OPERATIONS);
    operations
}

#[derive(Debug, Clone)]
pub struct Macaulay2Config {
    /// The `M2` executable.
    pub m2: PathBuf,
    /// `drivers/macaulay2/algebramesh.m2`.
    pub driver: PathBuf,
    /// Directory for request files.
    pub work_dir: PathBuf,
    /// Image digest or package version, recorded as provenance.
    pub build: Option<String>,
}

impl Default for Macaulay2Config {
    fn default() -> Self {
        Self {
            m2: "M2".into(),
            driver: "/opt/algebramesh/drivers/macaulay2/algebramesh.m2".into(),
            work_dir: std::env::temp_dir(),
            build: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Macaulay2Backend {
    config: Option<Macaulay2Config>,
}

impl Macaulay2Backend {
    pub fn new(config: Macaulay2Config) -> Self {
        Self {
            config: Some(config),
        }
    }
}

enum Operation {
    Factor(Polynomial),
    /// `gkz.*` (Issues #99, #100).
    #[cfg(feature = "pkg-gkz")]
    Gkz(packages::gkz::GkzOperation),
    /// `simplicial.homology` (Issue #103).
    #[cfg(feature = "pkg-topology")]
    Topology(packages::topology::TopologyOperation),
    /// `matrix.*` (Issue #57).
    Matrix(MatrixRequest),
    /// `ideal.*` (Issues #66, #67), with the items of the order of its ring.
    Ideal(IdealRequest, Vec<OrderItem>),
    /// `module.*`, `complex.*`, `ideal.hilbert_series` (Issues #97, #98),
    /// with the items of the order of its ring.
    Module(ModuleRequest, Vec<OrderItem>),
}

pub(crate) fn invalid(e: impl std::fmt::Display) -> CasError {
    CasError::InvalidArguments(e.to_string())
}

/// One item of the list Macaulay2 takes as `MonomialOrder => {...}` (Issue #64).
/// The driver receives it as `["Weights", [w_1, ..., w_k]]`, `["Lex", k]` or
/// `["GRevLex", k]` and lays it out as `Weights => {w_1, ..., w_k}`, `Lex => k`
/// or `GRevLex => k`. Macaulay2 compares by the items in turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderItem {
    /// `Weights => {w_1, ..., w_k}`: by the weighted degree. As checked in
    /// Macaulay2 1.24.11, `w_1` weighs the first variable not yet in a block
    /// (the first variable before any block), and the variables past the
    /// weights weigh 0.
    Weights(Vec<u32>),
    /// `Lex => k`: the next block of `k` variables, lexicographically.
    Lex(usize),
    /// `GRevLex => k`: the next block of `k` variables, by degree, then reverse
    /// lexicographically.
    GRevLex(usize),
}

impl Serialize for OrderItem {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Weights(w) => ("Weights", w).serialize(serializer),
            Self::Lex(k) => ("Lex", k).serialize(serializer),
            Self::GRevLex(k) => ("GRevLex", k).serialize(serializer),
        }
    }
}

// Macaulay2 takes a weight only as a small (32-bit) integer; every weight a
// valid order has is one. A larger `MAX_WEIGHT` needs a check in `order_items`.
const _: () = assert!(MAX_WEIGHT <= i32::MAX.unsigned_abs());

/// The items of `order` on `k` variables.
fn base_items(order: BaseOrder, k: usize) -> Vec<OrderItem> {
    match order {
        BaseOrder::Lex => vec![OrderItem::Lex(k)],
        // Macaulay2 1.24.11 refuses `GLex => k` as an item of a list
        // ("unrecognized ordering item keyword : GLex") and itself writes the
        // order `GLex` as `Weights => {1, ..., 1}, Lex => n`; the weights of a
        // block start at its first variable.
        BaseOrder::Grlex => vec![OrderItem::Weights(vec![1; k]), OrderItem::Lex(k)],
        BaseOrder::Grevlex => vec![OrderItem::GRevLex(k)],
    }
}

/// The items of Macaulay2's `MonomialOrder => {...}` for `order` on `nvars`
/// variables (Issue #64): the driver builds the ring of an order-dependent
/// operation with them instead of relying on Macaulay2's default (design §6,
/// rule 2). An order that is not valid for `nvars` variables, and so cannot
/// be written for Macaulay2, is refused as invalid arguments before the
/// driver runs.
///
/// A ring without variables gets one variable of the driver's own (Issue #35);
/// every order of one variable is the same, and its items are `Lex => 1`.
pub fn order_items(order: &MonomialOrder, nvars: usize) -> Result<Vec<OrderItem>, CasError> {
    order.validate(nvars).map_err(invalid)?;
    if nvars == 0 {
        return Ok(vec![OrderItem::Lex(1)]);
    }
    Ok(match order {
        MonomialOrder::Lex => base_items(BaseOrder::Lex, nvars),
        MonomialOrder::Grlex => base_items(BaseOrder::Grlex, nvars),
        MonomialOrder::Grevlex => base_items(BaseOrder::Grevlex, nvars),
        MonomialOrder::Weights { weights, then } => {
            let mut items = vec![OrderItem::Weights(weights.clone())];
            items.extend(base_items(*then, nvars));
            items
        }
        MonomialOrder::Block(blocks) => blocks
            .iter()
            .flat_map(|b| base_items(b.order, b.count))
            .collect(),
    })
}

fn parse(job: &CasRequest) -> Result<Operation, CasError> {
    #[cfg(feature = "pkg-gkz")]
    if let Some(op) = packages::gkz::parse(&job.operation, &job.arguments)? {
        return Ok(Operation::Gkz(op));
    }
    #[cfg(feature = "pkg-topology")]
    if let Some(op) = packages::topology::parse(&job.operation, &job.arguments)? {
        return Ok(Operation::Topology(op));
    }
    if matrix::OPERATIONS.contains(&job.operation.as_str()) {
        return MatrixRequest::parse(&job.operation, &job.arguments)
            .map(Operation::Matrix)
            .map_err(CasError::InvalidArguments);
    }
    if ideal::OPERATIONS.contains(&job.operation.as_str()) {
        let request = IdealRequest::parse(&job.operation, &job.arguments)
            .map_err(CasError::InvalidArguments)?;
        let order = order_items(&request.ring().order, request.ring().nvars())?;
        return Ok(Operation::Ideal(request, order));
    }
    if module::OPERATIONS.contains(&job.operation.as_str()) {
        let request = ModuleRequest::parse(&job.operation, &job.arguments)
            .map_err(CasError::InvalidArguments)?;
        let order = order_items(&request.ring().order, request.ring().nvars())?;
        return Ok(Operation::Module(request, order));
    }
    match job.operation.as_str() {
        POLYNOMIAL_FACTOR => {
            let args: FactorArguments =
                serde_json::from_value(job.arguments.clone()).map_err(invalid)?;
            let p = Polynomial::from_data(args.ring, &args.expression).map_err(invalid)?;
            if p.is_zero() {
                return Err(invalid("cannot factor the zero polynomial"));
            }
            Ok(Operation::Factor(p))
        }
        other => Err(CasError::UnsupportedOperation {
            backend: NAME.into(),
            operation: other.into(),
        }),
    }
}

#[derive(Serialize)]
pub(crate) struct WireTerm {
    pub(crate) coefficient: String,
    pub(crate) exponents: Vec<u32>,
}

fn request(op: &Operation) -> serde_json::Value {
    match op {
        Operation::Factor(p) => serde_json::json!({
            "operation": POLYNOMIAL_FACTOR,
            "domain": p.ring().domain.name(),
            "nvars": p.ring().nvars(),
            "terms": p.terms().map(|(e, c)| WireTerm { coefficient: c.to_string(), exponents: e.to_vec() }).collect::<Vec<_>>(),
        }),
        #[cfg(feature = "pkg-gkz")]
        Operation::Gkz(g) => packages::gkz::request(g),
        #[cfg(feature = "pkg-topology")]
        Operation::Topology(t) => packages::topology::request(t),
        Operation::Matrix(m) => m.driver_json(),
        Operation::Ideal(i, order) => i.driver_json(order),
        Operation::Module(m, order) => m.driver_json(order),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct DriverTerm {
    pub(crate) coefficient: String,
    pub(crate) exponents: Vec<u32>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DriverInfo {
    macaulay2_version: String,
    pub(crate) holonomic_systems_version: String,
}

impl DriverInfo {
    /// The version string recorded for jobs and offered at registration.
    pub(crate) fn describe(&self) -> String {
        format!(
            "Macaulay2 {} (HolonomicSystems {})",
            self.macaulay2_version, self.holonomic_systems_version
        )
    }
}

#[derive(Debug, Deserialize)]
struct DriverFactor {
    terms: Vec<DriverTerm>,
    multiplicity: u32,
}

/// The reply to a matrix job: the value in the form of `algebramesh-ops`;
/// also the reply to an ideal job, with the value as the driver writes it
/// (`IdealRequest::value` reads it).
#[derive(Debug, Deserialize)]
struct MatrixReply {
    value: serde_json::Value,
    info: DriverInfo,
}

#[derive(Debug, Deserialize)]
struct FactorReply {
    factors: Vec<DriverFactor>,
    info: DriverInfo,
}

pub(crate) fn bad(e: impl std::fmt::Display) -> CasError {
    CasError::UnsupportedResult(e.to_string())
}

/// The exponents a driver returned, for `ring` (Issue #35). The driver gives a
/// ring without variables one variable of its own (a polynomial ring needs
/// one), so its terms carry one exponent, which must be 0; it is dropped.
fn ring_exponents(ring: &RingSpec, exponents: &[u32]) -> Result<Vec<u32>, CasError> {
    match exponents {
        [0] if ring.nvars() == 0 => Ok(Vec::new()),
        [_] if ring.nvars() == 0 => Err(bad(format!(
            "exponents {exponents:?} for a ring without variables"
        ))),
        _ => Ok(exponents.to_vec()),
    }
}

pub(crate) fn polynomial_from(
    ring: &RingSpec,
    terms: &[DriverTerm],
) -> Result<Polynomial, CasError> {
    let terms = terms
        .iter()
        .map(|t| {
            Ok((
                ring_exponents(ring, &t.exponents)?,
                t.coefficient.parse::<Rational>().map_err(bad)?,
            ))
        })
        .collect::<Result<Vec<_>, CasError>>()?;
    Polynomial::from_terms(ring.clone(), terms).map_err(bad)
}

/// Converts and checks the driver's reply, stopping at the checkpoints once
/// `interrupt` is raised.
fn interpret(
    op: &Operation,
    reply: serde_json::Value,
    interrupt: &Interrupt,
) -> Result<(serde_json::Value, String), CasError> {
    match op {
        Operation::Factor(input) => {
            let reply: FactorReply = serde_json::from_value(reply).map_err(bad)?;
            let ring = input.ring().clone();
            let mut constant = Rational::one();
            let mut budget = PowerBudget::for_coefficients(input.terms().map(|(_, c)| c));
            let mut factors = Vec::new();
            for f in &reply.factors {
                let p = polynomial_from(&ring, &f.terms)?;
                if p.is_constant() {
                    let c = p
                        .terms()
                        .next()
                        .map(|(_, c)| c.clone())
                        .unwrap_or_else(Rational::zero);
                    constant = constant
                        .mul_pow_with(&c, f.multiplicity, &mut budget, &mut || {
                            interrupt.checkpoint()
                        })?
                        .map_err(bad)?;
                } else {
                    factors.push((p, f.multiplicity));
                }
            }
            // The product is compared with the input after the backend, by
            // algebramesh-verify.
            let factorization = Factorization::new(ring, constant, factors).map_err(bad)?;
            let value = serde_json::to_value(FactorResult::new(factorization)).map_err(bad)?;
            Ok((value, reply.info.describe()))
        }
        // The value is built by the package; its claims are checked after the
        // backend.
        #[cfg(feature = "pkg-gkz")]
        Operation::Gkz(g) => packages::gkz::interpret(g, reply, interrupt),
        // The value is built by the package from the Smith normal forms; its
        // claims are checked after the backend.
        #[cfg(feature = "pkg-topology")]
        Operation::Topology(t) => packages::topology::interpret(t, reply),
        // Read by the type of its operation; the certificate is checked after
        // the backend.
        Operation::Matrix(m) => {
            let reply: MatrixReply = serde_json::from_value(reply).map_err(bad)?;
            Ok((m.value(&reply.value).map_err(bad)?, reply.info.describe()))
        }
        // Read in the ring of the job by its operation; the basis, the
        // remainder, the membership and their certificates are checked after
        // the backend.
        Operation::Ideal(i, _) => {
            let reply: MatrixReply = serde_json::from_value(reply).map_err(bad)?;
            Ok((i.value(&reply.value).map_err(bad)?, reply.info.describe()))
        }
        // Read in the ring of the job by its operation; the claims are
        // checked after the backend.
        Operation::Module(m, _) => {
            let reply: MatrixReply = serde_json::from_value(reply).map_err(bad)?;
            Ok((m.value(&reply.value).map_err(bad)?, reply.info.describe()))
        }
    }
}

#[async_trait::async_trait]
impl CasBackend for Macaulay2Backend {
    fn name(&self) -> &'static str {
        NAME
    }

    fn driver_version(&self) -> String {
        DRIVER_VERSION.into()
    }

    /// The driver, and with the GKZ package `gkz.m2` and with the topology
    /// package `topology.m2` next to it, which the driver loads.
    fn driver_files(&self) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        if let Some(c) = &self.config {
            files.push(c.driver.clone());
            #[cfg(feature = "pkg-gkz")]
            files.push(c.driver.with_file_name("gkz.m2"));
            #[cfg(feature = "pkg-topology")]
            files.push(c.driver.with_file_name("topology.m2"));
        }
        files
    }

    fn capabilities(&self) -> Capabilities {
        // One M2 process per job, so a per-job limit holds where the platform
        // supports it.
        let enforced_limits = || {
            let mut limits = vec![algebramesh_core::LIMIT_WALL_SECONDS.to_owned()];
            if algebramesh_core::driver::MEMORY_LIMIT_SUPPORTED {
                limits.push(algebramesh_core::LIMIT_MEMORY_MB.to_owned());
            }
            limits
        };
        if self.config.is_none() {
            return Capabilities::default();
        }
        let mut capabilities = Capabilities {
            operations: operations().iter().map(|op| (*op).to_owned()).collect(),
            supported_types: vec![
                "polynomial[ZZ]".into(),
                "polynomial[QQ]".into(),
                "factorization".into(),
                "matrix[ZZ]".into(),
                "matrix[QQ]".into(),
                "ideal[QQ]".into(),
                "groebner_basis".into(),
                "normal_form".into(),
                "ideal_membership".into(),
                "submodule[QQ]".into(),
                "syzygies".into(),
                "free_resolution".into(),
                "hilbert_series".into(),
            ],
            max_precision: None,
            enforced_limits: enforced_limits(),
            ..Capabilities::default()
        };
        #[cfg(feature = "pkg-gkz")]
        capabilities
            .supported_types
            .extend(packages::gkz::TYPES.map(String::from));
        #[cfg(feature = "pkg-topology")]
        capabilities
            .supported_types
            .extend(packages::topology::TYPES.map(String::from));
        for d in operations()
            .iter()
            .filter_map(|op| algebramesh_ops::descriptor(op))
        {
            capabilities.declare(d.name, d.arguments, d.result);
        }
        capabilities
    }

    async fn prepare(&self) -> Result<Option<String>, CasError> {
        let Some(config) = &self.config else {
            return Ok(None);
        };
        let path = config.work_dir.join(format!(
            "algebramesh-m2-info-{}.json",
            algebramesh_core::JobId::new()
        ));
        std::fs::write(&path, r#"{"operation": "system.info"}"#)
            .map_err(|e| CasError::Unavailable(format!("{}: {e}", path.display())))?;
        let command = DriverCommand::new(&config.m2)
            .arg("--script")
            .arg(&config.driver)
            .arg(&path);
        let output = run_driver(
            &command,
            &ExecutionContext::new(Some(std::time::Duration::from_secs(120))),
        )
        .await;
        let _ = std::fs::remove_file(&path);
        let info: DriverInfo = serde_json::from_value(output?.result).map_err(bad)?;
        Ok(Some(info.describe()))
    }

    async fn execute(&self, job: CasRequest, ctx: ExecutionContext) -> Result<CasResult, CasError> {
        let Some(config) = &self.config else {
            return Err(CasError::UnsupportedOperation {
                backend: NAME.into(),
                operation: job.operation,
            });
        };
        // Parsing computes the facets of cone(A) (bounded, Issue #24); it runs
        // under the job's deadline like the result check.
        let op = {
            let job = job.clone();
            check_result(&ctx, move |_| parse(&job)).await?
        };
        let path = config
            .work_dir
            .join(format!("algebramesh-m2-{}.json", job.job_id));
        std::fs::write(&path, request(&op).to_string())
            .map_err(|e| CasError::Unavailable(format!("{}: {e}", path.display())))?;
        let mut command = DriverCommand::new(&config.m2)
            .arg("--script")
            .arg(&config.driver)
            .arg(&path);
        command.memory_limit_mb = job.limits.memory_mb;
        let output = run_driver(&command, &ctx).await;
        let _ = std::fs::remove_file(&path);
        let output = output?;

        // Under the same deadline as the driver (ADR 0016).
        let reply = output.result;
        let checked = tokio::time::Instant::now();
        let (value, version) =
            check_result(&ctx, move |interrupt| interpret(&op, reply, interrupt)).await?;
        let mut log = output.log;
        log.push(format!(
            "M2 returned after {:.3} s",
            output.elapsed.as_secs_f64()
        ));
        log.push(format!(
            "result converted and checked in {:.3} s",
            checked.elapsed().as_secs_f64()
        ));
        Ok(CasResult {
            value,
            backend: BackendInfo {
                name: NAME.into(),
                backend_version: Some(version),
                driver_version: DRIVER_VERSION.into(),
                build: config.build.clone(),
                ..BackendInfo::default()
            },
            execution_host: None,
            log,
            guarantee: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigInt;
    use num_rational::BigRational;

    /// [`super::interpret`] outside a job.
    fn interpret(
        op: &Operation,
        reply: serde_json::Value,
    ) -> Result<(serde_json::Value, String), CasError> {
        super::interpret(op, reply, &Interrupt::none())
    }

    /// `value` checked as the result of a job with `arguments`, as the runner
    /// checks it after the backend (Issue #47).
    fn verify(
        operation: &str,
        arguments: serde_json::Value,
        value: serde_json::Value,
    ) -> Result<CasResult, CasError> {
        use algebramesh_core::guarantee::ResultVerifier;
        let spec = algebramesh_core::JobSpec {
            schema_version: 1,
            backend: NAME.into(),
            operation: operation.into(),
            arguments,
            limits: Default::default(),
            retry_policy: Default::default(),
            environment: Default::default(),
        };
        let result = CasResult {
            value,
            backend: BackendInfo::default(),
            execution_host: None,
            log: vec![],
            guarantee: None,
        };
        algebramesh_verify::Registry::standard().verify(&spec, result, &Interrupt::none())
    }

    fn q(s: &str) -> BigRational {
        s.parse().unwrap()
    }

    fn is_zero(x: &BigRational) -> bool {
        x.numer() == &BigInt::from(0)
    }

    /// The value at `v` of a polynomial given by driver terms.
    fn evaluate(terms: &serde_json::Value, v: &[BigRational]) -> BigRational {
        terms
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let c = q(t["coefficient"].as_str().unwrap());
                let e = t["exponents"].as_array().unwrap();
                e.iter().zip(v).fold(c, |acc, (k, x)| {
                    (0..k.as_u64().unwrap()).fold(acc, |acc, _| acc * x)
                })
            })
            .sum()
    }

    fn job(operation: &str, arguments: serde_json::Value) -> CasRequest {
        CasRequest {
            job_id: algebramesh_core::JobId::new(),
            operation: operation.into(),
            arguments,
            limits: Default::default(),
        }
    }

    #[test]
    fn request_carries_no_names() {
        let op = parse(&job(
            POLYNOMIAL_FACTOR,
            serde_json::json!({
                "ring": {"domain": "QQ", "variables": ["alpha", "beta"], "order": "lex"},
                "expression": {"type": "polynomial", "terms": [
                    {"coefficient": "1/2", "exponents": [2, 0]},
                    {"coefficient": "-1", "exponents": [0, 1]}]}
            }),
        ))
        .unwrap();
        let text = request(&op).to_string();
        assert!(!text.contains("alpha") && !text.contains("beta"));
        assert!(text.contains("\"1/2\""));
    }

    /// Issue #91: a constant factor of multiplicity 2^32 - 1 costs no
    /// multiplication per unit of multiplicity. A constant 1 is absorbed, and
    /// a constant 3, whose power has more than 2^16 bits, is refused; both
    /// within seconds.
    #[test]
    fn a_constant_of_huge_multiplicity_is_absorbed_or_refused_at_once() {
        let op = parse(&job(
            POLYNOMIAL_FACTOR,
            serde_json::json!({
                "ring": {"domain": "QQ", "variables": ["x"], "order": "lex"},
                "expression": {"type": "polynomial", "terms": [
                    {"coefficient": "1", "exponents": [1]}]}
            }),
        ))
        .unwrap();
        let reply = |c: &str| {
            serde_json::json!({
                "factors": [
                    {"terms": [{"coefficient": c, "exponents": [0]}], "multiplicity": u32::MAX},
                    {"terms": [{"coefficient": "1", "exponents": [1]}], "multiplicity": 1}],
                "info": {"macaulay2_version": "fake", "holonomic_systems_version": "fake"}
            })
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let one = interpret(&op, reply("1"));
            let three = interpret(&op, reply("3"));
            let _ = tx.send((one, three));
        });
        let (one, three) = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the constant is not absorbed within 10 s");
        assert_eq!(one.unwrap().0["constant"], "1");
        assert!(
            matches!(&three, Err(CasError::UnsupportedResult(m)) if m.contains("too large")),
            "{three:?}"
        );
    }

    /// Issue #91: Macaulay2 gives the content over ZZ as powers of primes, so
    /// 2^65536 x is answered with the factor 2 of multiplicity 65536. The
    /// power has as many bits as the coefficient, and is accepted.
    #[test]
    fn a_content_over_zz_given_as_a_prime_power_is_accepted() {
        let coefficient = (num_bigint::BigInt::from(1) << 65536u32).to_string();
        let op = parse(&job(
            POLYNOMIAL_FACTOR,
            serde_json::json!({
                "ring": {"domain": "ZZ", "variables": ["x"], "order": "lex"},
                "expression": {"type": "polynomial", "terms": [
                    {"coefficient": coefficient, "exponents": [1]}]}
            }),
        ))
        .unwrap();
        let reply = serde_json::json!({
            "factors": [
                {"terms": [{"coefficient": "2", "exponents": [0]}], "multiplicity": 65536},
                {"terms": [{"coefficient": "1", "exponents": [1]}], "multiplicity": 1}],
            "info": {"macaulay2_version": "fake", "holonomic_systems_version": "fake"}
        });
        let (value, _) = interpret(&op, reply).unwrap();
        assert_eq!(value["constant"], coefficient.as_str());
    }

    /// Issue #35: constants of a ring without variables, with the driver's
    /// replies (the first one recorded from Macaulay2 1.24.11).
    #[test]
    fn constants_of_a_ring_without_variables_round_trip() {
        let factor = |domain: &str, c: &str, reply: serde_json::Value| {
            let input = job(
                POLYNOMIAL_FACTOR,
                serde_json::json!({
                    "ring": {"domain": domain, "variables": [], "order": "lex"},
                    "expression": {"type": "polynomial", "terms": [
                        {"coefficient": c, "exponents": []}]}
                }),
            );
            let op = parse(&input).unwrap();
            interpret(&op, reply)
        };
        let info =
            serde_json::json!({"macaulay2_version": "1.24.11", "holonomic_systems_version": "1.0"});
        let reply =
            |factors: serde_json::Value| serde_json::json!({"factors": factors, "info": info});
        // 2 over ZZ, as Macaulay2 answered.
        let (value, _) = factor(
            "ZZ",
            "2",
            reply(serde_json::json!([
            {"terms": [{"coefficient": "2", "exponents": [0]}], "multiplicity": 1}])),
        )
        .unwrap();
        assert_eq!(value["constant"], "2");
        assert_eq!(value["factors"], serde_json::json!([]));
        assert_eq!(value["verified_product"], true);
        // -6 over ZZ, as -1 * 2 * 3; -1/2 over QQ; a unit.
        let (value, _) = factor(
            "ZZ",
            "-6",
            reply(serde_json::json!([
            {"terms": [{"coefficient": "-1", "exponents": [0]}], "multiplicity": 1},
            {"terms": [{"coefficient": "2", "exponents": [0]}], "multiplicity": 1},
            {"terms": [{"coefficient": "3", "exponents": [0]}], "multiplicity": 1}])),
        )
        .unwrap();
        assert_eq!(value["constant"], "-6");
        let (value, _) = factor(
            "QQ",
            "-1/2",
            reply(serde_json::json!([
            {"terms": [{"coefficient": "-1/2", "exponents": [0]}], "multiplicity": 1}])),
        )
        .unwrap();
        assert_eq!(value["constant"], "-1/2");
        let (value, _) = factor("QQ", "1", reply(serde_json::json!([]))).unwrap();
        assert_eq!(value["constant"], "1");
        // A positive exponent of the driver's own variable is not a constant.
        assert!(matches!(
            factor(
                "ZZ",
                "2",
                reply(serde_json::json!([
                {"terms": [{"coefficient": "2", "exponents": [1]}], "multiplicity": 1}]))
            ),
            Err(CasError::UnsupportedResult(_))
        ));
    }

    #[test]
    fn interprets_factor_reply_and_checks_product() {
        let input = job(
            POLYNOMIAL_FACTOR,
            serde_json::json!({
                "ring": {"domain": "ZZ", "variables": ["x"], "order": "lex"},
                "expression": {"type": "polynomial", "terms": [
                    {"coefficient": "2", "exponents": [2]},
                    {"coefficient": "-2", "exponents": [0]}]}
            }),
        );
        let op = parse(&input).unwrap();
        let reply = serde_json::json!({"factors": [
            {"terms": [{"coefficient": "1", "exponents": [1]}, {"coefficient": "-1", "exponents": [0]}], "multiplicity": 1},
            {"terms": [{"coefficient": "1", "exponents": [1]}, {"coefficient": "1", "exponents": [0]}], "multiplicity": 1},
            {"terms": [{"coefficient": "2", "exponents": [0]}], "multiplicity": 1}],
            "info": {"macaulay2_version": "1.24.11", "holonomic_systems_version": "1.0"}});
        let (value, version) = interpret(&op, reply.clone()).unwrap();
        assert_eq!(value["constant"], "2");
        assert_eq!(value["verified_product"], true);
        assert_eq!(version, "Macaulay2 1.24.11 (HolonomicSystems 1.0)");
        assert!(verify(POLYNOMIAL_FACTOR, input.arguments.clone(), value).is_ok());

        // A wrong product is refused by the check after the backend.
        let mut wrong = reply.clone();
        wrong["factors"][2]["terms"][0]["coefficient"] = "3".into();
        let (value, _) = interpret(&op, wrong).unwrap();
        assert!(matches!(
            verify(POLYNOMIAL_FACTOR, input.arguments.clone(), value),
            Err(CasError::UnsupportedResult(m)) if m.contains("multiply to")
        ));

        // Issue #48: a factor of multiplicity 0 keeps the product, and is
        // refused for its multiplicity.
        let mut mutated = reply;
        mutated["factors"].as_array_mut().unwrap().push(serde_json::json!(
            {"terms": [{"coefficient": "1", "exponents": [1]}, {"coefficient": "3", "exponents": [0]}], "multiplicity": 0}));
        let (value, _) = interpret(&op, mutated).unwrap();
        assert!(matches!(
            verify(POLYNOMIAL_FACTOR, input.arguments, value),
            Err(CasError::UnsupportedResult(m)) if m.contains("multiplicity 0")
        ));
    }

    /// Issue #64: a ring whose order is malformed or does not fit its
    /// variables is refused as invalid arguments when the job is read, before
    /// the driver runs, also for an operation that does not depend on the order.
    #[tokio::test]
    async fn a_ring_with_an_invalid_order_is_refused_before_running() {
        let factor = |order: serde_json::Value| {
            job(
                POLYNOMIAL_FACTOR,
                serde_json::json!({
                    "ring": {"domain": "QQ", "variables": ["x", "y"], "order": order},
                    "expression": {"type": "polynomial", "terms": [
                        {"coefficient": "1", "exponents": [2, 0]},
                        {"coefficient": "-1", "exponents": [0, 1]}]}
                }),
            )
        };
        // Running this executable would fail as unavailable.
        let backend = Macaulay2Backend::new(Macaulay2Config {
            m2: "/nonexistent/M2".into(),
            ..Macaulay2Config::default()
        });
        for (order, why) in [
            (
                serde_json::json!({"weights": [-1, 0], "then": "lex"}),
                "the weight -1 is negative",
            ),
            (
                serde_json::json!({"weights": [1, 0, 0], "then": "lex"}),
                "3 weights for 2 variables",
            ),
            (
                serde_json::json!({"block": [{"count": 1, "order": "lex"}]}),
                "the blocks do not cover the 2 variables",
            ),
        ] {
            let input = factor(order);
            assert!(
                matches!(parse(&input), Err(CasError::InvalidArguments(ref m)) if m.contains(why)),
                "{why}"
            );
            let e = backend
                .execute(input, ExecutionContext::new(None))
                .await
                .unwrap_err();
            assert!(
                matches!(e, CasError::InvalidArguments(ref m) if m.contains(why)),
                "{e:?}"
            );
        }
        // With a valid order the job is read, and its request does not change:
        // factoring does not depend on the order.
        let valid = factor(serde_json::json!({"block": [
            {"count": 1, "order": "grevlex"}, {"count": 1, "order": "lex"}]}));
        assert_eq!(
            request(&parse(&valid).unwrap()),
            request(&parse(&factor(serde_json::json!("lex"))).unwrap())
        );
    }

    fn block(parts: &[(usize, BaseOrder)]) -> MonomialOrder {
        MonomialOrder::Block(
            parts
                .iter()
                .map(|&(count, order)| algebramesh_math_types::OrderBlock { count, order })
                .collect(),
        )
    }

    fn weights(weights: &[u32], then: BaseOrder) -> MonomialOrder {
        MonomialOrder::Weights {
            weights: weights.to_vec(),
            then,
        }
    }

    /// Issue #64: the items of each kind of order, as the driver receives them.
    #[test]
    fn orders_become_macaulay2_items() {
        use BaseOrder::{Grevlex, Grlex, Lex};
        let items = |order: MonomialOrder, nvars: usize| {
            serde_json::to_value(order_items(&order, nvars).unwrap()).unwrap()
        };
        assert_eq!(
            items(MonomialOrder::Lex, 3),
            serde_json::json!([["Lex", 3]])
        );
        assert_eq!(
            items(MonomialOrder::Grlex, 3),
            serde_json::json!([["Weights", [1, 1, 1]], ["Lex", 3]])
        );
        assert_eq!(
            items(MonomialOrder::Grevlex, 3),
            serde_json::json!([["GRevLex", 3]])
        );
        assert_eq!(
            items(weights(&[3, 0, MAX_WEIGHT], Grevlex), 3),
            serde_json::json!([["Weights", [3, 0, 2147483647]], ["GRevLex", 3]])
        );
        assert_eq!(
            items(weights(&[0, 0, 1], Grlex), 3),
            serde_json::json!([["Weights", [0, 0, 1]], ["Weights", [1, 1, 1]], ["Lex", 3]])
        );
        assert_eq!(
            items(block(&[(1, Grevlex), (2, Grlex), (1, Lex)]), 4),
            serde_json::json!([["GRevLex", 1], ["Weights", [1, 1]], ["Lex", 2], ["Lex", 1]])
        );
        // A ring without variables: the driver's own variable.
        assert_eq!(
            items(MonomialOrder::Grevlex, 0),
            serde_json::json!([["Lex", 1]])
        );
        assert_eq!(
            items(weights(&[], Grlex), 0),
            serde_json::json!([["Lex", 1]])
        );
    }

    /// Issue #64: read as Macaulay2 reads them, the items order the monomials
    /// as `MonomialOrder::compare` does. A `Weights` item weighs the variables
    /// from the first one not yet in a block on (as checked in Macaulay2
    /// 1.24.11), `Lex => k` and `GRevLex => k` take the next `k` variables.
    #[test]
    fn macaulay2_items_order_monomials_as_compare_does() {
        use BaseOrder::{Grevlex, Grlex, Lex};
        fn rows(items: &[OrderItem], n: usize) -> Vec<Vec<i128>> {
            let unit = |i: usize, v: i128| {
                let mut row = vec![0i128; n];
                row[i] = v;
                row
            };
            let mut rows = Vec::new();
            // The first variable not yet in a block.
            let mut next = 0;
            for item in items {
                match item {
                    OrderItem::Weights(w) => {
                        let mut row = vec![0i128; n];
                        for (i, &wi) in w.iter().enumerate() {
                            row[next + i] = i128::from(wi);
                        }
                        rows.push(row);
                    }
                    OrderItem::Lex(k) => {
                        rows.extend((next..next + k).map(|i| unit(i, 1)));
                        next += k;
                    }
                    OrderItem::GRevLex(k) => {
                        let mut ones = vec![0i128; n];
                        for e in &mut ones[next..next + k] {
                            *e = 1;
                        }
                        rows.push(ones);
                        rows.extend((next..next + k).rev().map(|i| unit(i, -1)));
                        next += k;
                    }
                }
            }
            assert_eq!(next, n, "the blocks cover the variables");
            rows
        }
        let n = 4;
        // Every monomial with exponents below 3.
        let monomials: Vec<Vec<u32>> = (0..81u32)
            .map(|m| (0..4).map(|i| m / 3u32.pow(i) % 3).collect())
            .collect();
        let orders = [
            MonomialOrder::Lex,
            MonomialOrder::Grlex,
            MonomialOrder::Grevlex,
            weights(&[3, 0, 1, 2], Grevlex),
            weights(&[0, 0, 0, 0], Lex),
            weights(&[1, 1, 1, 1], Lex),
            weights(&[0, 1, 0, 1], Grlex),
            weights(&[MAX_WEIGHT, 0, 1, 2], Grlex),
            block(&[(2, Grevlex), (2, Lex)]),
            block(&[(1, Lex), (3, Grlex)]),
            block(&[(2, Grlex), (2, Grlex)]),
            block(&[(3, Grevlex), (1, Grevlex)]),
            block(&[(1, Grevlex), (2, Grlex), (1, Lex)]),
            block(&[(1, Grlex), (1, Grlex), (1, Grlex), (1, Grlex)]),
        ];
        for order in &orders {
            let m = rows(&order_items(order, n).unwrap(), n);
            let images: Vec<Vec<i128>> = monomials
                .iter()
                .map(|e| {
                    m.iter()
                        .map(|row| row.iter().zip(e).map(|(w, &x)| w * i128::from(x)).sum())
                        .collect()
                })
                .collect();
            for (a, image_a) in monomials.iter().zip(&images) {
                for (b, image_b) in monomials.iter().zip(&images) {
                    assert_eq!(
                        image_a.cmp(image_b),
                        order.compare(a, b),
                        "{order:?}: {a:?} and {b:?}"
                    );
                }
            }
        }
    }

    /// Issue #64: an order that is not valid for the variables cannot be
    /// written for Macaulay2 and is refused as invalid arguments.
    #[test]
    fn orders_macaulay2_cannot_take_are_refused() {
        use BaseOrder::Lex;
        let refused = |order: MonomialOrder, nvars: usize| {
            matches!(
                order_items(&order, nvars),
                Err(CasError::InvalidArguments(_))
            )
        };
        assert!(refused(weights(&[1, 0], Lex), 3));
        assert!(refused(weights(&[1, 0, 0, 0], Lex), 3));
        // A weight above MAX_WEIGHT, which only Rust can build.
        assert!(refused(weights(&[MAX_WEIGHT + 1, 0], Lex), 2));
        assert!(refused(block(&[(2, Lex)]), 3));
        assert!(refused(block(&[(0, Lex), (2, Lex)]), 2));
        assert!(refused(MonomialOrder::Block(vec![]), 0));
    }

    /// backend reads it and checked as the runner checks it.
    fn check_matrix_reply(
        operation: &str,
        arguments: &serde_json::Value,
        reply: &serde_json::Value,
    ) -> Result<CasResult, CasError> {
        let op = parse(&job(operation, arguments.clone()))?;
        let (value, version) = interpret(&op, reply.clone())?;
        assert_eq!(version, "Macaulay2 1.24.11 (HolonomicSystems 1.0)");
        verify(operation, arguments.clone(), value)
    }

    /// Issues #57, #58: the replies Macaulay2 gave in its image
    /// (`testdata/matrix.json`) pass every check; each mutation, first shown
    /// to break its claim, is refused.
    #[test]
    fn recorded_matrix_replies_are_checked() {
        use algebramesh_core::guarantee::EvidenceLevel;
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../testdata/matrix.json")).unwrap();
        for case in &cases {
            let op = case["operation"].as_str().unwrap();
            let checked = check_matrix_reply(op, &case["arguments"], &case["reply"]).unwrap();
            let claims = checked.guarantee.unwrap().claims;
            assert!(
                claims.iter().all(|c| c.level == EvidenceLevel::Checked),
                "{op}: {claims:?}"
            );
        }
        let mutated = |i: usize, pointer: &str, to: serde_json::Value| {
            let case = &cases[i];
            let mut reply = case["reply"].clone();
            *reply.pointer_mut(pointer).unwrap() = to;
            check_matrix_reply(
                case["operation"].as_str().unwrap(),
                &case["arguments"],
                &reply,
            )
        };
        let product = |a: serde_json::Value, b: serde_json::Value| {
            let a: algebramesh_math_types::RationalMatrix = serde_json::from_value(a).unwrap();
            let b: algebramesh_math_types::RationalMatrix = serde_json::from_value(b).unwrap();
            a.mul(&b).unwrap()
        };
        let refused = |r: Result<CasResult, CasError>| {
            assert!(matches!(r, Err(CasError::UnsupportedResult(_))), "{r:?}")
        };
        // SNF: diag(2, 8) keeps 2 | 8, but P A Q is diag(2, 4).
        refused(mutated(0, "/value/diagonal/1", serde_json::json!("8")));
        // HNF: [[2, 0], [0, 8]] is a Hermite form, but U A is [[2, 0], [0, 4]].
        refused(mutated(1, "/value/form/1/1", serde_json::json!("8")));
        // rank: (1, 1, 1) is not in the kernel.
        assert!(
            !product(
                cases[2]["arguments"]["matrix"].clone(),
                serde_json::json!([["1"], ["1"], ["1"]])
            )
            .is_zero()
        );
        refused(mutated(
            2,
            "/value/certificate/kernel/2/0",
            serde_json::json!("1"),
        ));
        // kernel over ZZ: (-4, 2) lies in the kernel but spans half the lattice.
        assert!(
            product(
                serde_json::json!([["2", "4"]]),
                serde_json::json!([["-4"], ["2"]])
            )
            .is_zero()
        );
        refused(mutated(
            4,
            "/value/basis",
            serde_json::json!([["-4"], ["2"]]),
        ));
        // solve over ZZ: y = 1 keeps y A integral, but y b = 1 is an integer.
        refused(mutated(5, "/value/certificate/y/0", serde_json::json!("1")));
        // solve over QQ: y = (1, 1) is not in the left kernel.
        assert!(
            !product(
                serde_json::json!([["1", "1"]]),
                cases[7]["arguments"]["matrix"].clone()
            )
            .is_zero()
        );
        refused(mutated(
            7,
            "/value/certificate/y",
            serde_json::json!(["1", "1"]),
        ));
    }

    /// Issue #57: Macaulay2 and OSCAR give the same Hermite and Smith forms,
    /// which are unique.
    #[test]
    fn the_normal_forms_agree_with_oscar() {
        let ours: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../testdata/matrix.json")).unwrap();
        let oscar: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../algebramesh-backend-oscar/testdata/matrix.json"
        ))
        .unwrap();
        for (op, key) in [
            ("matrix.smith_normal_form", "diagonal"),
            ("matrix.hermite_normal_form", "form"),
        ] {
            let find = |cases: &[serde_json::Value], value: &str| {
                cases
                    .iter()
                    .find(|c| c["operation"] == op)
                    .map(|c| c["reply"].pointer(value).unwrap().clone())
                    .unwrap()
            };
            assert_eq!(
                find(&ours, &format!("/value/{key}")),
                find(&oscar, &format!("/{key}")),
                "{op}"
            );
        }
    }

    /// The cases of `testdata/ideal.json`: replies Macaulay2 gave in its image.
    fn ideal_cases() -> Vec<serde_json::Value> {
        serde_json::from_str(include_str!("../testdata/ideal.json")).unwrap()
    }

    fn ideal_case(name: &str) -> serde_json::Value {
        ideal_cases()
            .into_iter()
            .find(|c| c["name"] == name)
            .unwrap()
    }

    /// The recorded reply that is wrong on purpose: the one of the driver
    /// run with the grevlex order items for a lex job (Issue #68,
    /// acceptance 4).
    const WRONG_ORDER_REPLY: &str = "mixup_lex_answered_in_grevlex";

    /// Issues #66, #67, #68: the replies Macaulay2 gave in its image
    /// (`testdata/ideal.json`) pass every check, with the levels of
    /// `docs/claims.md`. `ideal.groebner_basis`: lex, grevlex, grlex, a
    /// weight order, block orders, the unit and the zero ideal, a zero
    /// generator, a coefficient beyond 64 bits. `ideal.normal_form` and
    /// `ideal.membership`: the twisted cubic with f in I and f not in I, in
    /// lex and grevlex, normal forms of one f in lex and grevlex, the zero
    /// ideal (with and without generators), the unit ideal, a zero generator,
    /// coefficients beyond 64 bits. The cases named `..._s_pairs` ask for the
    /// certificate of the S-pairs. The claims a certificate supports are
    /// asserted when the job asked for none; `gb.groebner` is checked with
    /// the S-pairs and asserted without, and so in effect are
    /// `normal_form.unique` and `membership.not_in`, which are checked on
    /// their own.
    #[test]
    fn recorded_ideal_replies_are_checked() {
        use algebramesh_core::guarantee::EvidenceLevel::{self, Asserted, Checked};
        let mut operations = std::collections::BTreeSet::new();
        let mut with_s_pairs = 0;
        for case in &ideal_cases() {
            let name = case["name"].as_str().unwrap();
            if name == WRONG_ORDER_REPLY {
                continue;
            }
            let operation = case["operation"].as_str().unwrap();
            operations.insert(operation.to_owned());
            let checked = check_matrix_reply(operation, &case["arguments"], &case["reply"])
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            let certificate = case["arguments"]["certificate"].as_str().unwrap();
            let with_certificate = certificate != "none";
            let c = if with_certificate { Checked } else { Asserted };
            let g = if certificate == "s_pairs" {
                with_s_pairs += 1;
                Checked
            } else {
                Asserted
            };
            // Each claim with its own level and its effective level.
            let expected: Vec<(&str, EvidenceLevel, EvidenceLevel)> =
                match (operation, case["reply"]["value"]["member"].as_bool()) {
                    (ideal::IDEAL_GROEBNER_BASIS, _) => vec![
                        ("gb.reduced_form", Checked, Checked),
                        ("gb.contained", c, c),
                        ("gb.contains", c, c),
                        ("gb.groebner", g, g),
                    ],
                    (ideal::IDEAL_NORMAL_FORM, _) => vec![
                        ("gb.reduced_form", Checked, Checked),
                        ("gb.contained", c, c),
                        ("gb.contains", c, c),
                        ("gb.groebner", g, g),
                        ("normal_form.reduced", c, c),
                        ("normal_form.unique", Checked, g),
                    ],
                    (ideal::IDEAL_MEMBERSHIP, Some(true)) => vec![("membership.in", c, c)],
                    (ideal::IDEAL_MEMBERSHIP, Some(false)) if with_certificate => vec![
                        ("gb.reduced_form", Checked, Checked),
                        ("gb.contains", Checked, Checked),
                        ("gb.groebner", g, g),
                        ("normal_form.reduced", Checked, Checked),
                        ("membership.not_in", Checked, g),
                    ],
                    (ideal::IDEAL_MEMBERSHIP, Some(false)) => {
                        vec![("membership.not_in", Asserted, Asserted)]
                    }
                    _ => panic!("{name}: {operation}"),
                };
            let guarantee = checked.guarantee.unwrap();
            let claims: Vec<(&str, EvidenceLevel, EvidenceLevel)> = guarantee
                .claims
                .iter()
                .map(|c| (c.id.as_str(), c.level, guarantee.effective_level(&c.id)))
                .collect();
            assert_eq!(claims, expected, "{name}");
        }
        assert_eq!(with_s_pairs, 19);
        let mut offered = ideal::OPERATIONS.map(String::from).to_vec();
        offered.sort();
        assert_eq!(operations.into_iter().collect::<Vec<_>>(), offered);
    }

    /// Issue #66: one changed entry of C, of N or of the basis is refused;
    /// so is the grevlex basis sent for a lex job, which is not monic in lex.
    #[test]
    fn changed_ideal_replies_are_refused() {
        let refused = |case: &serde_json::Value, reply: &serde_json::Value| {
            let r = check_matrix_reply(ideal::IDEAL_GROEBNER_BASIS, &case["arguments"], reply);
            assert!(matches!(r, Err(CasError::UnsupportedResult(_))), "{r:?}");
        };
        let mutated = |name: &str, pointer: &str, to: serde_json::Value| {
            let case = ideal_case(name);
            let mut reply = case["reply"].clone();
            *reply.pointer_mut(pointer).unwrap() = to;
            refused(&case, &reply);
        };
        // The twisted cubic: g_0 = -f_2 and f_1 = -g_1; the sign of either
        // entry changed gives f_2 = z^2 - yw and g_1 = yz - xw, which are not
        // -g_0 and -f_1.
        mutated(
            "twisted_cubic_grevlex",
            "/value/certificate/basis_in_generators/columns/0/0/terms/0/coefficient",
            serde_json::json!("1"),
        );
        mutated(
            "twisted_cubic_grevlex",
            "/value/certificate/generators_in_basis/columns/1/0/terms/0/coefficient",
            serde_json::json!("1"),
        );
        // Each element of the basis is monic.
        mutated(
            "small_lex",
            "/value/basis/2/0/coefficient",
            serde_json::json!("2"),
        );
        // A certificate the job did not ask for, and none where it did.
        let (none, given) = (
            ideal_case("twisted_cubic_grevlex_none"),
            ideal_case("twisted_cubic_grevlex"),
        );
        refused(&none, &given["reply"]);
        refused(&given, &none["reply"]);
        // The grevlex basis of F = {x^2 y - z/2, 2 x y^2 - x, 3 x z - y} for
        // the lex job: z^2 - x/3 leads with -x/3 in lex.
        refused(
            &ideal_case("small_lex"),
            &ideal_case("small_grevlex")["reply"],
        );
    }

    /// The exponent vector of the first term of a polynomial as the driver
    /// writes it, which is its leading monomial in the order the driver used.
    fn first_exponents(terms: &serde_json::Value) -> Vec<u64> {
        terms[0]["exponents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_u64().unwrap())
            .collect()
    }

    /// The pairs (i, j), i < j, of a basis as the driver writes it whose
    /// leading monomials (the first terms) are not coprime, in increasing
    /// lexicographic order.
    fn non_coprime_pairs(basis: &serde_json::Value) -> Vec<[usize; 2]> {
        let leading: Vec<Vec<u64>> = basis
            .as_array()
            .unwrap()
            .iter()
            .map(first_exponents)
            .collect();
        let t = leading.len();
        (0..t)
            .flat_map(|i| (i + 1..t).map(move |j| [i, j]))
            .filter(|&[i, j]| {
                leading[i]
                    .iter()
                    .zip(&leading[j])
                    .any(|(a, b)| *a > 0 && *b > 0)
            })
            .collect()
    }

    /// The value at `v` of S(g_i, g_j) = (L / lm(g_i)) g_i - (L / lm(g_j)) g_j,
    /// L = lcm(lm(g_i), lm(g_j)), for monic g_i and g_j of `basis` whose first
    /// terms lead.
    fn s_pair_value(
        basis: &serde_json::Value,
        i: usize,
        j: usize,
        v: &[BigRational],
    ) -> BigRational {
        let (gi, gj) = (&basis[i], &basis[j]);
        let (a, b) = (first_exponents(gi), first_exponents(gj));
        let monomial = |e: Vec<u64>| {
            e.iter()
                .zip(v)
                .fold(q("1"), |acc, (k, x)| (0..*k).fold(acc, |acc, _| acc * x))
        };
        let quotient = |m: &[u64]| -> Vec<u64> {
            a.iter()
                .zip(&b)
                .zip(m)
                .map(|((x, y), z)| x.max(y) - z)
                .collect()
        };
        monomial(quotient(&a)) * evaluate(gi, v) - monomial(quotient(&b)) * evaluate(gj, v)
    }

    /// The point (2, 3, 5, 7) cut to `n` variables.
    fn point(n: usize) -> Vec<BigRational> {
        twisted_point()[..n].to_vec()
    }

    /// Issue #68: the certificates of the S-pairs Macaulay2 gave. Each reply
    /// with them is the reply of the same job asked with `representation`
    /// with `s_pairs` added; a member keeps its coefficients alone. The pairs
    /// are those whose leading monomials are not coprime, in increasing order,
    /// and S(g_i, g_j) = sum_k g_k H_kp holds at a point: the twisted cubic
    /// (z^2 - yw, yz - xw, y^2 - xz) has S(g_0, g_1) = -w g_2 and S(g_1, g_2) =
    /// x g_0, with (0, 2) coprime (z^2, y^2); cyclic 4 in grevlex 13 of its 21
    /// pairs; the lex basis (z^7 - z/162, y - 9 z^3, x - 3 z^2), the unit
    /// ideal, the zero ideal and the basis (x^2 - y^2) of one element none.
    #[test]
    fn recorded_s_pairs_certificates() {
        let cases = ideal_cases();
        let mut seen = 0;
        for case in &cases {
            let name = case["name"].as_str().unwrap();
            if case["arguments"]["certificate"] != "s_pairs" {
                continue;
            }
            seen += 1;
            let value = &case["reply"]["value"];
            if let Some(twin) = name.strip_suffix("_s_pairs")
                && let Some(twin) = cases.iter().find(|c| c["name"] == twin)
            {
                let mut without = case["reply"].clone();
                if let Some(c) = without["value"]["certificate"].as_object_mut() {
                    c.remove("s_pairs");
                }
                assert_eq!(without, twin["reply"], "{name}");
            }
            if value["member"] == true {
                assert!(value["certificate"].get("s_pairs").is_none(), "{name}");
                continue;
            }
            let (basis, s_pairs) = if value["member"] == false {
                (
                    &value["certificate"]["basis"],
                    &value["certificate"]["s_pairs"],
                )
            } else {
                (&value["basis"], &value["certificate"]["s_pairs"])
            };
            let pairs: Vec<[usize; 2]> = serde_json::from_value(s_pairs["pairs"].clone()).unwrap();
            assert_eq!(pairs, non_coprime_pairs(basis), "{name}");
            let h = &s_pairs["representations"];
            let g = basis.as_array().unwrap();
            assert_eq!(h["rows"], g.len(), "{name}");
            assert_eq!(
                h["columns"].as_array().unwrap().len(),
                pairs.len(),
                "{name}"
            );
            let n = case["arguments"]["ideal"]["ring"]["variables"]
                .as_array()
                .unwrap()
                .len();
            let v = point(n);
            let gh = row_times(g, h, &v);
            for (p, &[i, j]) in pairs.iter().enumerate() {
                assert_eq!(gh[p], s_pair_value(basis, i, j, &v), "{name}: pair {p}");
            }
        }
        assert_eq!(seen, 20);

        let s_pairs =
            |name: &str| ideal_case(name)["reply"]["value"]["certificate"]["s_pairs"].clone();
        let x = |c: &str, e: [u32; 4]| serde_json::json!({"coefficient": c, "exponents": e});
        assert_eq!(
            s_pairs("twisted_cubic_grevlex_s_pairs"),
            serde_json::json!({"pairs": [[0, 1], [1, 2]], "representations": {"rows": 3, "columns": [
                [{"row": 2, "terms": [x("-1", [0, 0, 0, 1])]}],
                [{"row": 0, "terms": [x("1", [1, 0, 0, 0])]}]
            ]}})
        );
        assert_eq!(
            s_pairs("cyclic4_grevlex_s_pairs")["pairs"]
                .as_array()
                .unwrap()
                .len(),
            13
        );
        assert_eq!(
            ideal_case("cyclic4_grevlex_s_pairs")["reply"]["value"]["basis"]
                .as_array()
                .unwrap()
                .len(),
            7
        );
        for (name, t) in [
            ("small_lex_s_pairs", 3),
            ("unit_ideal_s_pairs", 1),
            ("zero_ideal_s_pairs", 0),
            ("one_element_s_pairs", 1),
        ] {
            assert_eq!(
                s_pairs(name),
                serde_json::json!({"pairs": [], "representations": {"rows": t, "columns": []}}),
                "{name}"
            );
        }
    }

    /// Issue #68: a certificate of the S-pairs whose data is wrong is
    /// refused, each change first shown at the point (2, 3, 5, 7): with G =
    /// (z^2 - yw, yz - xw, y^2 - xz) and H = (-w e_2 | x e_0), a changed h
    /// breaks S(g_0, g_1) = sum_k g_k H_k0; the representation straight from
    /// the definition, S(g_0, g_1) = y g_0 - z g_1, holds but lm(y g_0) = yz^2
    /// is L itself; a pair left out, a coprime pair listed, the pairs
    /// reordered; the S-pairs missing where the job asked for them or given
    /// where it did not; and a changed h in a normal form and in a
    /// certificate of f not in I.
    #[test]
    fn changed_s_pair_replies_are_refused() {
        let refused = |case: &serde_json::Value, reply: &serde_json::Value, why: &str| {
            let operation = case["operation"].as_str().unwrap();
            let r = check_matrix_reply(operation, &case["arguments"], reply);
            assert!(
                matches!(&r, Err(CasError::UnsupportedResult(m)) if m.contains(why)),
                "{why}: {r:?}"
            );
        };
        let case = ideal_case("twisted_cubic_grevlex_s_pairs");
        let reply = &case["reply"];
        let basis = &reply["value"]["basis"];
        let g = basis.as_array().unwrap();
        let v = twisted_point();
        let h = |reply: &serde_json::Value| {
            reply["value"]["certificate"]["s_pairs"]["representations"].clone()
        };
        let t = |c: &str, e: [u32; 4]| serde_json::json!([{"coefficient": c, "exponents": e}]);
        assert_eq!(
            row_times(g, &h(reply), &v)[0],
            s_pair_value(basis, 0, 1, &v)
        );

        // -w made w.
        let changed_h = changed(
            reply,
            &[(
                "/value/certificate/s_pairs/representations/columns/0/0/terms/0/coefficient",
                serde_json::json!("1"),
            )],
        );
        assert_ne!(
            row_times(g, &h(&changed_h), &v)[0],
            s_pair_value(basis, 0, 1, &v)
        );
        refused(&case, &changed_h, "S(g_0, g_1) is not sum_k g_k H_k0");

        // h_0 = L / lm(g_0) = y, h_1 = -L / lm(g_1) = -z: the equation holds.
        let definition = changed(
            reply,
            &[(
                "/value/certificate/s_pairs/representations/columns/0",
                serde_json::json!([
                    {"row": 0, "terms": t("1", [0, 1, 0, 0])},
                    {"row": 1, "terms": t("-1", [0, 0, 1, 0])}
                ]),
            )],
        );
        assert_eq!(
            row_times(g, &h(&definition), &v)[0],
            s_pair_value(basis, 0, 1, &v)
        );
        refused(
            &case,
            &definition,
            "H_00 gives no lcm representation of S(g_0, g_1)",
        );

        // The pair (1, 2) left out.
        let left_out = changed(
            reply,
            &[
                (
                    "/value/certificate/s_pairs/pairs",
                    serde_json::json!([[0, 1]]),
                ),
                (
                    "/value/certificate/s_pairs/representations/columns",
                    serde_json::json!([h(reply)["columns"][0]]),
                ),
            ],
        );
        refused(&case, &left_out, "s_pairs.pairs lacks the pair (1, 2)");

        // (0, 2) listed, z^2 and y^2 coprime, with an empty column: the list
        // is refused before H is read.
        let coprime = changed(
            reply,
            &[
                (
                    "/value/certificate/s_pairs/pairs",
                    serde_json::json!([[0, 1], [0, 2], [1, 2]]),
                ),
                (
                    "/value/certificate/s_pairs/representations/columns",
                    serde_json::json!([h(reply)["columns"][0], [], h(reply)["columns"][1]]),
                ),
            ],
        );
        refused(
            &case,
            &coprime,
            "s_pairs.pairs[1] = (0, 2): the leading monomials of g_0 and g_2 are coprime",
        );

        // Both pairs with their columns, in the other order.
        let reordered = changed(
            reply,
            &[
                (
                    "/value/certificate/s_pairs/pairs",
                    serde_json::json!([[1, 2], [0, 1]]),
                ),
                (
                    "/value/certificate/s_pairs/representations/columns",
                    serde_json::json!([h(reply)["columns"][1], h(reply)["columns"][0]]),
                ),
            ],
        );
        refused(&case, &reordered, "s_pairs.pairs[0] is (1, 2), not (0, 1)");

        // The S-pairs missing where the job asked for them, and given where
        // it did not.
        let representation = ideal_case("twisted_cubic_grevlex");
        refused(&case, &representation["reply"], ideal::S_PAIRS_MISSING);
        refused(&representation, reply, ideal::S_PAIRS_UNREQUESTED);

        // A normal form and a certificate of f not in I with -w made w: the
        // same G and H as above, so the same equation breaks.
        for (name, basis_at) in [
            ("normal_form_twisted_cubic_s_pairs", "/value/basis"),
            (
                "membership_twisted_cubic_not_in_s_pairs",
                "/value/certificate/basis",
            ),
        ] {
            let case = ideal_case(name);
            let reply = &case["reply"];
            assert_eq!(reply.pointer(basis_at), Some(basis));
            assert_eq!(
                h(reply),
                h(&ideal_case("twisted_cubic_grevlex_s_pairs")["reply"])
            );
            let changed_h = changed(
                reply,
                &[(
                    "/value/certificate/s_pairs/representations/columns/0/0/terms/0/coefficient",
                    serde_json::json!("1"),
                )],
            );
            refused(&case, &changed_h, "S(g_0, g_1) is not sum_k g_k H_k0");
        }
    }

    /// Issue #68, acceptance 4: the lex job F = (-xyz + z^2, -xyz - x^2 +
    /// xy) answered by the driver run with the grevlex order items. Its reply
    /// is the grevlex one, G = (x^2 - xy + z^2, xyz - z^2, yz^3 + xz^2 - yz^2)
    /// with the grevlex S-pairs (0, 1) and (1, 2). As sent it is refused (not
    /// in increasing lex order). Put in increasing lex order (lex leading
    /// monomials xz^2 < xyz < x^2), with C, N, H and the pairs renumbered, it
    /// passes with `representation`, `gb.groebner` asserted; with the S-pairs
    /// it is refused at `gb.groebner`: every lex leading monomial has x, so
    /// all three pairs are listed in lex and the grevlex list lacks (0, 2).
    /// It is no lex Groebner basis: the lex reply holds y^2 z^3 - y^2 z^2 +
    /// z^3, an element of I whose lex leading monomial no lex leading
    /// monomial of G divides.
    #[test]
    fn the_grevlex_reply_to_a_lex_job_is_refused_by_its_s_pairs() {
        let refused = |arguments: &serde_json::Value, reply: &serde_json::Value, why: &str| {
            let r = check_matrix_reply(ideal::IDEAL_GROEBNER_BASIS, arguments, reply);
            assert!(
                matches!(&r, Err(CasError::UnsupportedResult(m)) if m.contains(why)),
                "{why}: {r:?}"
            );
        };
        let case = ideal_case(WRONG_ORDER_REPLY);
        let arguments = &case["arguments"];
        assert_eq!(arguments["ideal"]["ring"]["order"], "lex");
        assert_eq!(arguments["certificate"], "s_pairs");
        assert_eq!(
            arguments["ideal"],
            ideal_case("mixup_lex_s_pairs")["arguments"]["ideal"]
        );
        let reply = &case["reply"];
        assert_eq!(reply, &ideal_case("mixup_grevlex_s_pairs")["reply"]);
        assert_eq!(
            reply["value"]["certificate"]["s_pairs"]["pairs"],
            serde_json::json!([[0, 1], [1, 2]])
        );
        refused(
            arguments,
            reply,
            "the leading monomials of g_0 and g_1 are not in strictly increasing order",
        );

        // In increasing lex order: g_k becomes g_(2-k), the pair (0, 1)
        // becomes (1, 2) and (1, 2) becomes (0, 1).
        let mut sorted = reply.clone();
        let value = &mut sorted["value"];
        let reverse = |v: &mut serde_json::Value| v.as_array_mut().unwrap().reverse();
        let renumber_rows = |m: &mut serde_json::Value| {
            for column in m["columns"].as_array_mut().unwrap() {
                for entry in column.as_array_mut().unwrap() {
                    entry["row"] = (2 - entry["row"].as_u64().unwrap()).into();
                }
            }
        };
        reverse(&mut value["basis"]);
        reverse(&mut value["certificate"]["basis_in_generators"]["columns"]);
        renumber_rows(&mut value["certificate"]["generators_in_basis"]);
        let s_pairs = &mut value["certificate"]["s_pairs"];
        renumber_rows(&mut s_pairs["representations"]);
        reverse(&mut s_pairs["representations"]["columns"]);
        let lex_leading = |p: &serde_json::Value| {
            p.as_array()
                .unwrap()
                .iter()
                .map(|t| serde_json::from_value::<Vec<u32>>(t["exponents"].clone()).unwrap())
                .max()
                .unwrap()
        };
        let leading: Vec<Vec<u32>> = value["basis"]
            .as_array()
            .unwrap()
            .iter()
            .map(lex_leading)
            .collect();
        assert_eq!(leading, [vec![1, 0, 2], vec![1, 1, 1], vec![2, 0, 0]]);
        assert!(leading.iter().all(|m| m[0] > 0));

        let mut representation = arguments.clone();
        representation["certificate"] = "representation".into();
        let mut without = sorted.clone();
        without["value"]["certificate"]
            .as_object_mut()
            .unwrap()
            .remove("s_pairs");
        let checked =
            check_matrix_reply(ideal::IDEAL_GROEBNER_BASIS, &representation, &without).unwrap();
        let groebner = checked
            .guarantee
            .unwrap()
            .claims
            .into_iter()
            .find(|c| c.id == "gb.groebner")
            .unwrap();
        assert_eq!(
            groebner.level,
            algebramesh_core::guarantee::EvidenceLevel::Asserted
        );
        refused(arguments, &sorted, "s_pairs.pairs[1] is (1, 2), not (0, 2)");

        // y^2 z^3 - y^2 z^2 + z^3 in the checked lex reply.
        let lex = &ideal_case("mixup_lex_s_pairs")["reply"]["value"]["basis"][0];
        assert_eq!(lex_leading(lex), [0, 2, 3]);
        assert!(
            leading
                .iter()
                .all(|m| m.iter().zip([0, 2, 3]).any(|(a, b)| *a > b))
        );
    }

    /// Driver terms as a set, to compare polynomials whatever the order of
    /// their terms.
    fn term_set(terms: &serde_json::Value) -> std::collections::BTreeSet<String> {
        terms
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.to_string())
            .collect()
    }

    /// The point (2, 3, 5, 7) for the four variables of the twisted cubic.
    fn twisted_point() -> Vec<BigRational> {
        ["2", "3", "5", "7"].map(q).to_vec()
    }

    /// The value at `v` of f - r - sum_k q_k g_k, from driver terms.
    fn division_defect(
        f: &serde_json::Value,
        r: &serde_json::Value,
        quotients: &serde_json::Value,
        basis: &serde_json::Value,
        v: &[BigRational],
    ) -> BigRational {
        let qs = quotients.as_array().unwrap();
        let gs = basis.as_array().unwrap();
        assert_eq!(qs.len(), gs.len());
        let sum: BigRational = qs
            .iter()
            .zip(gs)
            .map(|(q, g)| evaluate(q, v) * evaluate(g, v))
            .sum();
        evaluate(f, v) - evaluate(r, v) - sum
    }

    /// The entry (i, j) of a driver matrix, as driver terms.
    fn matrix_entry(m: &serde_json::Value, i: usize, j: usize) -> serde_json::Value {
        m["columns"][j]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["row"] == i)
            .map_or(serde_json::json!([]), |e| e["terms"].clone())
    }

    /// The values at `v` of the entries of the row a M, for the driver
    /// matrix `m` and `a` given by driver terms.
    fn row_times(
        a: &[serde_json::Value],
        m: &serde_json::Value,
        v: &[BigRational],
    ) -> Vec<BigRational> {
        (0..m["columns"].as_array().unwrap().len())
            .map(|j| {
                a.iter()
                    .enumerate()
                    .map(|(i, p)| evaluate(p, v) * evaluate(&matrix_entry(m, i, j), v))
                    .sum()
            })
            .collect()
    }

    /// The generators of a case, as driver terms.
    fn generator_terms(case: &serde_json::Value) -> Vec<serde_json::Value> {
        case["arguments"]["ideal"]["generators"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["terms"].clone())
            .collect()
    }

    /// `reply` with the values at the pointers changed.
    fn changed(
        reply: &serde_json::Value,
        changes: &[(&str, serde_json::Value)],
    ) -> serde_json::Value {
        let mut reply = reply.clone();
        for (pointer, to) in changes {
            *reply.pointer_mut(pointer).unwrap() = to.clone();
        }
        reply
    }

    /// Issue #67: the normal forms and memberships Macaulay2 gave. Each
    /// membership is the one its case names; the remainder of f by the
    /// twisted cubic satisfies f - r = sum_k q_k g_k at a point, and is the
    /// remainder of the certificate of f not in I; lex and grevlex give
    /// different remainders of the same f (acceptance 1), the one in lex a
    /// polynomial in z alone; the zero ideal leaves f as it is, with no
    /// quotients, and the unit ideal reduces it to 0 with q_1 = f; a
    /// coefficient of a remainder is 2^64 + 2, past 64 bits.
    #[test]
    fn recorded_normal_forms_and_memberships() {
        let value = |name: &str| ideal_case(name)["reply"]["value"].clone();
        let f = |name: &str| ideal_case(name)["arguments"]["polynomial"]["terms"].clone();
        for case in ideal_cases()
            .iter()
            .filter(|c| c["operation"] == ideal::IDEAL_MEMBERSHIP)
        {
            let name = case["name"].as_str().unwrap();
            let member = case["reply"]["value"]["member"].as_bool().unwrap();
            assert_eq!(member, !name.contains("not_in"), "{name}");
        }
        let nf = value("normal_form_twisted_cubic");
        assert!(is_zero(&division_defect(
            &f("normal_form_twisted_cubic"),
            &nf["remainder"],
            &nf["certificate"]["quotients"],
            &nf["basis"],
            &twisted_point()
        )));
        assert_eq!(
            value("membership_twisted_cubic_not_in")["certificate"]["remainder"],
            nf["remainder"]
        );
        assert_eq!(
            value("normal_form_twisted_cubic_in")["remainder"],
            serde_json::json!([])
        );

        let (lex, grevlex) = (
            value("normal_form_small_lex"),
            value("normal_form_small_grevlex"),
        );
        assert_ne!(term_set(&lex["remainder"]), term_set(&grevlex["remainder"]));
        let in_z_alone = |t: &serde_json::Value| t["exponents"][0] == 0 && t["exponents"][1] == 0;
        assert!(lex["remainder"].as_array().unwrap().iter().all(in_z_alone));
        assert!(
            !grevlex["remainder"]
                .as_array()
                .unwrap()
                .iter()
                .all(in_z_alone)
        );
        assert_eq!(
            term_set(&value("normal_form_small_grevlex_none")["remainder"]),
            term_set(&grevlex["remainder"])
        );

        for name in ["normal_form_zero_ideal", "normal_form_no_generators"] {
            let zero = value(name);
            assert_eq!(term_set(&zero["remainder"]), term_set(&f(name)), "{name}");
            assert_eq!(zero["certificate"]["quotients"], serde_json::json!([]));
        }
        let unit = value("normal_form_unit_ideal");
        assert_eq!(unit["remainder"], serde_json::json!([]));
        let quotients = unit["certificate"]["quotients"].as_array().unwrap();
        assert_eq!(quotients.len(), 1);
        assert_eq!(
            term_set(&quotients[0]),
            term_set(&f("normal_form_unit_ideal"))
        );

        let big = value("normal_form_big_coefficient");
        assert!(
            big["remainder"]
                .to_string()
                .contains("\"18446744073709551618\"")
        );
    }

    /// Issue #67: a normal form whose data is wrong is refused, each change
    /// first shown to break its claim at the point (2, 3, 5, 7): a changed
    /// quotient or remainder (`normal_form.reduced`), a remainder with a term
    /// a leading monomial divides, a changed entry of C (`gb.contained`) or
    /// of N (`gb.contains`); so are a certificate the job did not ask for,
    /// none where it did, and the lex normal form sent for the grevlex job.
    #[test]
    fn changed_normal_form_replies_are_refused() {
        let case = ideal_case("normal_form_twisted_cubic");
        let refused = |case: &serde_json::Value, reply: &serde_json::Value| {
            let r = check_matrix_reply(ideal::IDEAL_NORMAL_FORM, &case["arguments"], reply);
            assert!(matches!(r, Err(CasError::UnsupportedResult(_))), "{r:?}");
        };
        let v = twisted_point();
        let reply = &case["reply"];
        let f = &case["arguments"]["polynomial"]["terms"];
        let defect = |reply: &serde_json::Value| {
            let value = &reply["value"];
            division_defect(
                f,
                &value["remainder"],
                &value["certificate"]["quotients"],
                &value["basis"],
                &v,
            )
        };
        assert!(is_zero(&defect(reply)));
        // G = (z^2 - yw, yz - xw, y^2 - xz), q = (0, x, y): q_1 = 2x, or the
        // coefficient of xw in r = x^2 z + x^2 w + xw made 2.
        let quotient = changed(
            reply,
            &[(
                "/value/certificate/quotients/1/0/coefficient",
                serde_json::json!("2"),
            )],
        );
        assert!(!is_zero(&defect(&quotient)));
        refused(&case, &quotient);
        let remainder = changed(
            reply,
            &[("/value/remainder/2/coefficient", serde_json::json!("2"))],
        );
        assert!(!is_zero(&defect(&remainder)));
        refused(&case, &remainder);
        // r + g_0 with q_0 = -1: f - r = sum_k q_k g_k still holds, but the
        // term z^2 of the remainder is the leading monomial of g_0.
        let basis = reply["value"]["basis"].as_array().unwrap().clone();
        assert_eq!(basis[0][0]["exponents"], serde_json::json!([0, 0, 2, 0]));
        let mut r = reply["value"]["remainder"].as_array().unwrap().clone();
        r.extend(basis[0].as_array().unwrap().iter().cloned());
        let unreduced = changed(
            reply,
            &[
                ("/value/remainder", serde_json::Value::Array(r)),
                (
                    "/value/certificate/quotients/0",
                    serde_json::json!([{"coefficient": "-1", "exponents": [0, 0, 0, 0]}]),
                ),
            ],
        );
        assert!(is_zero(&defect(&unreduced)));
        refused(&case, &unreduced);
        // C and N: g_0 = -f_2 and f_1 = -g_1; either sign changed breaks it.
        let generators = generator_terms(&case);
        let c = changed(
            reply,
            &[(
                "/value/certificate/basis_in_generators/columns/0/0/terms/0/coefficient",
                serde_json::json!("1"),
            )],
        );
        let fc = row_times(
            &generators,
            &c["value"]["certificate"]["basis_in_generators"],
            &v,
        );
        assert_ne!(fc[0], evaluate(&basis[0], &v));
        refused(&case, &c);
        let n = changed(
            reply,
            &[(
                "/value/certificate/generators_in_basis/columns/1/0/terms/0/coefficient",
                serde_json::json!("1"),
            )],
        );
        let gn = row_times(
            &basis,
            &n["value"]["certificate"]["generators_in_basis"],
            &v,
        );
        assert_ne!(gn[1], evaluate(&generators[1], &v));
        refused(&case, &n);
        // A certificate the job did not ask for, and none where it did.
        let (none, given) = (
            ideal_case("normal_form_small_grevlex_none"),
            ideal_case("normal_form_small_grevlex"),
        );
        refused(&none, &given["reply"]);
        refused(&given, &none["reply"]);
        // The lex normal form for the grevlex job: y - 9 z^3 leads with
        // -9 z^3 in grevlex.
        refused(&given, &ideal_case("normal_form_small_lex")["reply"]);
    }

    /// Issue #67: a membership whose data is wrong is refused, each change
    /// first shown false at the point (2, 3, 5, 7): a changed coefficient
    /// (`membership.in`); f not in I claimed a member with the coefficients
    /// of another polynomial; for f not in I a changed quotient or remainder
    /// or a remainder not reduced (`normal_form.reduced`), a changed entry of
    /// N (`gb.contains`), and a zero remainder (acceptance 4); so are a
    /// certificate of the wrong kind for `member`, and certificates not asked
    /// for or missing.
    #[test]
    fn changed_membership_replies_are_refused() {
        let refused = |case: &serde_json::Value, reply: &serde_json::Value| {
            let r = check_matrix_reply(ideal::IDEAL_MEMBERSHIP, &case["arguments"], reply);
            assert!(matches!(r, Err(CasError::UnsupportedResult(_))), "{r:?}");
        };
        let v = twisted_point();
        let (in_case, not_in_case) = (
            ideal_case("membership_twisted_cubic_in"),
            ideal_case("membership_twisted_cubic_not_in"),
        );
        let (in_reply, not_in_reply) = (&in_case["reply"], &not_in_case["reply"]);
        let generators = generator_terms(&in_case);
        let f_in = &in_case["arguments"]["polynomial"]["terms"];
        let f_not_in = &not_in_case["arguments"]["polynomial"]["terms"];
        // The value at v of f - sum_i c_i f_i.
        let representation_defect = |f: &serde_json::Value, reply: &serde_json::Value| {
            let c = reply["value"]["certificate"]["coefficients"]
                .as_array()
                .unwrap();
            let sum: BigRational = c
                .iter()
                .zip(&generators)
                .map(|(c, g)| evaluate(c, &v) * evaluate(g, &v))
                .sum();
            evaluate(f, &v) - sum
        };
        assert!(is_zero(&representation_defect(f_in, in_reply)));
        // c = (x, 3, w^2): c_1 = 4.
        let coefficient = changed(
            in_reply,
            &[(
                "/value/certificate/coefficients/1/0/coefficient",
                serde_json::json!("4"),
            )],
        );
        assert!(!is_zero(&representation_defect(f_in, &coefficient)));
        refused(&in_case, &coefficient);
        // The reply for f in I given for f = x^2 z + y^3 + xw.
        assert!(!is_zero(&representation_defect(f_not_in, in_reply)));
        refused(&not_in_case, in_reply);

        // f not in I: G, N, q = (0, x, y), r = x^2 z + x^2 w + xw.
        let certificate = &not_in_reply["value"]["certificate"];
        let defect = |reply: &serde_json::Value, f: &serde_json::Value| {
            let c = &reply["value"]["certificate"];
            division_defect(f, &c["remainder"], &c["quotients"], &c["basis"], &v)
        };
        assert!(is_zero(&defect(not_in_reply, f_not_in)));
        let quotient = changed(
            not_in_reply,
            &[(
                "/value/certificate/quotients/2/0/coefficient",
                serde_json::json!("2"),
            )],
        );
        assert!(!is_zero(&defect(&quotient, f_not_in)));
        refused(&not_in_case, &quotient);
        let remainder = changed(
            not_in_reply,
            &[(
                "/value/certificate/remainder/0/coefficient",
                serde_json::json!("2"),
            )],
        );
        assert!(!is_zero(&defect(&remainder, f_not_in)));
        refused(&not_in_case, &remainder);
        // r + g_2 with q_2 = y - 1: the term y^2 of the remainder is the
        // leading monomial of g_2 = y^2 - xz.
        let basis = certificate["basis"].as_array().unwrap().clone();
        assert_eq!(basis[2][0]["exponents"], serde_json::json!([0, 2, 0, 0]));
        let mut r = certificate["remainder"].as_array().unwrap().clone();
        r.extend(basis[2].as_array().unwrap().iter().cloned());
        let unreduced = changed(
            not_in_reply,
            &[
                ("/value/certificate/remainder", serde_json::Value::Array(r)),
                (
                    "/value/certificate/quotients/2",
                    serde_json::json!([
                        {"coefficient": "1", "exponents": [0, 1, 0, 0]},
                        {"coefficient": "-1", "exponents": [0, 0, 0, 0]}
                    ]),
                ),
            ],
        );
        assert!(is_zero(&defect(&unreduced, f_not_in)));
        refused(&not_in_case, &unreduced);
        // N: f_0 = -g_2; the sign changed gives g_2, not f_0.
        let n = changed(
            not_in_reply,
            &[(
                "/value/certificate/generators_in_basis/columns/0/0/terms/0/coefficient",
                serde_json::json!("1"),
            )],
        );
        let gn = row_times(
            &basis,
            &n["value"]["certificate"]["generators_in_basis"],
            &v,
        );
        assert_ne!(gn[0], evaluate(&generators[0], &v));
        refused(&not_in_case, &n);
        // Acceptance 4: f in I claimed not in I with the true data of its
        // normal form: f - 0 = sum_k q_k g_k holds and N is right, but the
        // remainder is 0.
        let nf = &ideal_case("normal_form_twisted_cubic_in")["reply"]["value"];
        let zero = changed(
            not_in_reply,
            &[
                (
                    "/value/certificate/quotients",
                    nf["certificate"]["quotients"].clone(),
                ),
                ("/value/certificate/remainder", serde_json::json!([])),
            ],
        );
        assert!(is_zero(&defect(&zero, f_in)));
        refused(&in_case, &zero);
        // A certificate of the wrong kind for `member`.
        refused(
            &not_in_case,
            &changed(not_in_reply, &[("/value/member", true.into())]),
        );
        refused(
            &in_case,
            &changed(in_reply, &[("/value/member", false.into())]),
        );
        // A certificate the job did not ask for, and none where it did.
        for (none, given) in [
            (
                "membership_twisted_cubic_in_none",
                "membership_twisted_cubic_in",
            ),
            (
                "membership_twisted_cubic_not_in_none",
                "membership_twisted_cubic_not_in",
            ),
        ] {
            let (none, given) = (ideal_case(none), ideal_case(given));
            refused(&none, &given["reply"]);
            refused(&given, &none["reply"]);
        }
    }

    /// Issue #67: the request of a normal form or a membership carries f by
    /// its exponent vectors next to the generators, never a variable name.
    #[test]
    fn a_reduction_request_carries_the_polynomial() {
        for name in [
            "normal_form_small_lex",
            "membership_twisted_cubic_not_in_none",
        ] {
            let case = ideal_case(name);
            let operation = case["operation"].as_str().unwrap();
            let req = request(&parse(&job(operation, case["arguments"].clone())).unwrap());
            assert_eq!(req["operation"], operation);
            assert_eq!(
                term_set(&req["polynomial"]),
                term_set(&case["arguments"]["polynomial"]["terms"]),
                "{name}"
            );
            assert_eq!(req["certificate"], case["arguments"]["certificate"]);
            assert!(!req.to_string().contains("\"x\""));
        }
    }

    /// Issue #66, acceptance 1: lex and grevlex give different reduced
    /// Groebner bases of the same generators.
    #[test]
    fn lex_and_grevlex_bases_differ() {
        let basis = |name: &str| ideal_case(name)["reply"]["value"]["basis"].clone();
        let t = |c: &str, e: [u32; 3]| serde_json::json!({"coefficient": c, "exponents": e});
        // z^7 - z/162, y - 9 z^3, x - 3 z^2.
        assert_eq!(
            basis("small_lex"),
            serde_json::json!([
                [t("1", [0, 0, 7]), t("-1/162", [0, 0, 1])],
                [t("1", [0, 1, 0]), t("-9", [0, 0, 3])],
                [t("1", [1, 0, 0]), t("-3", [0, 0, 2])]
            ])
        );
        assert_eq!(basis("small_grevlex").as_array().unwrap().len(), 6);
        // F = {-xyz + z^2, -xyz - x^2 + xy}: 4 elements in lex, 3 in grevlex.
        assert_eq!(basis("mixup_lex").as_array().unwrap().len(), 4);
        assert_eq!(basis("mixup_grevlex").as_array().unwrap().len(), 3);
    }

    /// Issues #66, #68: the request carries the generators by exponent
    /// vectors, the order of the ring as the items of `MonomialOrder`, never
    /// the variable names, and the certificate asked for.
    #[test]
    fn an_ideal_request_carries_the_order_items() {
        let case = ideal_case("twisted_cubic_block");
        let req =
            request(&parse(&job(ideal::IDEAL_GROEBNER_BASIS, case["arguments"].clone())).unwrap());
        assert_eq!(
            req["order"],
            serde_json::json!([["Lex", 1], ["Weights", [1, 1, 1]], ["Lex", 3]])
        );
        assert_eq!(req["certificate"], "representation");
        assert_eq!(req["generators"].as_array().unwrap().len(), 3);
        let text = req.to_string();
        assert!(!text.contains("\"x\"") && !text.contains("\"w\""));
        // Issue #68: the certificate of the S-pairs is asked by its name.
        let case = ideal_case("twisted_cubic_block_s_pairs");
        let req =
            request(&parse(&job(ideal::IDEAL_GROEBNER_BASIS, case["arguments"].clone())).unwrap());
        assert_eq!(req["certificate"], "s_pairs");
    }

    /// Issues #66, #67: an ideal over ZZ or in a ring without variables is
    /// refused as invalid arguments before the driver runs. Since #68 the
    /// `s_pairs` certificate is taken.
    #[tokio::test]
    async fn ideal_jobs_the_driver_cannot_take_are_refused_before_running() {
        // Running this executable would fail as unavailable.
        let backend = Macaulay2Backend::new(Macaulay2Config {
            m2: "/nonexistent/M2".into(),
            ..Macaulay2Config::default()
        });
        // Issue #67: the same for the normal form and the membership, and a
        // job of either without its polynomial, or of the Groebner basis
        // with one.
        for (operation, case) in [
            (ideal::IDEAL_GROEBNER_BASIS, "small_lex"),
            (ideal::IDEAL_NORMAL_FORM, "normal_form_small_lex"),
            (ideal::IDEAL_MEMBERSHIP, "membership_twisted_cubic_in"),
        ] {
            let arguments = ideal_case(case)["arguments"].clone();
            let mut over_zz = arguments.clone();
            over_zz["ideal"]["ring"]["domain"] = "ZZ".into();
            let mut no_variables = arguments.clone();
            no_variables["ideal"]["ring"]["variables"] = serde_json::json!([]);
            no_variables["ideal"]["generators"] = serde_json::json!([]);
            if let Some(p) = no_variables.get_mut("polynomial") {
                *p = serde_json::json!({"type": "polynomial", "terms": []});
            }
            let mut polynomial = arguments;
            if polynomial
                .as_object_mut()
                .unwrap()
                .remove("polynomial")
                .is_none()
            {
                polynomial["polynomial"] =
                    ideal_case("normal_form_small_lex")["arguments"]["polynomial"].clone();
            }
            for (arguments, why) in [
                (over_zz, ideal::OVER_QQ_ONLY),
                (no_variables, "in a ring with variables"),
                (polynomial, "polynomial"),
            ] {
                let e = backend
                    .execute(job(operation, arguments), ExecutionContext::new(None))
                    .await
                    .unwrap_err();
                assert!(
                    matches!(e, CasError::InvalidArguments(ref m) if m.contains(why)),
                    "{operation}, {why}: {e:?}"
                );
            }
        }
    }

    /// Issue #54: each operation is declared with its signature from
    /// `algebramesh-ops`.
    #[test]
    fn operations_are_declared_with_their_signatures() {
        let caps = Macaulay2Backend::new(Macaulay2Config::default()).capabilities();
        assert!(!caps.operations.is_empty());
        for op in &caps.operations {
            let d = algebramesh_ops::descriptor(op).unwrap();
            assert_eq!(
                caps.signatures[op],
                algebramesh_core::Signature::new(d.arguments, d.result)
            );
        }
        assert_eq!(caps.signatures.len(), caps.operations.len());
    }

    /// Issue #100: the GKZ operations and types are offered exactly when the
    /// backend is built with the package (feature `pkg-gkz`), and the
    /// operations of the backend itself always.
    #[test]
    fn gkz_is_offered_exactly_with_its_package() {
        let caps = Macaulay2Backend::new(Macaulay2Config::default()).capabilities();
        let with = cfg!(feature = "pkg-gkz");
        for op in ["gkz.generate", "gkz.indicial", "gkz.series"] {
            assert_eq!(caps.operations.iter().any(|o| o == op), with, "{op}");
        }
        for t in ["gkz_system", "gkz_indicial", "gkz_series", "integer_matrix"] {
            assert_eq!(caps.supported_types.iter().any(|s| s == t), with, "{t}");
        }
        for op in OPERATIONS {
            assert!(caps.operations.iter().any(|o| o == op), "{op}");
        }
        if !with {
            assert!(matches!(
                parse(&job("gkz.generate", serde_json::json!({}))),
                Err(CasError::UnsupportedOperation { .. })
            ));
        }
    }

    /// Issue #103: `simplicial.homology` and its types are offered exactly
    /// when the backend is built with the topology package (feature
    /// `pkg-topology`), whose driver file is then hashed with the driver.
    #[test]
    fn topology_is_offered_exactly_with_its_package() {
        let backend = Macaulay2Backend::new(Macaulay2Config::default());
        let caps = backend.capabilities();
        let with = cfg!(feature = "pkg-topology");
        assert_eq!(
            caps.operations.iter().any(|o| o == "simplicial.homology"),
            with
        );
        for t in ["simplicial_complex", "simplicial_homology"] {
            assert_eq!(caps.supported_types.iter().any(|s| s == t), with, "{t}");
        }
        assert_eq!(
            backend
                .driver_files()
                .iter()
                .any(|f| f.ends_with("topology.m2")),
            with
        );
        if !with {
            assert!(matches!(
                parse(&job("simplicial.homology", serde_json::json!({}))),
                Err(CasError::UnsupportedOperation { .. })
            ));
        }
    }

    #[test]
    fn unconfigured_backend_offers_nothing() {
        assert!(
            Macaulay2Backend::default()
                .capabilities()
                .operations
                .is_empty()
        );
    }

    /// The cases of `testdata/module.json` (Issues #97, #98): replies Macaulay2
    /// gave in its image, each with the claims and the levels it must get and
    /// what it must not claim.
    fn module_cases() -> Vec<serde_json::Value> {
        serde_json::from_str(include_str!("../testdata/module.json")).unwrap()
    }

    /// The value of a recorded reply, without the wrapping of this backend.
    fn module_reply(case: &serde_json::Value) -> &serde_json::Value {
        &case["reply"]["value"]
    }

    /// Issues #97, #98: the recorded replies pass the checks with the claims
    /// their case lists, and give its not_claimed.
    #[test]
    fn recorded_module_replies_are_checked() {
        let mut operations = std::collections::BTreeSet::new();
        for case in module_cases() {
            let name = case["name"].as_str().unwrap();
            let operation = case["operation"].as_str().unwrap();
            operations.insert(operation.to_owned());
            let checked = check_matrix_reply(operation, &case["arguments"], &case["reply"])
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let guarantee = checked.guarantee.unwrap();
            let claims: Vec<serde_json::Value> = guarantee
                .claims
                .iter()
                .map(|c| serde_json::json!([c.id, c.level]))
                .collect();
            assert_eq!(serde_json::Value::from(claims), case["claims"], "{name}");
            assert_eq!(
                serde_json::json!(guarantee.not_claimed),
                case["not_claimed"],
                "{name}"
            );
        }
        let mut offered = module::OPERATIONS.map(String::from).to_vec();
        offered.sort();
        assert_eq!(operations.into_iter().collect::<Vec<_>>(), offered);
    }

    /// Issues #97, #98: one changed coefficient in a syzygy, in d_2 of the
    /// twisted cubic, and in the numerator of its Hilbert series, is refused:
    /// F S, d_1 d_2 and the Hilbert series with the Betti numbers no longer
    /// agree.
    #[test]
    fn changed_module_replies_are_refused() {
        for (name, pointer) in [
            (
                "syzygies_twisted_cubic",
                "/syzygies/columns/0/0/terms/0/coefficient",
            ),
            (
                "resolution_twisted_cubic",
                "/differentials/1/columns/0/0/terms/0/coefficient",
            ),
            ("hilbert_series_twisted_cubic", "/numerator/0"),
        ] {
            let case = module_cases()
                .into_iter()
                .find(|c| c["name"] == name)
                .unwrap();
            let mut reply = case["reply"].clone();
            let mut value = module_reply(&case).clone();
            *value.pointer_mut(pointer).unwrap() = serde_json::json!("2");
            reply["value"] = value;
            let operation = case["operation"].as_str().unwrap();
            match check_matrix_reply(operation, &case["arguments"], &reply) {
                Err(CasError::UnsupportedResult(_)) => {}
                other => panic!("{name}: {other:?}"),
            }
        }
    }

    /// Issues #97, #98: the degrees, the Betti numbers and the Hilbert series
    /// Macaulay2 gave agree with those OSCAR gave (its `testdata/module.json`),
    /// once each is read as the backend reads it.
    #[test]
    fn the_betti_tables_agree_with_oscar() {
        let oscar: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../algebramesh-backend-oscar/testdata/module.json"
        ))
        .unwrap();
        let ours = module_cases();
        assert_eq!(ours.len(), oscar.len());
        let mut compared = 0;
        for (a, b) in ours.iter().zip(&oscar) {
            let name = a["name"].as_str().unwrap();
            assert_eq!(a["name"], b["name"]);
            let Operation::Module(request, _) = parse(&job(
                a["operation"].as_str().unwrap(),
                a["arguments"].clone(),
            ))
            .unwrap() else {
                panic!("{name}")
            };
            let ours = request.value(module_reply(a)).unwrap();
            let theirs = request.value(&b["reply"]).unwrap();
            for key in ["degrees", "betti", "numerator", "denominator_exponent"] {
                assert_eq!(ours.get(key), theirs.get(key), "{name}: {key}");
            }
            compared += usize::from(ours.get("betti").is_some());
        }
        assert_eq!(compared, 8);
    }
}
