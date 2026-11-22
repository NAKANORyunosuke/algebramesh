//! The checks of `polynomial.factor` through the registry (Issue #47).
//!
//! Each claim at level checked has a mutation test (ADR 0018): the test makes
//! the claim false, confirms that by hand, without the checker, and expects
//! UNSUPPORTED_RESULT.
//!
//! | claim | mutation tests |
//! |---|---|
//! | `factor.product` | `a_product_other_than_the_input_is_refused`, `a_factor_of_multiplicity_zero_is_refused`, `a_constant_factor_is_refused` |
//! | `factor.domain` | `coefficients_outside_the_domain_are_refused`, `a_factor_with_a_coefficient_outside_the_domain_is_refused` |
//! | `factor.primitive` | `over_the_integers_the_factors_are_primitive` |
//! | `factor.non_associate` | `associate_factors_are_refused` |

#![allow(clippy::unwrap_used)]

use algebramesh_core::check::Interrupt;
use algebramesh_core::guarantee::{EvidenceLevel, ResultVerifier, Summary};
use algebramesh_core::{BackendInfo, CasError, CasResult, JobSpec};
use algebramesh_verify::{Registry, catalogue};
use serde_json::{Value, json};

fn ring(domain: &str) -> Value {
    json!({"domain": domain, "variables": ["x", "y"], "order": "grevlex"})
}

fn poly(terms: &[(&str, [u32; 2])]) -> Value {
    json!({"type": "polynomial", "terms": terms.iter()
        .map(|(c, e)| json!({"coefficient": c, "exponents": e})).collect::<Vec<_>>()})
}

fn spec(domain: &str, input: Value) -> JobSpec {
    JobSpec {
        schema_version: 1,
        backend: "asir".into(),
        operation: "polynomial.factor".into(),
        arguments: json!({"ring": ring(domain), "expression": input}),
        limits: Default::default(),
        retry_policy: Default::default(),
        environment: Default::default(),
    }
}

/// The value a backend writes for `constant * prod(factor ^ multiplicity)`.
fn value(domain: &str, constant: &str, factors: &[(Value, u32, &str)]) -> Value {
    json!({
        "type": "factorization",
        "ring": ring(domain),
        "constant": constant,
        "factors": factors.iter()
            .map(|(f, m, d)| json!({"factor": f, "multiplicity": m, "display": d}))
            .collect::<Vec<_>>(),
        "verified_product": true,
    })
}

fn check(spec: &JobSpec, value: Value) -> Result<CasResult, CasError> {
    let result = CasResult {
        value,
        backend: BackendInfo {
            name: "asir".into(),
            backend_version: Some("20260331".into()),
            driver_version: "algebramesh-backend-asir 1.1.0".into(),
            ..BackendInfo::default()
        },
        execution_host: None,
        log: vec![],
        guarantee: None,
    };
    let checked = Registry::standard().verify(spec, result, &Interrupt::default());
    // What a result claims is what the catalogue lists (Issue #61).
    if let Ok(CasResult {
        guarantee: Some(guarantee),
        ..
    }) = &checked
    {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/claims.md");
        let catalogue = catalogue::parse(&std::fs::read_to_string(path).unwrap());
        let problems = catalogue.check_guarantee(&spec.operation, guarantee);
        assert!(problems.is_empty(), "{problems:#?}");
    }
    checked
}

fn x4_minus_y4() -> Value {
    poly(&[("1", [4, 0]), ("-1", [0, 4])])
}

fn factors_of_x4_minus_y4() -> Vec<(Value, u32, &'static str)> {
    vec![
        (poly(&[("1", [1, 0]), ("-1", [0, 1])]), 1, "x-y"),
        (poly(&[("1", [1, 0]), ("1", [0, 1])]), 1, "x+y"),
        (poly(&[("1", [2, 0]), ("1", [0, 2])]), 1, "x^2+y^2"),
    ]
}

fn refused(result: Result<CasResult, CasError>, why: &str) {
    match result {
        Err(CasError::UnsupportedResult(m)) => assert!(m.contains(why), "{m}"),
        other => panic!("expected UNSUPPORTED_RESULT ({why}), got {other:?}"),
    }
}

// The mutations are confirmed with the arithmetic below, of plain integers,
// and not with the checker or the mathematical types it uses.

fn gcd(a: i128, b: i128) -> i128 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

/// The rational number n/d, with d > 0 and gcd(n, d) = 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Q(i128, i128);

impl Q {
    fn new(n: i128, d: i128) -> Self {
        let g = gcd(n, d) * d.signum();
        Self(n / g, d / g)
    }

    /// `n` or `n/d`, as the value writes a coefficient.
    fn parse(s: &str) -> Self {
        match s.split_once('/') {
            Some((n, d)) => Self::new(n.parse().unwrap(), d.parse().unwrap()),
            None => Self::new(s.parse().unwrap(), 1),
        }
    }

