//! What the tests of the `ideal.*` checks share: the JSON of their values,
//! a polynomial arithmetic of the tests' own (`Q`, `Poly`, `row_times`, `at`,
//! `grevlex`) that does not use the polynomial code of the checks, and the
//! twisted cubic.

#![allow(dead_code, clippy::unwrap_used)]

use std::cmp::Ordering;
use std::collections::BTreeMap;

use algebramesh_core::check::Interrupt;
use algebramesh_core::guarantee::{EvidenceLevel, ResultVerifier};
use algebramesh_core::{BackendInfo, CasError, CasResult, JobSpec};
use algebramesh_verify::{Registry, catalogue};
use serde_json::{Value, json};

/// A job of `operation` on the backend `oscar`.
pub fn job(operation: &str, arguments: Value) -> JobSpec {
    JobSpec {
        schema_version: 1,
        backend: "oscar".into(),
        operation: operation.into(),
        arguments,
        limits: Default::default(),
        retry_policy: Default::default(),
        environment: Default::default(),
    }
}

pub fn result(value: Value) -> CasResult {
    CasResult {
        value,
        backend: BackendInfo {
            name: "oscar".into(),
            ..BackendInfo::default()
        },
        execution_host: None,
        log: vec![],
        guarantee: None,
    }
}

pub fn check(spec: &JobSpec, value: Value) -> Result<CasResult, CasError> {
    let checked = Registry::standard().verify(spec, result(value), &Interrupt::default());
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

pub fn levels(result: &CasResult) -> Vec<(String, EvidenceLevel)> {
    result
        .guarantee
        .as_ref()
        .unwrap()
        .claims
        .iter()
        .map(|c| (c.id.clone(), c.level))
        .collect()
}

pub fn reason(result: &CasResult, id: &str) -> Option<String> {
    let claims = &result.guarantee.as_ref().unwrap().claims;
    claims.iter().find(|c| c.id == id).unwrap().reason.clone()
}

pub fn refused(result: Result<CasResult, CasError>, why: &str) {
    match result {
        Err(CasError::UnsupportedResult(m)) => assert!(m.contains(why), "{m}"),
        other => panic!("expected UNSUPPORTED_RESULT ({why}), got {other:?}"),
    }
}

// ------------------------------------------------------------- the JSON

/// A polynomial as the value writes it, from its terms.
pub fn p(terms: &[(&str, &[u32])]) -> Value {
    let terms: Vec<Value> = terms
        .iter()
        .map(|(c, e)| json!({"coefficient": c, "exponents": e}))
        .collect();
    json!({"type": "polynomial", "terms": terms})
}

/// A polynomial matrix as the value writes it: `rows` rows, each column as
/// its entries by row.
pub fn pm(rows: usize, columns: Vec<Vec<(usize, Value)>>) -> Value {
    let columns: Vec<Value> = columns
        .into_iter()
        .map(|c| {
            Value::Array(
                c.into_iter()
                    .map(|(row, entry)| json!({"row": row, "entry": entry}))
                    .collect(),
            )
        })
        .collect();
    json!({"type": "polynomial_matrix", "rows": rows, "columns": columns})
}

pub fn ring(variables: &[&str], order: &str) -> Value {
    json!({"domain": "QQ", "variables": variables, "order": order})
}

pub fn arguments(ring: Value, generators: &[Value], certificate: &str) -> Value {
    json!({
        "ideal": {"type": "ideal", "ring": ring, "generators": generators},
        "certificate": certificate,
    })
}

// ------------------------------------------ arithmetic of the tests' own

pub fn gcd(a: i128, b: i128) -> i128 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

/// A rational number p/q in lowest terms with q > 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Q(pub i128, pub i128);

impl Q {
    pub fn new(p: i128, q: i128) -> Self {
        let g = gcd(p, q).max(1) * q.signum();
        Self(p / g, q / g)
    }

    pub fn parse(s: &str) -> Self {
        match s.split_once('/') {
            Some((p, q)) => Self::new(p.parse().unwrap(), q.parse().unwrap()),
            None => Self::new(s.parse().unwrap(), 1),
        }
    }

    pub fn add(self, o: Self) -> Self {
        Self::new(self.0 * o.1 + o.0 * self.1, self.1 * o.1)
    }

    pub fn mul(self, o: Self) -> Self {
        Self::new(self.0 * o.0, self.1 * o.1)
    }

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }
}

/// A polynomial: its nonzero terms by exponent vector.
pub type Poly = BTreeMap<Vec<u32>, Q>;

pub fn poly(v: &Value) -> Poly {
    let mut out = Poly::new();
    for t in v["terms"].as_array().unwrap() {
        let e: Vec<u32> = serde_json::from_value(t["exponents"].clone()).unwrap();
        let c = Q::parse(t["coefficient"].as_str().unwrap());
        add_term(&mut out, e, c);
    }
    out
}

pub fn add_term(p: &mut Poly, e: Vec<u32>, c: Q) {
    let sum = p.get(&e).map_or(c, |d| d.add(c));
    if sum.is_zero() {
        p.remove(&e);
    } else {
        p.insert(e, sum);
    }
}

pub fn add(a: &Poly, b: &Poly) -> Poly {
    let mut out = a.clone();
    for (e, c) in b {
        add_term(&mut out, e.clone(), *c);
    }
    out
}

