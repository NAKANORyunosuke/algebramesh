//! `ideal.*` (A3, Issues #66, #67): the reduced Groebner basis of an ideal
//! of QQ[x_1, ..., x_n] in the monomial order of its ring, the normal form of
//! a polynomial modulo the ideal, and whether the polynomial lies in the
//! ideal. A CAS computes the values and their certificates;
//! `algebramesh-verify` checks them (ADR 0019, ADR 0024, ADR 0025).
//!
//! The value of `ideal.groebner_basis` holds the basis G and, unless the job
//! asked for none, a `certificate` of two matrices: C (s x t) with
//! `g_j = sum_i f_i C_ij` and N (t x s) with `f_i = sum_k g_k N_ki`, for the
//! s generators F of the job and the t elements of G.
//!
//! The value of `ideal.normal_form` holds the remainder r of the polynomial
//! f of the job and the basis G; its certificate adds to C and N the
//! quotients q_1, ..., q_t with `f - r = sum_k q_k g_k`. The value of
//! `ideal.membership` says whether f lies in the ideal; its certificate is
//! the coefficients c_1, ..., c_s with `f = sum_i c_i f_i` when it does, and
//! G, N, the quotients and the remainder (r != 0) when it does not.
//!
//! When the job asks for `certificate: "s_pairs"` (Issue #68), the
//! certificate of `ideal.groebner_basis` and of `ideal.normal_form`, and the
//! certificate of `ideal.membership` when f does not lie in the ideal, also
//! hold `s_pairs`: the pairs (i, j), i < j, of elements of G whose leading
//! monomials are not coprime, in increasing lexicographic order, and a
//! matrix H (t x P, one column for each of the P pairs) with
//! `S(g_i, g_j) = sum_k g_k H_kp` for the p-th pair (i, j), each product
//! `H_kp g_k` with its leading monomial below `lcm(lm(g_i), lm(g_j))`.
//!
//! A certificate too large for a result (before #36) is replaced by
//! `{"omitted": "certificate_too_large"}`.

use algebramesh_math_types::{
    CoefficientDomain, EntryData, Ideal, IdealData, Polynomial, PolynomialData, PolynomialKind,
    PolynomialMatrix, PolynomialMatrixData, PolynomialMatrixKind, RingSpec, TermData,
};
use serde::{Deserialize, Serialize};

pub use crate::certificate::{CERTIFICATE_TOO_LARGE, Certificate};

pub const IDEAL_GROEBNER_BASIS: &str = "ideal.groebner_basis";
pub const IDEAL_NORMAL_FORM: &str = "ideal.normal_form";
pub const IDEAL_MEMBERSHIP: &str = "ideal.membership";

/// The operations on ideals.
pub const OPERATIONS: [&str; 3] = [IDEAL_GROEBNER_BASIS, IDEAL_NORMAL_FORM, IDEAL_MEMBERSHIP];

/// Why the `ideal.*` operations refuse a ring over another domain than QQ.
pub const OVER_QQ_ONLY: &str = "the ideal.* operations are over QQ only";

/// The most polynomials times variables a job may have: the generators of
/// its ideal, and the polynomial of `ideal.normal_form` and
/// `ideal.membership`. Each polynomial holds the ring, its variables among
/// them, so a zero generator of a few bytes would otherwise cost a copy of
/// every variable name.
pub const MAX_GENERATORS_TIMES_VARIABLES: usize = 1 << 20;

/// Why a reply is refused that has no `s_pairs` though the job asked for
/// them.
pub const S_PAIRS_MISSING: &str =
    "the job asked for the certificate of the S-pairs and the reply has none";

/// Why a reply is refused that has `s_pairs` though the job did not ask for
/// them.
pub const S_PAIRS_UNREQUESTED: &str = "a certificate of the S-pairs the job did not ask for";

/// Which certificate the backend returns with a Groebner basis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdealCertificateRequest {
    /// C and N: the basis and the generators in terms of each other.
    #[default]
    Representation,
    /// C, N and an lcm representation of each S-pair (Issue #68).
    SPairs,
    None,
}

impl IdealCertificateRequest {
    /// Whether the job asked for a certificate at all.
    pub fn any(self) -> bool {
        self != Self::None
    }

    /// The name of the request, as the jobs and the drivers write it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Representation => "representation",
            Self::SPairs => "s_pairs",
            Self::None => "none",
        }
    }
}

/// The arguments of `ideal.groebner_basis`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroebnerBasisArguments {
    /// The ideal of QQ[x_1, ..., x_n] given by its generators F, in the
    /// monomial order of its ring.
    pub ideal: IdealData,
    #[serde(default)]
    pub certificate: IdealCertificateRequest,
}

/// The arguments of `ideal.normal_form` and `ideal.membership`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdealPolynomialArguments {
    /// The ideal of QQ[x_1, ..., x_n] given by its generators F, in the
    /// monomial order of its ring.
    pub ideal: IdealData,
    /// The polynomial f, read in the ring of the ideal.
    pub polynomial: PolynomialData,
    #[serde(default)]
    pub certificate: IdealCertificateRequest,
}

/// `ideal.groebner_basis`: the reduced Groebner basis G of the ideal in the
/// order of its ring, without zero elements, each element monic, in strictly
/// increasing order of the leading monomials. The ring is the ring of the
/// job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroebnerBasisResult {
    #[serde(rename = "type")]
    pub kind: GroebnerBasisKind,
    pub basis: Vec<PolynomialData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<Certificate<GroebnerCertificate>>,
}

/// The basis and the generators in terms of each other, and the S-pairs
/// when the job asked for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroebnerCertificate {
    /// C, s x t: `g_j = sum_i f_i C_ij`.
    pub basis_in_generators: PolynomialMatrixData,
    /// N, t x s: `f_i = sum_k g_k N_ki`.
    pub generators_in_basis: PolynomialMatrixData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s_pairs: Option<SPairsCertificate>,
}

/// An lcm representation of each S-pair of the basis G = (g_1, ..., g_t)
/// whose leading monomials are not coprime (Issue #68).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SPairsCertificate {
    /// Each (i, j), 0-based, i < j, with lm(g_i) and lm(g_j) not coprime,
    /// in increasing lexicographic order of (i, j).
    pub pairs: Vec<[usize; 2]>,
    /// H, t x P: column p holds h_1, ..., h_t for the p-th pair (i, j),
    /// with `S(g_i, g_j) = sum_k g_k H_kp`.
    pub representations: PolynomialMatrixData,
}

/// The `"type"` field of [`GroebnerBasisResult`], which must be
/// `"groebner_basis"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroebnerBasisKind {
    #[default]
    #[serde(rename = "groebner_basis")]
    Value,
}