    fn is_integer(self) -> bool {
        self.1 == 1
    }
}

impl std::ops::Add for Q {
    type Output = Self;
    fn add(self, o: Self) -> Self {
        Self::new(self.0 * o.1 + o.0 * self.1, self.1 * o.1)
    }
}

impl std::ops::Mul for Q {
    type Output = Self;
    fn mul(self, o: Self) -> Self {
        Self::new(self.0 * o.0, self.1 * o.1)
    }
}

/// The terms c x^i y^j of a polynomial in the JSON, as (c, [i, j]).
fn terms(p: &Value) -> Vec<(Q, [u32; 2])> {
    p["terms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            let e = |k: usize| u32::try_from(t["exponents"][k].as_u64().unwrap()).unwrap();
            (Q::parse(t["coefficient"].as_str().unwrap()), [e(0), e(1)])
        })
        .collect()
}

/// p(x, y).
fn at(p: &Value, x: i128, y: i128) -> Q {
    terms(p).into_iter().fold(Q::new(0, 1), |sum, (c, [i, j])| {
        sum + c * Q::new(x.pow(i) * y.pow(j), 1)
    })
}

/// constant * prod f_i(x, y)^m_i.
fn product_at(constant: &str, factors: &[(Value, u32, &str)], x: i128, y: i128) -> Q {
    factors
        .iter()
        .fold(Q::parse(constant), |product, (f, m, _)| {
            (0..*m).fold(product, |p, _| p * at(f, x, y))
        })
}

/// Whether the input is constant * prod f_i^m_i. Both have degree at most d in
/// each variable, and two such polynomials that agree at every point of
/// {0, ..., d}^2 are equal.
fn multiplies_to(input: &Value, constant: &str, factors: &[(Value, u32, &str)]) -> bool {
    let degree = |p: &Value| terms(p).iter().flat_map(|(_, e)| *e).max().unwrap_or(0);
    let d = factors.iter().map(|(f, m, _)| m * degree(f)).sum::<u32>();
    let d = i128::from(d.max(degree(input)));
    (0..=d).all(|x| (0..=d).all(|y| at(input, x, y) == product_at(constant, factors, x, y)))
}

/// Whether f = u g, for f and g of degree at most 1 in each variable, which
/// then agree on {0, 1}^2.
fn proportional(f: &Value, g: &Value, u: Q) -> bool {
    (0..=1).all(|x| (0..=1).all(|y| at(f, x, y) == u * at(g, x, y)))
}

fn is_constant(f: &Value) -> bool {
    terms(f).iter().all(|(_, e)| *e == [0, 0])
}

#[test]
fn a_right_factorization_gets_its_claims() {
    let spec = spec("QQ", x4_minus_y4());
    let value = value("QQ", "1", &factors_of_x4_minus_y4());
    let checked = check(&spec, value.clone()).unwrap();
    assert_eq!(checked.value, value, "the value is not changed");
    let g = checked.guarantee.unwrap();
    let level = |id: &str| g.claims.iter().find(|c| c.id == id).map(|c| c.level);
    assert_eq!(level("factor.domain"), Some(EvidenceLevel::Checked));
    assert_eq!(level("factor.non_associate"), Some(EvidenceLevel::Checked));
    assert_eq!(level("factor.irreducible"), Some(EvidenceLevel::Asserted));
    assert_eq!(level("factor.primitive"), None, "over QQ only");
    assert_eq!(g.summary, Summary::PartiallyVerified);
    let asserted = g
        .claims
        .iter()
        .find(|c| c.id == "factor.irreducible")
        .unwrap();
    assert_eq!(
        asserted.by,
        "asir 20260331 (algebramesh-backend-asir 1.1.0)"
    );
}

/// factor.product: x^2 + y^2 replaced by x^2 + 2y^2. Only the product is
/// wrong: the factors are rational, not constant, of multiplicity 1,
/// irreducible and pairwise not associate.
#[test]
fn a_product_other_than_the_input_is_refused() {
    let input = x4_minus_y4();
    let spec = spec("QQ", input.clone());
    let mut wrong = factors_of_x4_minus_y4();
    wrong[2] = (poly(&[("1", [2, 0]), ("2", [0, 2])]), 1, "x^2+2*y^2");
    // At (x, y) = (2, 1) the input is 16 - 1 = 15 and the product is 1 * 3 * 6.
    assert_eq!(at(&input, 2, 1), Q::new(15, 1));
    assert_eq!(product_at("1", &wrong, 2, 1), Q::new(18, 1));
    refused(check(&spec, value("QQ", "1", &wrong)), "multiply to");
}