pub fn mul(a: &Poly, b: &Poly) -> Poly {
    let mut out = Poly::new();
    for (e1, c1) in a {
        for (e2, c2) in b {
            let e = e1.iter().zip(e2).map(|(x, y)| x + y).collect();
            add_term(&mut out, e, c1.mul(*c2));
        }
    }
    out
}

pub fn scale(a: &Poly, c: Q) -> Poly {
    mul(
        a,
        &Poly::from([(vec![0; a.keys().next().map_or(0, Vec::len)], c)]),
    )
}

/// The entry at row `i` and column `j` of a matrix in JSON.
pub fn entry(m: &Value, i: usize, j: usize) -> Poly {
    m["columns"][j]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["row"] == i)
        .map_or_else(Poly::new, |e| poly(&e["entry"]))
}

/// `sum_i row_i m_ij`, by the definition of the product.
pub fn row_times(row: &[Poly], m: &Value, j: usize) -> Poly {
    row.iter().enumerate().fold(Poly::new(), |sum, (i, r)| {
        add(&sum, &mul(r, &entry(m, i, j)))
    })
}

/// Whether `g_j = sum_i f_i C_ij` for every j, and `f_i = sum_k g_k N_ki` for
/// every i.
pub fn representations_hold(f: &[Value], g: &[Value], c: &Value, n: &Value) -> (bool, bool) {
    let (f, g): (Vec<Poly>, Vec<Poly>) =
        (f.iter().map(poly).collect(), g.iter().map(poly).collect());
    (
        (0..g.len()).all(|j| row_times(&f, c, j) == g[j]),
        (0..f.len()).all(|i| row_times(&g, n, i) == f[i]),
    )
}

/// The value of `p` at an integer point.
pub fn at(p: &Poly, point: &[i128]) -> Q {
    p.iter().fold(Q(0, 1), |sum, (e, c)| {
        let m = e
            .iter()
            .zip(point)
            .map(|(&d, x)| x.pow(d))
            .product::<i128>();
        sum.add(c.mul(Q(m, 1)))
    })
}

/// grevlex on exponent vectors, by its definition: the degree first, then
/// the smaller last differing exponent belongs to the larger monomial.
pub fn grevlex(a: &[u32], b: &[u32]) -> Ordering {
    let degree = |e: &[u32]| e.iter().sum::<u32>();
    degree(a).cmp(&degree(b)).then_with(|| {
        a.iter()
            .zip(b)
            .rev()
            .find(|(x, y)| x != y)
            .map_or(Ordering::Equal, |(x, y)| y.cmp(x))
    })
}

/// lex on exponent vectors: the first differing exponent decides.
pub fn lex(a: &[u32], b: &[u32]) -> Ordering {
    a.cmp(b)
}

/// The leading term of `p` in `order`.
pub fn leading(p: &Poly, order: fn(&[u32], &[u32]) -> Ordering) -> (Vec<u32>, Q) {
    let (e, c) = p.iter().max_by(|a, b| order(a.0, b.0)).unwrap();
    (e.clone(), *c)
}

pub fn divides(a: &[u32], b: &[u32]) -> bool {
    a.iter().zip(b).all(|(x, y)| x <= y)
}

// -------------------------------------------------------- twisted cubic

pub const XYZW: [&str; 4] = ["x", "y", "z", "w"];

/// F = [xz - y^2, xw - yz, yw - z^2].
pub fn f() -> Vec<Value> {
    vec![
        p(&[("1", &[1, 0, 1, 0]), ("-1", &[0, 2, 0, 0])]),
        p(&[("1", &[1, 0, 0, 1]), ("-1", &[0, 1, 1, 0])]),
        p(&[("1", &[0, 1, 0, 1]), ("-1", &[0, 0, 2, 0])]),
    ]
}

/// G = [z^2 - yw, yz - xw, y^2 - xz], in increasing grevlex order of the
/// leading monomials z^2 < yz < y^2.
pub fn g() -> Vec<Value> {
    vec![
        p(&[("1", &[0, 0, 2, 0]), ("-1", &[0, 1, 0, 1])]),
        p(&[("1", &[0, 1, 1, 0]), ("-1", &[1, 0, 0, 1])]),
        p(&[("1", &[0, 2, 0, 0]), ("-1", &[1, 0, 1, 0])]),
    ]
}

pub fn constant(c: &str) -> Value {
    p(&[(c, &[0, 0, 0, 0])])
}

/// -1 on the antidiagonal of a 3 x 3 matrix: g_j = -f_{2-j}, and
/// f_i = -g_{2-i}.
pub fn antidiagonal() -> Value {
    pm(3, (0..3).map(|j| vec![(2 - j, constant("-1"))]).collect())
}

/// A point of V(I): (t^3, t^2 s, t s^2, s^3) at t = s = 1.
pub const ON_THE_CUBIC: [i128; 4] = [1, 1, 1, 1];

pub fn to_json(p: &Poly) -> Value {
    let terms: Vec<Value> = p
        .iter()
        .map(|(e, c)| {
            let c = if c.1 == 1 {
                c.0.to_string()
            } else {
                format!("{}/{}", c.0, c.1)
            };
            json!({"coefficient": c, "exponents": e})
        })
        .collect();
    json!({"type": "polynomial", "terms": terms})
}