/// `ideal.normal_form`: the normal form r of f modulo the ideal in the order
/// of its ring, and the reduced Groebner basis G it is taken with, as in
/// [`GroebnerBasisResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalFormResult {
    #[serde(rename = "type")]
    pub kind: NormalFormKind,
    pub remainder: PolynomialData,
    pub basis: Vec<PolynomialData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<Certificate<NormalFormCertificate>>,
}

/// C and N as for a Groebner basis, the quotients, and the S-pairs when the
/// job asked for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalFormCertificate {
    /// C, s x t: `g_j = sum_i f_i C_ij`.
    pub basis_in_generators: PolynomialMatrixData,
    /// N, t x s: `f_i = sum_k g_k N_ki`.
    pub generators_in_basis: PolynomialMatrixData,
    /// q_1, ..., q_t: `f - r = sum_k q_k g_k`.
    pub quotients: Vec<PolynomialData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s_pairs: Option<SPairsCertificate>,
}

/// The `"type"` field of [`NormalFormResult`], which must be
/// `"normal_form"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NormalFormKind {
    #[default]
    #[serde(rename = "normal_form")]
    Value,
}

/// `ideal.membership`: whether f lies in the ideal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipResult {
    #[serde(rename = "type")]
    pub kind: MembershipKind,
    pub member: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub certificate: Option<Certificate<MembershipCertificate>>,
}

/// The certificate of `ideal.membership`: of the kind `member` says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum MembershipCertificate {
    /// `member: true`: `f = sum_i c_i f_i`, one coefficient for each
    /// generator.
    InIdeal { coefficients: Vec<PolynomialData> },
    /// `member: false`: the reduced Groebner basis G, N (t x s) with
    /// `f_i = sum_k g_k N_ki`, the quotients with `f - r = sum_k q_k g_k`,
    /// the remainder r, which is not 0, and the S-pairs when the job asked
    /// for them.
    NotInIdeal {
        basis: Vec<PolynomialData>,
        generators_in_basis: PolynomialMatrixData,
        quotients: Vec<PolynomialData>,
        remainder: PolynomialData,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        s_pairs: Option<SPairsCertificate>,
    },
}

/// The `"type"` field of [`MembershipResult`], which must be
/// `"ideal_membership"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MembershipKind {
    #[default]
    #[serde(rename = "ideal_membership")]
    Value,
}

/// An ideal job as the drivers take it, checked before any CAS runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdealRequest {
    pub operation: &'static str,
    /// The ideal of the job, its generators read in its ring.
    pub ideal: Ideal,
    /// The polynomial f of `ideal.normal_form` and `ideal.membership`, read
    /// in the ring of the ideal; none for `ideal.groebner_basis`.
    pub polynomial: Option<Polynomial>,
    /// The certificate the job asked for.
    pub certificate: IdealCertificateRequest,
}

/// One term as the drivers write it: `{"coefficient": "p/q", "exponents":
/// [...]}`.
#[derive(Debug, Clone, Serialize)]
struct WireTerm {
    coefficient: String,
    exponents: Vec<u32>,
}

/// A polynomial matrix as the drivers write it: the number of rows, and each
/// column as its nonzero entries by row.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverMatrix {
    rows: usize,
    columns: Vec<Vec<DriverEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverEntry {
    row: usize,
    terms: Vec<TermData>,
}

/// The S-pairs as the drivers write them: H as a [`DriverMatrix`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverSPairs {
    pairs: Vec<[usize; 2]>,
    representations: DriverMatrix,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverCertificate {
    basis_in_generators: DriverMatrix,
    generators_in_basis: DriverMatrix,
    #[serde(default)]
    s_pairs: Option<DriverSPairs>,
}

/// The reply of a driver to `ideal.groebner_basis`, without the wrapping of
/// its backend.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverGroebnerBasis {
    basis: Vec<Vec<TermData>>,
    certificate: Option<DriverCertificate>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverNormalFormCertificate {
    basis_in_generators: DriverMatrix,
    generators_in_basis: DriverMatrix,
    quotients: Vec<Vec<TermData>>,
    #[serde(default)]
    s_pairs: Option<DriverSPairs>,
}

/// The reply of a driver to `ideal.normal_form`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverNormalForm {
    basis: Vec<Vec<TermData>>,
    remainder: Vec<TermData>,
    certificate: Option<DriverNormalFormCertificate>,
}