/// The input is read from the specification: a backend that answers another
/// question, with a right factorization of another polynomial, is refused.
#[test]
fn a_factorization_of_another_polynomial_is_refused() {
    let spec = spec("QQ", x4_minus_y4());
    let other = value(
        "QQ",
        "1",
        &[
            (poly(&[("1", [1, 0]), ("-1", [0, 1])]), 1, "x-y"),
            (poly(&[("1", [1, 0]), ("1", [0, 1])]), 1, "x+y"),
        ],
    );
    refused(check(&spec, other), "multiply to");
}

#[test]
fn another_ring_or_another_shape_is_refused() {
    let spec = spec("QQ", x4_minus_y4());
    let mut value = value("QQ", "1", &factors_of_x4_minus_y4());
    value["ring"]["order"] = json!("lex");
    refused(check(&spec, value), "ring");
    refused(
        check(&spec, json!({"type": "gkz_system"})),
        "not a factorization",
    );
}

/// factor.domain: over ZZ the constant 1/2 is refused, though the product is
/// the input.
#[test]
fn coefficients_outside_the_domain_are_refused() {
    // 2x^2 - 2 over ZZ, as 1/2 * (2x - 2) * (2x + 2).
    let input = poly(&[("2", [2, 0]), ("-2", [0, 0])]);
    let spec = spec("ZZ", input.clone());
    let factors = [
        (poly(&[("2", [1, 0]), ("-2", [0, 0])]), 1, "2*x-2"),
        (poly(&[("2", [1, 0]), ("2", [0, 0])]), 1, "2*x+2"),
    ];
    assert!(!Q::parse("1/2").is_integer());
    assert!(multiplies_to(&input, "1/2", &factors));
    // With the constant outside ZZ, factors over ZZ cannot all be primitive
    // (Gauss's lemma); the message shows that the domain refused it.
    refused(
        check(&spec, value("ZZ", "1/2", &factors)),
        "the constant 1/2 is not in ZZ",
    );
}

/// factor.domain: over ZZ a factor with the coefficient 1/2 is refused. The
/// constant 4 is an integer, and the product is the input.
#[test]
fn a_factor_with_a_coefficient_outside_the_domain_is_refused() {
    // 2x^2 - 2 over ZZ, as 4 * (x/2 - 1/2) * (x + 1).
    let input = poly(&[("2", [2, 0]), ("-2", [0, 0])]);
    let spec = spec("ZZ", input.clone());
    let factors = [
        (poly(&[("1/2", [1, 0]), ("-1/2", [0, 0])]), 1, "x/2-1/2"),
        (poly(&[("1", [1, 0]), ("1", [0, 0])]), 1, "x+1"),
    ];
    assert!(terms(&factors[0].0).iter().all(|(c, _)| !c.is_integer()));
    assert!(Q::parse("4").is_integer());
    assert!(multiplies_to(&input, "4", &factors));
    refused(
        check(&spec, value("ZZ", "4", &factors)),
        "coefficient 1/2 is not in ZZ",
    );
}

/// Issue #102: two factors that differ by a unit are one factor with the sum
/// of their multiplicities, so a factorization that lists both is refused,
/// over QQ (unit 2) and over ZZ (unit -1), whatever else holds.
#[test]
fn associate_factors_are_refused() {
    // x^2 - 1 over QQ as (x - 1)(2x + 2)/2 is fine; (x + 1)(2x + 2)/2 =
    // (x + 1)^2 lists x + 1 twice, up to the unit 2.
    let input = poly(&[("1", [2, 0]), ("2", [1, 0]), ("1", [0, 0])]);
    let spec_qq = spec("QQ", input.clone());
    let twice = [
        (poly(&[("1", [1, 0]), ("1", [0, 0])]), 1, "x+1"),
        (poly(&[("2", [1, 0]), ("2", [0, 0])]), 1, "2*x+2"),
    ];
    assert!(
        multiplies_to(&input, "1/2", &twice),
        "the product is the input"
    );
    assert!(
        proportional(&twice[1].0, &twice[0].0, Q::new(2, 1)),
        "2x + 2 = 2 (x + 1)"
    );
    refused(
        check(&spec_qq, value("QQ", "1/2", &twice)),
        "the factors x+1 and 2*x+2 are associates",
    );
    let once = [(poly(&[("1", [1, 0]), ("1", [0, 0])]), 2, "x+1")];
    let g = check(&spec_qq, value("QQ", "1", &once))
        .unwrap()
        .guarantee
        .unwrap();
    let claim = g
        .claims
        .iter()
        .find(|c| c.id == "factor.non_associate")
        .unwrap();
    assert_eq!(claim.level, EvidenceLevel::Checked);

    // Over ZZ: (x - y)(y - x) = -(x - y)^2, with x - y and y - x = -(x - y).
    let input = poly(&[("-1", [2, 0]), ("2", [1, 1]), ("-1", [0, 2])]);
    let opposite = [
        (poly(&[("1", [1, 0]), ("-1", [0, 1])]), 1, "x-y"),
        (poly(&[("-1", [1, 0]), ("1", [0, 1])]), 1, "-x+y"),
    ];
    assert!(
        multiplies_to(&input, "1", &opposite),
        "the product is the input"
    );
    assert!(
        proportional(&opposite[1].0, &opposite[0].0, Q::new(-1, 1)),
        "y - x = -(x - y)"
    );
    refused(
        check(&spec("ZZ", input), value("ZZ", "1", &opposite)),
        "the factors x-y and -x+y are associates",
    );
}

/// Over ZZ the factors are primitive: the content belongs in the constant.
#[test]
fn over_the_integers_the_factors_are_primitive() {
    let input = poly(&[("2", [2, 0]), ("-2", [0, 0])]);
    let spec = spec("ZZ", input.clone());
    let right = value(
        "ZZ",
        "2",
        &[
            (poly(&[("1", [1, 0]), ("-1", [0, 0])]), 1, "x-1"),
            (poly(&[("1", [1, 0]), ("1", [0, 0])]), 1, "x+1"),
        ],
    );
    let g = check(&spec, right).unwrap().guarantee.unwrap();
    let primitive = g
        .claims
        .iter()
        .find(|c| c.id == "factor.primitive")
        .unwrap();
    assert_eq!(primitive.level, EvidenceLevel::Checked);
    let irreducible = g
        .claims
        .iter()
        .find(|c| c.id == "factor.irreducible")
        .unwrap();
    assert_eq!(
        irreducible.depends_on,
        ["factor.product", "factor.primitive"]
    );

    // factor.primitive: the same product with the content 2 left in a factor.
    let content_in_a_factor = [
        (poly(&[("2", [1, 0]), ("-2", [0, 0])]), 1, "2*x-2"),
        (poly(&[("1", [1, 0]), ("1", [0, 0])]), 1, "x+1"),
    ];
    // Every coefficient is an integer, and the product is the input.
    let mut coefficients = content_in_a_factor.iter().flat_map(|(f, _, _)| terms(f));
    assert!(coefficients.all(|(c, _)| c.is_integer()));
    assert!(multiplies_to(&input, "1", &content_in_a_factor));
    // The content of 2x - 2 is gcd(2, -2) = 2.
    let content = terms(&content_in_a_factor[0].0)
        .iter()
        .fold(0, |g, (c, _)| gcd(g, c.0));
    assert_eq!(content, 2);
    refused(
        check(&spec, value("ZZ", "1", &content_in_a_factor)),
        "not primitive",
    );
}

/// Issue #48: a non-constant factor of multiplicity 0 multiplies to 1, so the
/// product still equals the input; the factorization is refused all the
/// same, for its multiplicity.
#[test]
fn a_factor_of_multiplicity_zero_is_refused() {
    let input = x4_minus_y4();
    let spec = spec("QQ", input.clone());
    let mut mutated = factors_of_x4_minus_y4();
    mutated.push((poly(&[("1", [1, 0]), ("1", [0, 0])]), 0, "x+1"));
    // The factor x + 1 is not constant and has multiplicity 0, and the
    // product is the input: only the multiplicity can refuse it.
    assert!(!is_constant(&mutated[3].0));
    assert_eq!(mutated[3].1, 0);
    assert!(multiplies_to(&input, "1", &mutated));
    refused(check(&spec, value("QQ", "1", &mutated)), "multiplicity 0");
}

/// A constant factor belongs in the constant.
#[test]
fn a_constant_factor_is_refused() {
    let input = x4_minus_y4();
    let spec = spec("QQ", input.clone());
    let mut with_constant = factors_of_x4_minus_y4();
    with_constant.push((poly(&[("1", [0, 0])]), 1, "1"));
    // The factor 1 is constant, of multiplicity 1, and the product is the
    // input: only the check of constant factors can refuse it.
    assert!(is_constant(&with_constant[3].0));
    assert!(multiplies_to(&input, "1", &with_constant));
    refused(
        check(&spec, value("QQ", "1", &with_constant)),
        "is a constant",
    );
}

/// With the multiplicities checked, the product claim is checked.
#[test]
fn the_product_is_checked() {
    let spec = spec("QQ", x4_minus_y4());
    let g = check(&spec, value("QQ", "1", &factors_of_x4_minus_y4()))
        .unwrap()
        .guarantee
        .unwrap();
    let product = g.claims.iter().find(|c| c.id == "factor.product").unwrap();
    assert_eq!(product.level, EvidenceLevel::Checked);
}