/// The reply of a driver to `ideal.membership`; its certificate is read by
/// the kind `member` says.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverMembership {
    member: bool,
    certificate: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverInIdeal {
    coefficients: Vec<Vec<TermData>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DriverNotInIdeal {
    basis: Vec<Vec<TermData>>,
    generators_in_basis: DriverMatrix,
    quotients: Vec<Vec<TermData>>,
    remainder: Vec<TermData>,
    #[serde(default)]
    s_pairs: Option<DriverSPairs>,
}

impl IdealRequest {
    /// Reads and checks the arguments of a job of `operation`: the ideal is
    /// over QQ, in a ring with variables and a valid order, its generators
    /// (and the polynomial of `ideal.normal_form` and `ideal.membership`)
    /// belong to the ring, there are at most
    /// [`MAX_GENERATORS_TIMES_VARIABLES`] polynomials times variables, and
    /// the certificate is `representation`, `s_pairs` or `none`.
    /// `ideal.groebner_basis` takes no polynomial; the other two require
    /// one.
    pub fn parse(operation: &str, arguments: &serde_json::Value) -> Result<Self, String> {
        let Some(&operation) = OPERATIONS.iter().find(|op| **op == operation) else {
            return Err(format!("{operation} is not an ideal operation"));
        };
        let (ideal, polynomial, certificate) = if operation == IDEAL_GROEBNER_BASIS {
            let args: GroebnerBasisArguments =
                serde_json::from_value(arguments.clone()).map_err(|e| e.to_string())?;
            (args.ideal, None, args.certificate)
        } else {
            let args: IdealPolynomialArguments =
                serde_json::from_value(arguments.clone()).map_err(|e| e.to_string())?;
            (args.ideal, Some(args.polynomial), args.certificate)
        };
        let ring = &ideal.ring;
        if ring.domain != CoefficientDomain::Rationals {
            return Err(OVER_QQ_ONLY.into());
        }
        if ring.nvars() == 0 {
            return Err(format!("{OVER_QQ_ONLY}, in a ring with variables"));
        }
        let generators = ideal.generators.len();
        let polynomials = generators.saturating_add(usize::from(polynomial.is_some()));
        if polynomials.saturating_mul(ring.nvars()) > MAX_GENERATORS_TIMES_VARIABLES {
            let what = if polynomial.is_some() {
                format!("{generators} generators and the polynomial")
            } else {
                format!("{generators} generators")
            };
            return Err(format!(
                "{what} in {} variables: at most {MAX_GENERATORS_TIMES_VARIABLES} generators times variables",
                ring.nvars()
            ));
        }
        let polynomial = polynomial
            .map(|p| Polynomial::from_data(ring.clone(), &p))
            .transpose()
            .map_err(|e| format!("the polynomial: {e}"))?;
        let ideal = Ideal::from_data(&ideal).map_err(|e| format!("the ideal: {e}"))?;
        Ok(Self {
            operation,
            ideal,
            polynomial,
            certificate,
        })
    }

    /// The ring of the ideal.
    pub fn ring(&self) -> &RingSpec {
        &self.ideal.ring
    }

    /// The request for a driver. `order` is the monomial order of the ring as
    /// the driver takes it: the items the backend's `order_items` gives for
    /// `self.ring().order` and `self.ring().nvars()` (Issue #64). The
    /// polynomial of `ideal.normal_form` and `ideal.membership` comes as
    /// `polynomial`, its terms written as those of the generators. The
    /// certificate is `"representation"`, `"s_pairs"` or `"none"`.
    pub fn driver_json(&self, order: &impl Serialize) -> serde_json::Value {
        let ring = self.ring();
        let generators: Vec<Vec<WireTerm>> = self.ideal.generators.iter().map(wire).collect();
        let mut request = serde_json::json!({
            "operation": self.operation,
            "domain": ring.domain.name(),
            "nvars": ring.nvars(),
            "order": order,
            "generators": generators,
            "certificate": self.certificate.name(),
        });
        if let Some(p) = &self.polynomial {
            request["polynomial"] = serde_json::json!(wire(p));
        }
        request
    }

    /// The value of the job from the driver's reply (without the wrapping of
    /// its backend). Each polynomial is read in the ring of the job and
    /// written again in its canonical form, so that every value has one form
    /// whatever the driver printed; a polynomial that does not belong to the
    /// ring, a matrix of another shape than C (s x t), N (t x s) and H
    /// (t x P for P pairs), a list of quotients other than t or of
    /// coefficients other than s, a certificate the job did not ask for, or
    /// missing, or left out by the driver, S-pairs the job did not ask for,
    /// or missing when it did, and a certificate of `ideal.membership` of
    /// another kind than `member` says, are refused. Whether the value is
    /// right is for the checker, the list of pairs included.
    pub fn value(&self, reply: &serde_json::Value) -> Result<serde_json::Value, String> {
        // Only the checker leaves a certificate out.
        let certificate = reply.get("certificate").filter(|c| !c.is_null());
        match certificate {
            Some(c) if c.get("omitted").is_some() => {
                return Err("the driver left the certificate out; only the checker does".into());
            }
            Some(_) if !self.certificate.any() => {
                return Err("a certificate the job did not ask for".into());
            }
            None if self.certificate.any() => {
                return Err("the job asked for a certificate and the reply has none".into());
            }
            _ => {}
        }
        match self.operation {
            IDEAL_NORMAL_FORM => self.normal_form_value(reply),
            IDEAL_MEMBERSHIP => self.membership_value(reply),
            _ => self.groebner_basis_value(reply),
        }
    }

    fn groebner_basis_value(&self, reply: &serde_json::Value) -> Result<serde_json::Value, String> {
        let reply: DriverGroebnerBasis =
            serde_json::from_value(reply.clone()).map_err(|e| e.to_string())?;
        let ring = self.ring();
        let basis = polynomials(ring, "the basis element g", &reply.basis)?;
        let (s, t) = (self.ideal.generators.len(), basis.len());
        let certificate = match reply.certificate {
            None => None,
            Some(c) => Some(Certificate::Given(GroebnerCertificate {
                basis_in_generators: matrix(ring, "C", &c.basis_in_generators, s, t)?,
                generators_in_basis: matrix(ring, "N", &c.generators_in_basis, t, s)?,
                s_pairs: self.s_pairs(c.s_pairs.as_ref(), t)?,
            })),
        };
        let value = GroebnerBasisResult {
            kind: GroebnerBasisKind::Value,
            basis,
            certificate,
        };
        serde_json::to_value(value).map_err(|e| e.to_string())
    }

    fn normal_form_value(&self, reply: &serde_json::Value) -> Result<serde_json::Value, String> {
        let reply: DriverNormalForm =
            serde_json::from_value(reply.clone()).map_err(|e| e.to_string())?;
        let ring = self.ring();
        let basis = polynomials(ring, "the basis element g", &reply.basis)?;
        let remainder =
            canonical(ring, &reply.remainder).map_err(|e| format!("the remainder: {e}"))?;
        let (s, t) = (self.ideal.generators.len(), basis.len());
        let certificate = match reply.certificate {
            None => None,
            Some(c) => Some(Certificate::Given(NormalFormCertificate {
                basis_in_generators: matrix(ring, "C", &c.basis_in_generators, s, t)?,
                generators_in_basis: matrix(ring, "N", &c.generators_in_basis, t, s)?,
                quotients: counted(ring, "quotient", "q", &c.quotients, t)?,
                s_pairs: self.s_pairs(c.s_pairs.as_ref(), t)?,
            })),
        };
        let value = NormalFormResult {
            kind: NormalFormKind::Value,
            remainder,
            basis,
            certificate,
        };
        serde_json::to_value(value).map_err(|e| e.to_string())
    }

    fn membership_value(&self, reply: &serde_json::Value) -> Result<serde_json::Value, String> {
        let reply: DriverMembership =
            serde_json::from_value(reply.clone()).map_err(|e| e.to_string())?;
        let ring = self.ring();
        let s = self.ideal.generators.len();
        let certificate = match (reply.member, reply.certificate) {
            (_, None | Some(serde_json::Value::Null)) => None,
            (true, Some(c)) => {
                if c.get("coefficients").is_none() {
                    return Err(
                        "member is true and the certificate is not one of coefficients".into(),
                    );
                }
                let c: DriverInIdeal = serde_json::from_value(c).map_err(|e| e.to_string())?;
                Some(MembershipCertificate::InIdeal {
                    coefficients: counted(ring, "coefficient", "c", &c.coefficients, s)?,
                })
            }
            (false, Some(c)) => {
                if c.get("coefficients").is_some() {
                    return Err("member is false and the certificate is one of coefficients".into());
                }
                let c: DriverNotInIdeal = serde_json::from_value(c).map_err(|e| e.to_string())?;
                let basis = polynomials(ring, "the basis element g", &c.basis)?;
                let t = basis.len();
                Some(MembershipCertificate::NotInIdeal {
                    generators_in_basis: matrix(ring, "N", &c.generators_in_basis, t, s)?,
                    quotients: counted(ring, "quotient", "q", &c.quotients, t)?,
                    remainder: canonical(ring, &c.remainder)
                        .map_err(|e| format!("the remainder: {e}"))?,
                    s_pairs: self.s_pairs(c.s_pairs.as_ref(), t)?,
                    basis,
                })
            }
        };
        let value = MembershipResult {
            kind: MembershipKind::Value,
            member: reply.member,
            certificate: certificate.map(Certificate::Given),
        };
        serde_json::to_value(value).map_err(|e| e.to_string())
    }

    /// The S-pairs of a certificate with a basis of `t` elements: present
    /// exactly when the job asked for them, with H of t rows and one column
    /// for each pair, read in the ring of the job.
    fn s_pairs(
        &self,
        given: Option<&DriverSPairs>,
        t: usize,
    ) -> Result<Option<SPairsCertificate>, String> {
        match (self.certificate, given) {
            (IdealCertificateRequest::SPairs, Some(s)) => Ok(Some(SPairsCertificate {
                pairs: s.pairs.clone(),
                representations: matrix(self.ring(), "H", &s.representations, t, s.pairs.len())?,
            })),
            (IdealCertificateRequest::SPairs, None) => Err(S_PAIRS_MISSING.into()),
            (_, Some(_)) => Err(S_PAIRS_UNREQUESTED.into()),
            (_, None) => Ok(None),
        }
    }
}

/// The terms of `p` as the drivers take them, in decreasing lexicographic
/// order of the exponents.
fn wire(p: &Polynomial) -> Vec<WireTerm> {
    p.terms()
        .rev()
        .map(|(e, c)| WireTerm {
            coefficient: c.to_string(),
            exponents: e.to_vec(),
        })
        .collect()
}

/// `terms` read in `ring` and written in canonical form.
fn canonical(ring: &RingSpec, terms: &[TermData]) -> Result<PolynomialData, String> {
    let data = PolynomialData {
        kind: PolynomialKind::Polynomial,
        terms: terms.to_vec(),
    };
    Polynomial::from_data(ring.clone(), &data)
        .map(|p| p.to_data())
        .map_err(|e| e.to_string())
}

/// Each polynomial of `list` read in `ring` and written in canonical form;
/// `name_j` names the j-th in a refusal.
fn polynomials(
    ring: &RingSpec,
    name: &str,
    list: &[Vec<TermData>],
) -> Result<Vec<PolynomialData>, String> {
    list.iter()
        .enumerate()
        .map(|(j, terms)| canonical(ring, terms).map_err(|e| format!("{name}_{j}: {e}")))
        .collect()
}

/// [`polynomials`] of a list that must have `expected` polynomials: one
/// `what` (`symbol_j`) for each.
fn counted(
    ring: &RingSpec,
    what: &str,
    symbol: &str,
    list: &[Vec<TermData>],
    expected: usize,
) -> Result<Vec<PolynomialData>, String> {
    if list.len() != expected {
        return Err(format!("{} {what}s, not {expected}", list.len()));
    }
    polynomials(ring, &format!("the {what} {symbol}"), list)
}

/// The matrix `name` of a driver's reply, of `nrows x ncols`, read in `ring`
/// and written in canonical form.
fn matrix(
    ring: &RingSpec,
    name: &str,
    m: &DriverMatrix,
    nrows: usize,
    ncols: usize,
) -> Result<PolynomialMatrixData, String> {
    if m.rows != nrows || m.columns.len() != ncols {
        return Err(format!(
            "{name} is {} x {}, not {nrows} x {ncols}",
            m.rows,
            m.columns.len()
        ));
    }
    let data = PolynomialMatrixData {
        kind: PolynomialMatrixKind::PolynomialMatrix,
        rows: m.rows,
        columns: m
            .columns
            .iter()
            .map(|column| {
                column
                    .iter()
                    .map(|e| EntryData {
                        row: e.row,
                        entry: PolynomialData {
                            kind: PolynomialKind::Polynomial,
                            terms: e.terms.clone(),
                        },
                    })
                    .collect()
            })
            .collect(),
    };
    PolynomialMatrix::from_data(ring.clone(), &data)
        .map(|m| m.to_data())
        .map_err(|e| format!("{name}: {e}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::{Value, json};

    use super::*;

    /// The twisted cubic: F = [xz - y^2, xw - yz, yw - z^2] in QQ[x, y, z, w]
    /// with grevlex.
    fn twisted_cubic(certificate: &str) -> Value {
        let t = |c: &str, e: [u32; 4]| json!({"coefficient": c, "exponents": e});
        let p = |terms: Vec<Value>| json!({"type": "polynomial", "terms": terms});
        json!({
            "ideal": {
                "type": "ideal",
                "ring": {"domain": "QQ", "variables": ["x", "y", "z", "w"], "order": "grevlex"},
                "generators": [
                    p(vec![t("1", [1, 0, 1, 0]), t("-1", [0, 2, 0, 0])]),
                    p(vec![t("1", [1, 0, 0, 1]), t("-1", [0, 1, 1, 0])]),
                    p(vec![t("1", [0, 1, 0, 1]), t("-1", [0, 0, 2, 0])]),
                ],
            },
            "certificate": certificate,
        })
    }

    fn parse(arguments: Value) -> Result<IdealRequest, String> {
        IdealRequest::parse(IDEAL_GROEBNER_BASIS, &arguments)
    }

    /// The reply of a driver for the twisted cubic: G = [z^2 - yw, yz - xw,
    /// y^2 - xz], C and N with -1 on the antidiagonal.
    fn reply() -> Value {
        let t = |c: &str, e: [u32; 4]| json!({"coefficient": c, "exponents": e});
        let minus_one = || vec![t("-1", [0, 0, 0, 0])];
        let antidiagonal = json!({
            "rows": 3,
            "columns": [
                [{"row": 2, "terms": minus_one()}],
                [{"row": 1, "terms": minus_one()}],
                [{"row": 0, "terms": minus_one()}],
            ],
        });
        json!({
            "basis": [
                [t("1", [0, 0, 2, 0]), t("-1", [0, 1, 0, 1])],
                [t("1", [0, 1, 1, 0]), t("-1", [1, 0, 0, 1])],
                [t("1", [0, 2, 0, 0]), t("-1", [1, 0, 1, 0])],
            ],
            "certificate": {
                "basis_in_generators": antidiagonal,
                "generators_in_basis": antidiagonal,
            },
        })
    }

    #[test]
    fn the_request_names_the_order_the_backend_gives() {
        let request = parse(twisted_cubic("representation")).unwrap();
        let order = json!([["GRevLex", 4]]);
        assert_eq!(
            request.driver_json(&order),
            json!({
                "operation": "ideal.groebner_basis", "domain": "QQ", "nvars": 4,
                "order": [["GRevLex", 4]],
                "generators": [
                    [{"coefficient": "1", "exponents": [1, 0, 1, 0]},
                     {"coefficient": "-1", "exponents": [0, 2, 0, 0]}],
                    [{"coefficient": "1", "exponents": [1, 0, 0, 1]},
                     {"coefficient": "-1", "exponents": [0, 1, 1, 0]}],
                    [{"coefficient": "1", "exponents": [0, 1, 0, 1]},
                     {"coefficient": "-1", "exponents": [0, 0, 2, 0]}],
                ],
                "certificate": "representation",
            })
        );
        // The default is a representation.
        let mut args = twisted_cubic("none");
        args.as_object_mut().unwrap().remove("certificate");
        assert_eq!(
            parse(args).unwrap().certificate,
            IdealCertificateRequest::Representation
        );
        let none = parse(twisted_cubic("none")).unwrap();
        assert_eq!(none.certificate, IdealCertificateRequest::None);
        assert_eq!(none.driver_json(&order)["certificate"], "none");
        // Issue #68: the S-pairs are asked of the driver by name.
        let s_pairs = parse(twisted_cubic("s_pairs")).unwrap();
        assert_eq!(s_pairs.certificate, IdealCertificateRequest::SPairs);
        assert_eq!(s_pairs.driver_json(&order)["certificate"], "s_pairs");
    }

    #[test]
    fn requests_are_checked_before_any_cas_runs() {
        let mut zz = twisted_cubic("representation");
        zz["ideal"]["ring"]["domain"] = json!("ZZ");
        assert_eq!(parse(zz).unwrap_err(), OVER_QQ_ONLY);
        let none = json!({"ideal": {"type": "ideal",
            "ring": {"domain": "QQ", "variables": [], "order": "lex"}, "generators": []}});
        assert!(parse(none).unwrap_err().starts_with(OVER_QQ_ONLY));
        let mut bad_order = twisted_cubic("representation");
        bad_order["ideal"]["ring"]["order"] = json!({"weights": [1, 2], "then": "lex"});
        assert!(parse(bad_order).unwrap_err().contains("weights"));
        let mut short = twisted_cubic("representation");
        short["ideal"]["generators"][0]["terms"][0]["exponents"] = json!([1, 0, 1]);
        assert!(parse(short).is_err());
        let mut unknown = twisted_cubic("representation");
        unknown["script"] = json!("x");
        assert!(parse(unknown).is_err());
        assert!(
            parse(json!({"ideal": twisted_cubic("none")["ideal"], "certificate": "include"}))
                .is_err()
        );
        assert!(IdealRequest::parse("ideal.saturation", &twisted_cubic("none")).is_err());
    }

    /// Issue #66 (review): zero generators of a few bytes each in a ring of
    /// many variables are refused before the ideal is built, since each
    /// generator holds the variables of the ring.
    #[test]
    fn many_generators_in_many_variables_are_refused() {
        let ideal = |nvars: usize, generators: usize| {
            let variables: Vec<String> = (0..nvars).map(|i| format!("v{i}")).collect();
            let zero = json!({"type": "polynomial", "terms": []});
            json!({"ideal": {"type": "ideal",
                "ring": {"domain": "QQ", "variables": variables, "order": "grevlex"},
                "generators": vec![zero; generators]}})
        };
        let limit = MAX_GENERATORS_TIMES_VARIABLES;
        assert!(parse(ideal(1024, limit / 1024)).is_ok());
        let error = parse(ideal(1024, limit / 1024 + 1)).unwrap_err();
        assert!(error.contains("generators times variables"), "{error}");
    }

    #[test]
    fn a_reply_is_read_by_its_type_in_the_ring_of_the_job() {
        let request = parse(twisted_cubic("representation")).unwrap();
        let value = request.value(&reply()).unwrap();
        let result: GroebnerBasisResult = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(result.basis.len(), 3);
        // Written again in canonical form: the terms of g_2 come in another
        // order, the matrices with their type.
        assert_eq!(
            value["basis"][2],
            json!({"type": "polynomial", "terms": [
                {"coefficient": "-1", "exponents": [1, 0, 1, 0]},
                {"coefficient": "1", "exponents": [0, 2, 0, 0]}]})
        );
        assert_eq!(
            value["certificate"]["basis_in_generators"]["type"],
            "polynomial_matrix"
        );
        assert!(matches!(result.certificate, Some(Certificate::Given(_))));
        // A certificate the job did not ask for, a missing one, and one the
        // driver left out are refused.
        let none = parse(twisted_cubic("none")).unwrap();
        assert!(none.value(&reply()).unwrap_err().contains("did not ask"));
        let mut bare = reply();
        bare.as_object_mut().unwrap().remove("certificate");
        assert!(none.value(&bare).is_ok());
        assert!(request.value(&bare).unwrap_err().contains("has none"));
        let mut omitted = reply();
        omitted["certificate"] = json!({"omitted": "certificate_too_large"});
        assert!(
            request
                .value(&omitted)
                .unwrap_err()
                .contains("only the checker")
        );
        // A matrix of another shape, a term of another ring, an unknown key.
        let mut shape = reply();
        shape["certificate"]["generators_in_basis"]["rows"] = json!(2);
        assert!(request.value(&shape).unwrap_err().contains("N is 2 x 3"));
        let mut ring = reply();
        ring["basis"][0][0]["exponents"] = json!([0, 0, 2]);
        assert!(request.value(&ring).unwrap_err().contains("g_0"));
        let mut extra = reply();
        extra["ring"] = json!("QQ");
        assert!(request.value(&extra).is_err());
    }

    #[test]
    fn values_read_and_write_their_json() {
        let omitted: GroebnerBasisResult = serde_json::from_value(json!({
            "type": "groebner_basis", "basis": [],
            "certificate": {"omitted": "certificate_too_large"},
        }))
        .unwrap();
        assert!(matches!(
            omitted.certificate,
            Some(Certificate::Omitted { .. })
        ));
        let bare: GroebnerBasisResult =
            serde_json::from_value(json!({"type": "groebner_basis", "basis": []})).unwrap();
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({"type": "groebner_basis", "basis": []})
        );
        assert!(
            serde_json::from_value::<GroebnerBasisResult>(json!({"type": "ideal", "basis": []}))
                .is_err()
        );
    }

    // ------------------------------------------- normal form and membership

    /// The twisted cubic with f = x^2 w - y^3 (in I) or f = xw (not in I).
    fn with_polynomial(certificate: &str, f: Value) -> Value {
        let mut args = twisted_cubic(certificate);
        args["polynomial"] = f;
        args
    }

    fn poly(terms: &[(&str, [u32; 4])]) -> Value {
        let terms: Vec<Value> = terms
            .iter()
            .map(|(c, e)| json!({"coefficient": c, "exponents": e}))
            .collect();
        json!({"type": "polynomial", "terms": terms})
    }

    /// xw, which is not in the ideal: it is its own normal form.
    fn xw() -> Value {
        poly(&[("1", [1, 0, 0, 1])])
    }

    /// The terms of a driver for `p`.
    fn terms(p: &Value) -> Value {
        p["terms"].clone()
    }

    #[test]
    fn a_polynomial_job_names_its_polynomial() {
        for op in [IDEAL_NORMAL_FORM, IDEAL_MEMBERSHIP] {
            let request =
                IdealRequest::parse(op, &with_polynomial("representation", xw())).unwrap();
            assert_eq!(request.operation, op);
            assert_eq!(request.certificate, IdealCertificateRequest::Representation);
            let order = json!([["GRevLex", 4]]);
            let wire = request.driver_json(&order);
            assert_eq!(wire["operation"], op);
            assert_eq!(
                wire["polynomial"],
                json!([{"coefficient": "1", "exponents": [1, 0, 0, 1]}])
            );
            assert_eq!(wire["generators"].as_array().unwrap().len(), 3);
            // f is required, and read in the ring of the ideal.
            assert!(IdealRequest::parse(op, &twisted_cubic("representation")).is_err());
            let short = with_polynomial(
                "none",
                json!({"type": "polynomial", "terms": [
                {"coefficient": "1", "exponents": [1, 0, 1]}]}),
            );
            let error = IdealRequest::parse(op, &short).unwrap_err();
            assert!(error.starts_with("the polynomial:"), "{error}");
            let s_pairs = IdealRequest::parse(op, &with_polynomial("s_pairs", xw())).unwrap();
            assert_eq!(s_pairs.driver_json(&order)["certificate"], "s_pairs");
            let mut zz = with_polynomial("none", xw());
            zz["ideal"]["ring"]["domain"] = json!("ZZ");
            assert_eq!(IdealRequest::parse(op, &zz).unwrap_err(), OVER_QQ_ONLY);
            let none = IdealRequest::parse(op, &with_polynomial("none", xw())).unwrap();
            assert_eq!(none.certificate, IdealCertificateRequest::None);
            assert_eq!(none.driver_json(&order)["certificate"], "none");
        }
        // ideal.groebner_basis takes no polynomial.
        assert!(parse(with_polynomial("representation", xw())).is_err());
        assert_eq!(parse(twisted_cubic("none")).unwrap().polynomial, None);
        assert!(
            parse(twisted_cubic("none"))
                .unwrap()
                .driver_json(&json!([]))
                .get("polynomial")
                .is_none()
        );
    }

    /// Issue #67: the polynomial counts with the generators, (s + 1) n.
    #[test]
    fn the_polynomial_counts_towards_the_limit() {
        let job = |nvars: usize, generators: usize| {
            let variables: Vec<String> = (0..nvars).map(|i| format!("v{i}")).collect();
            let zero = json!({"type": "polynomial", "terms": []});
            json!({"ideal": {"type": "ideal",
                "ring": {"domain": "QQ", "variables": variables, "order": "grevlex"},
                "generators": vec![zero.clone(); generators]},
                "polynomial": zero})
        };
        let limit = MAX_GENERATORS_TIMES_VARIABLES;
        assert!(IdealRequest::parse(IDEAL_MEMBERSHIP, &job(1024, limit / 1024 - 1)).is_ok());
        let error = IdealRequest::parse(IDEAL_NORMAL_FORM, &job(1024, limit / 1024)).unwrap_err();
        assert!(error.contains("and the polynomial"), "{error}");
    }

    /// A normal form of xw: r = xw, the basis of the cubic, C and N as for
    /// the basis, and three zero quotients.
    fn normal_form_reply() -> Value {
        let mut reply = reply();
        reply["remainder"] = terms(&xw());
        reply["certificate"]["quotients"] = json!([[], [], []]);
        reply
    }

    #[test]
    fn a_normal_form_reply_is_read_in_the_ring_of_the_job() {
        let request =
            IdealRequest::parse(IDEAL_NORMAL_FORM, &with_polynomial("representation", xw()))
                .unwrap();
        let value = request.value(&normal_form_reply()).unwrap();
        assert_eq!(value["type"], "normal_form");
        let result: NormalFormResult = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(result.basis.len(), 3);
        assert_eq!(result.remainder.terms.len(), 1);
        let Some(Certificate::Given(c)) = &result.certificate else {
            panic!("a certificate");
        };
        assert_eq!(c.quotients.len(), 3);
        assert!(c.quotients.iter().all(|q| q.terms.is_empty()));
        // Quotients other than t, a remainder of another ring, a certificate
        // without quotients.
        let mut two = normal_form_reply();
        two["certificate"]["quotients"] = json!([[], []]);
        assert!(
            request
                .value(&two)
                .unwrap_err()
                .contains("2 quotients, not 3")
        );
        let mut ring = normal_form_reply();
        ring["remainder"][0]["exponents"] = json!([1, 0, 0]);
        assert!(request.value(&ring).unwrap_err().contains("the remainder"));
        let mut bad_quotient = normal_form_reply();
        bad_quotient["certificate"]["quotients"][1] =
            json!([{"coefficient": "1", "exponents": [1]}]);
        assert!(
            request
                .value(&bad_quotient)
                .unwrap_err()
                .contains("the quotient q_1")
        );
        let mut no_quotients = normal_form_reply();
        no_quotients["certificate"]
            .as_object_mut()
            .unwrap()
            .remove("quotients");
        assert!(request.value(&no_quotients).is_err());
        // The certificate: not asked for, missing, left out.
        let none = IdealRequest::parse(IDEAL_NORMAL_FORM, &with_polynomial("none", xw())).unwrap();
        assert!(
            none.value(&normal_form_reply())
                .unwrap_err()
                .contains("did not ask")
        );
        let mut bare = normal_form_reply();
        bare.as_object_mut().unwrap().remove("certificate");
        let value = none.value(&bare).unwrap();
        assert!(value.get("certificate").is_none());
        assert!(request.value(&bare).unwrap_err().contains("has none"));
        let mut omitted = normal_form_reply();
        omitted["certificate"] = json!({"omitted": "certificate_too_large"});
        assert!(
            request
                .value(&omitted)
                .unwrap_err()
                .contains("only the checker")
        );
        // A reply without a remainder.
        let mut no_remainder = bare;
        no_remainder.as_object_mut().unwrap().remove("remainder");
        assert!(none.value(&no_remainder).is_err());
    }

    /// x^2 w - y^3 = y f_0 + x f_1: the coefficients (y, x, 0).
    fn in_ideal_reply() -> Value {
        json!({"member": true, "certificate": {"coefficients": [
            [{"coefficient": "1", "exponents": [0, 1, 0, 0]}],
            [{"coefficient": "1", "exponents": [1, 0, 0, 0]}],
            [],
        ]}})
    }

    fn not_in_ideal_reply() -> Value {
        let reply = normal_form_reply();
        json!({"member": false, "certificate": {
            "basis": reply["basis"],
            "generators_in_basis": reply["certificate"]["generators_in_basis"],
            "quotients": reply["certificate"]["quotients"],
            "remainder": reply["remainder"],
        }})
    }

    #[test]
    fn a_membership_reply_is_read_by_its_kind() {
        let request =
            IdealRequest::parse(IDEAL_MEMBERSHIP, &with_polynomial("representation", xw()))
                .unwrap();
        let value = request.value(&in_ideal_reply()).unwrap();
        assert_eq!(value["type"], "ideal_membership");
        let result: MembershipResult = serde_json::from_value(value).unwrap();
        assert!(result.member);
        assert!(matches!(
            result.certificate,
            Some(Certificate::Given(MembershipCertificate::InIdeal { ref coefficients }))
                if coefficients.len() == 3
        ));
        let value = request.value(&not_in_ideal_reply()).unwrap();
        let result: MembershipResult = serde_json::from_value(value.clone()).unwrap();
        assert!(!result.member);
        assert!(matches!(
            result.certificate,
            Some(Certificate::Given(MembershipCertificate::NotInIdeal { ref basis, .. }))
                if basis.len() == 3
        ));
        assert_eq!(
            value["certificate"]["generators_in_basis"]["type"],
            "polynomial_matrix"
        );
        // A certificate of the wrong kind for `member`.
        let mut true_with_remainder = not_in_ideal_reply();
        true_with_remainder["member"] = json!(true);
        assert!(
            request
                .value(&true_with_remainder)
                .unwrap_err()
                .contains("not one of coefficients")
        );
        let mut false_with_coefficients = in_ideal_reply();
        false_with_coefficients["member"] = json!(false);
        assert!(
            request
                .value(&false_with_coefficients)
                .unwrap_err()
                .contains("is one of coefficients")
        );
        // Counts, shapes, unknown keys.
        let mut two = in_ideal_reply();
        two["certificate"]["coefficients"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert!(
            request
                .value(&two)
                .unwrap_err()
                .contains("2 coefficients, not 3")
        );
        let mut c_too = not_in_ideal_reply();
        c_too["certificate"]["basis_in_generators"] =
            normal_form_reply()["certificate"]["basis_in_generators"].clone();
        assert!(request.value(&c_too).is_err());
        let mut extra = in_ideal_reply();
        extra["certificate"]["remainder"] = json!([]);
        assert!(request.value(&extra).is_err());
        let mut shape = not_in_ideal_reply();
        shape["certificate"]["generators_in_basis"]["rows"] = json!(2);
        assert!(request.value(&shape).unwrap_err().contains("N is 2 x 3"));
        let mut quotients = not_in_ideal_reply();
        quotients["certificate"]["quotients"] = json!([[]]);
        assert!(
            request
                .value(&quotients)
                .unwrap_err()
                .contains("1 quotients, not 3")
        );
        // The certificate: not asked for, missing, left out.
        let none = IdealRequest::parse(IDEAL_MEMBERSHIP, &with_polynomial("none", xw())).unwrap();
        assert!(
            none.value(&in_ideal_reply())
                .unwrap_err()
                .contains("did not ask")
        );
        let bare = json!({"member": false});
        assert_eq!(
            none.value(&bare).unwrap(),
            json!({"type": "ideal_membership", "member": false})
        );
        assert!(request.value(&bare).unwrap_err().contains("has none"));
        let omitted = json!({"member": true, "certificate": {"omitted": "certificate_too_large"}});
        assert!(
            request
                .value(&omitted)
                .unwrap_err()
                .contains("only the checker")
        );
    }

    #[test]
    fn normal_forms_and_memberships_read_and_write_their_json() {
        let bare: NormalFormResult = serde_json::from_value(json!({
            "type": "normal_form", "remainder": {"type": "polynomial", "terms": []}, "basis": [],
        }))
        .unwrap();
        assert_eq!(bare.certificate, None);
        assert!(
            serde_json::from_value::<NormalFormResult>(json!({"type": "groebner_basis",
                "remainder": {"type": "polynomial", "terms": []}, "basis": []}))
            .is_err()
        );
        let omitted: MembershipResult = serde_json::from_value(json!({
            "type": "ideal_membership", "member": true,
            "certificate": {"omitted": "certificate_too_large"},
        }))
        .unwrap();
        assert!(matches!(
            omitted.certificate,
            Some(Certificate::Omitted { .. })
        ));
        let zero = json!({"type": "polynomial", "terms": []});
        let in_ideal: MembershipResult = serde_json::from_value(json!({
            "type": "ideal_membership", "member": true,
            "certificate": {"coefficients": [zero]},
        }))
        .unwrap();
        assert!(matches!(
            in_ideal.certificate,
            Some(Certificate::Given(MembershipCertificate::InIdeal { .. }))
        ));
        // Each kind refuses the keys of the other.
        assert!(
            serde_json::from_value::<MembershipResult>(json!({
                "type": "ideal_membership", "member": true,
                "certificate": {"coefficients": [], "remainder": zero},
            }))
            .is_err()
        );
        let n = json!({"type": "polynomial_matrix", "rows": 0, "columns": []});
        let not_in: MembershipResult = serde_json::from_value(json!({
            "type": "ideal_membership", "member": false,
            "certificate": {"basis": [], "generators_in_basis": n, "quotients": [],
                            "remainder": zero},
        }))
        .unwrap();
        assert!(matches!(
            not_in.certificate,
            Some(Certificate::Given(MembershipCertificate::NotInIdeal { .. }))
        ));
        assert!(
            serde_json::from_value::<MembershipResult>(json!({
                "type": "ideal_membership", "member": false,
                "certificate": {"basis": [], "generators_in_basis": n, "quotients": [],
                                "remainder": zero, "basis_in_generators": n},
            }))
            .is_err()
        );
        assert_eq!(
            serde_json::to_value(&not_in).unwrap()["certificate"]["remainder"],
            zero
        );
    }

    // ------------------------------------------------------------ S-pairs

    /// The S-pairs of the twisted cubic as a driver writes them: (0, 1) and
    /// (1, 2), with S(g_0, g_1) = -w g_2 and S(g_1, g_2) = x g_0.
    fn s_pairs_reply() -> Value {
        let t = |c: &str, e: [u32; 4]| json!({"coefficient": c, "exponents": e});
        json!({
            "pairs": [[0, 1], [1, 2]],
            "representations": {"rows": 3, "columns": [
                [{"row": 2, "terms": [t("-1", [0, 0, 0, 1])]}],
                [{"row": 0, "terms": [t("1", [1, 0, 0, 0])]}],
            ]},
        })
    }

    /// Issue #68: the S-pairs come in the certificate exactly when the job
    /// asked for them, with H read as a matrix of t rows and one column for
    /// each pair.
    #[test]
    fn s_pairs_come_exactly_when_the_job_asks_for_them() {
        let request = parse(twisted_cubic("s_pairs")).unwrap();
        let mut with = reply();
        with["certificate"]["s_pairs"] = s_pairs_reply();
        let value = request.value(&with).unwrap();
        assert_eq!(
            value["certificate"]["s_pairs"]["pairs"],
            json!([[0, 1], [1, 2]])
        );
        assert_eq!(
            value["certificate"]["s_pairs"]["representations"]["type"],
            "polynomial_matrix"
        );
        let result: GroebnerBasisResult = serde_json::from_value(value).unwrap();
        let Some(Certificate::Given(c)) = result.certificate else {
            panic!("a certificate");
        };
        let s = c.s_pairs.unwrap();
        assert_eq!(s.pairs, [[0, 1], [1, 2]]);
        assert_eq!(
            (s.representations.rows, s.representations.columns.len()),
            (3, 2)
        );
        // Missing when asked for, given when not, H of another shape.
        assert_eq!(request.value(&reply()).unwrap_err(), S_PAIRS_MISSING);
        let representation = parse(twisted_cubic("representation")).unwrap();
        assert_eq!(
            representation.value(&with).unwrap_err(),
            S_PAIRS_UNREQUESTED
        );
        let none = parse(twisted_cubic("none")).unwrap();
        assert!(none.value(&with).unwrap_err().contains("did not ask"));
        let mut short = with.clone();
        short["certificate"]["s_pairs"]["pairs"] = json!([[0, 1]]);
        assert!(
            request
                .value(&short)
                .unwrap_err()
                .contains("H is 3 x 2, not 3 x 1")
        );
        let mut rows = with.clone();
        rows["certificate"]["s_pairs"]["representations"]["rows"] = json!(2);
        assert!(
            request
                .value(&rows)
                .unwrap_err()
                .contains("H is 2 x 2, not 3 x 2")
        );
        let mut unknown = with.clone();
        unknown["certificate"]["s_pairs"]["lcm"] = json!([]);
        assert!(request.value(&unknown).is_err());
        let mut triple = with;
        triple["certificate"]["s_pairs"]["pairs"] = json!([[0, 1, 2], [1, 2]]);
        assert!(request.value(&triple).is_err());
        // No pairs: H is t x 0.
        let mut empty = reply();
        empty["certificate"]["s_pairs"] = json!({"pairs": [],
            "representations": {"rows": 3, "columns": []}});
        assert!(request.value(&empty).is_ok());
    }

    #[test]
    fn s_pairs_come_with_normal_forms_and_memberships_not_in() {
        for op in [IDEAL_NORMAL_FORM, IDEAL_MEMBERSHIP] {
            let request = IdealRequest::parse(op, &with_polynomial("s_pairs", xw())).unwrap();
            let representation =
                IdealRequest::parse(op, &with_polynomial("representation", xw())).unwrap();
            let mut with = if op == IDEAL_NORMAL_FORM {
                normal_form_reply()
            } else {
                not_in_ideal_reply()
            };
            let without = with.clone();
            with["certificate"]["s_pairs"] = s_pairs_reply();
            let value = request.value(&with).unwrap();
            assert_eq!(
                value["certificate"]["s_pairs"]["representations"]["rows"],
                3
            );
            assert_eq!(request.value(&without).unwrap_err(), S_PAIRS_MISSING);
            assert_eq!(
                representation.value(&with).unwrap_err(),
                S_PAIRS_UNREQUESTED
            );
            let mut shape = with;
            shape["certificate"]["s_pairs"]["representations"]["rows"] = json!(4);
            assert!(request.value(&shape).unwrap_err().contains("H is 4 x 2"));
        }
        let read: MembershipResult = serde_json::from_value(
            IdealRequest::parse(IDEAL_MEMBERSHIP, &with_polynomial("s_pairs", xw()))
                .unwrap()
                .value(&{
                    let mut r = not_in_ideal_reply();
                    r["certificate"]["s_pairs"] = s_pairs_reply();
                    r
                })
                .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            read.certificate,
            Some(Certificate::Given(MembershipCertificate::NotInIdeal {
                s_pairs: Some(_),
                ..
            }))
        ));
        // f in the ideal: the certificate is the coefficients, whatever the
        // job asked for, and holds no S-pairs.
        let request =
            IdealRequest::parse(IDEAL_MEMBERSHIP, &with_polynomial("s_pairs", xw())).unwrap();
        assert!(request.value(&in_ideal_reply()).is_ok());
        let mut with = in_ideal_reply();
        with["certificate"]["s_pairs"] = s_pairs_reply();
        assert!(request.value(&with).is_err());
    }

    #[test]
    fn s_pairs_read_and_write_their_json() {
        let n = json!({"type": "polynomial_matrix", "rows": 0, "columns": []});
        let s_pairs = json!({"pairs": [], "representations": n});
        let value = json!({
            "type": "groebner_basis", "basis": [],
            "certificate": {"basis_in_generators": n, "generators_in_basis": n,
                            "s_pairs": s_pairs},
        });
        let read: GroebnerBasisResult = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(&read).unwrap(), value);
        // Without S-pairs the key is not written.
        let bare = json!({
            "type": "groebner_basis", "basis": [],
            "certificate": {"basis_in_generators": n, "generators_in_basis": n},
        });
        let read: GroebnerBasisResult = serde_json::from_value(bare.clone()).unwrap();
        assert_eq!(serde_json::to_value(&read).unwrap(), bare);
        let zero = json!({"type": "polynomial", "terms": []});
        let not_in = json!({
            "type": "ideal_membership", "member": false,
            "certificate": {"basis": [], "generators_in_basis": n, "quotients": [],
                            "remainder": zero, "s_pairs": s_pairs},
        });
        let read: MembershipResult = serde_json::from_value(not_in.clone()).unwrap();
        assert_eq!(serde_json::to_value(&read).unwrap(), not_in);
        // The coefficients of f in the ideal take no S-pairs.
        assert!(
            serde_json::from_value::<MembershipResult>(json!({
                "type": "ideal_membership", "member": true,
                "certificate": {"coefficients": [], "s_pairs": s_pairs},
            }))
            .is_err()
        );
    }
}
