use std::fmt;
use std::str::FromStr;

use num_bigint::BigInt;
use num_integer::Integer;
use num_rational::BigRational;
use num_traits::{One, Pow, Signed, Zero};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::MathError;

/// An exact rational number, always reduced with a positive denominator.
///
/// Serialized as a decimal string `"p"` or `"p/q"`; floating-point notation is rejected.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Rational(pub BigRational);

impl Rational {
    pub fn new(num: BigInt, den: BigInt) -> Result<Self, MathError> {
        if den.is_zero() {
            return Err(MathError::InvalidRational(format!("{num}/0")));
        }
        Ok(Self(BigRational::new(num, den)))
    }

    pub fn integer(value: impl Into<BigInt>) -> Self {
        Self(BigRational::from_integer(value.into()))
    }

    pub fn zero() -> Self {
        Self(BigRational::zero())
    }

    pub fn one() -> Self {
        Self(BigRational::one())
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    pub fn is_integer(&self) -> bool {
        self.0.is_integer()
    }

    pub fn is_negative(&self) -> bool {
        self.0.is_negative()
    }

    pub fn numer(&self) -> &BigInt {
        self.0.numer()
    }

    pub fn denom(&self) -> &BigInt {
        self.0.denom()
    }

    /// `self * base^exponent`, as a CAS answer's constant absorbs a constant
    /// factor of multiplicity `exponent` (Issue #91). The cost is bounded
    /// whatever `exponent` is: a base of 0, 1 or -1 needs no multiplication,
    /// other powers are taken by repeated squaring, and large ones are
    /// refused. `checkpoint` is called before the power and before the
    /// product; its error is returned in the outer result, and ends the
    /// computation.
    ///
    /// With `exponent` at least 2 and a base other than 0, 1 and -1, the bits
    /// of the power are taken from `budget`, what the powers of one answer
    /// may still have. A power that would take more is refused with
    /// [`MathError::TooLarge`] in the inner result, before it is taken when it
    /// surely would. What is left does not depend on the order of the
    /// factors. A constant given with multiplicity 1, such as the content of
    /// the polynomial, is multiplied in as it is and takes nothing.
    pub fn mul_pow_with<E>(
        &self,
        base: &Self,
        exponent: u32,
        budget: &mut PowerBudget,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Result<Self, MathError>, E> {
        if exponent == 0 || base.0.is_one() {
            return Ok(Ok(self.clone()));
        }
        // A zero constant so far does not skip later powers, so that what is
        // left of the budget does not depend on the order of the factors.
        if base.is_zero() {
            return Ok(Ok(Self::zero()));
        }
        if (-&base.0).is_one() {
            return Ok(Ok(if exponent.is_multiple_of(2) {
                self.clone()
            } else {
                Self(-&self.0)
            }));
        }
        checkpoint()?;
        if exponent == 1 {
            return Ok(Ok(Self(product(&self.0, &base.0))));
        }
        let integer = base.0.is_integer();
        let left = if integer {
            budget.integer
        } else {
            budget.fraction
        };
        // |n|^e has at least e * (bits(n) - 1) + 1 bits.
        let least =
            (base.numer().bits() - 1 + base.denom().bits() - 1).saturating_mul(u64::from(exponent));
        if least > left {
            return Ok(Err(too_large(least, integer, left)));
        }
        // The power of a reduced fraction is reduced; the denominator stays
        // positive.
        let power = BigRational::new_raw(
            Pow::pow(base.numer(), exponent),
            Pow::pow(base.denom(), exponent),
        );
        let bits = if integer {
            power.numer().bits()
        } else {
            power.numer().bits() + power.denom().bits()
        };
        if bits > left {
            return Ok(Err(too_large(bits, integer, left)));
        }
        if integer {
            budget.integer -= bits;
        } else {
            budget.fraction -= bits;
        }
        checkpoint()?;
        Ok(Ok(Self(product(&self.0, &power))))
    }
}

/// The bits that the powers taken by [`Rational::mul_pow_with`] for the
/// constant factors of one CAS answer may still have (Issue #91).
///
/// A CAS gives the content of a polynomial with multiplicity 1, or, over ZZ,
/// as powers of its primes (Macaulay2). The content divides the numerator of
/// every coefficient, and its prime powers have together at most twice its
/// bits. So the powers of integers may have twice the bits of the largest
/// numerator among the coefficients, and at least 2^16 bits; a product of
/// two integers costs about as much as the multiplication (with a fraction
/// so far, the gcd with its denominator still takes time quadratic in the
/// bits of the denominator). The powers of
/// fractions, which no CAS gives with a multiplicity above 1, may have 2^16
/// bits in their numerators and denominators together: a product with a
/// fraction reduces by gcds, whose cost grows with the square of the bits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerBudget {
    integer: u64,
    fraction: u64,
}

impl PowerBudget {
    /// The budget of an answer for a polynomial with these coefficients.
    pub fn for_coefficients<'a>(coefficients: impl IntoIterator<Item = &'a Rational>) -> Self {
        let largest = coefficients
            .into_iter()
            .map(|c| c.numer().bits())
            .max()
            .unwrap_or(0);
        Self {
            integer: largest.saturating_mul(2).max(MIN_POWER_BITS),
            fraction: MIN_POWER_BITS,
        }
    }
}

/// The bits that the powers of integers of one answer may have at least, and
/// the powers of fractions at most (about 19,700 decimal digits).
const MIN_POWER_BITS: u64 = 1 << 16;

fn too_large(bits: u64, integer: bool, left: u64) -> MathError {
    let kind = if integer { "an integer" } else { "a fraction" };
    MathError::TooLarge(format!(
        "a power of {kind} of at least {bits} bits, more than the {left} bits left for the powers of this answer"
    ))
}

/// `x * y`, reduced. As the product of [`BigRational`], but each gcd starts
/// with one division of the larger number by the smaller, and the result is
/// not reduced again. The binary gcd of `num-bigint` (its `Integer` for
/// `BigUint`) takes time quadratic in the larger number even when the smaller
/// is 1: about 1.5 s for an integer of 2^20 bits in a release build. The
/// product of [`BigRational`] takes three such gcds for two integers, the
/// last on the product, about 7 s in all for integers of 2^20 bits.
fn product(x: &BigRational, y: &BigRational) -> BigRational {
    fn gcd(a: &BigInt, b: &BigInt) -> BigInt {
        let (large, small) = if a.bits() >= b.bits() { (a, b) } else { (b, a) };
        if small.is_zero() {
            return large.abs();
        }
        small.gcd(&(large % small))
    }
    if x.is_zero() || y.is_zero() {
        return BigRational::zero();
    }
    let ad = gcd(x.numer(), y.denom());
    let bc = gcd(y.numer(), x.denom());
    // Both fractions are reduced with positive denominators, so the quotients
    // give a reduced fraction with a positive denominator.
    BigRational::new_raw(
        (x.numer() / &ad) * (y.numer() / &bc),
        (x.denom() / &bc) * (y.denom() / &ad),
    )
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_integer() {
            write!(f, "{}", self.0.numer())
        } else {
            write!(f, "{}/{}", self.0.numer(), self.0.denom())
        }
    }
}

fn parse_integer(s: &str) -> Option<BigInt> {
    let digits = s.strip_prefix(['-', '+']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    BigInt::from_str(s.strip_prefix('+').unwrap_or(s)).ok()
}

/// Parses `"p"` or `"p/q"` with decimal integers `p`, `q` and `q != 0`.
pub fn parse_rational(s: &str) -> Result<Rational, MathError> {
    let invalid = || MathError::InvalidRational(s.to_owned());
    match s.split_once('/') {
        None => parse_integer(s).map(Rational::integer).ok_or_else(invalid),
        Some((n, d)) => {
            let n = parse_integer(n).ok_or_else(invalid)?;
            let d = parse_integer(d).ok_or_else(invalid)?;
            Rational::new(n, d).map_err(|_| invalid())
        }
    }
}

impl FromStr for Rational {
    type Err = MathError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_rational(s)
    }
}

impl Serialize for Rational {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rational {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Text(String),
            Int(i64),
        }
        match Repr::deserialize(deserializer)? {
            Repr::Text(s) => parse_rational(&s).map_err(serde::de::Error::custom),
            Repr::Int(i) => Ok(Rational::integer(i)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalizes() {
        assert_eq!(parse_rational("4/-6").unwrap().to_string(), "-2/3");
        assert_eq!(parse_rational("+10/5").unwrap().to_string(), "2");
        assert_eq!(parse_rational("-0").unwrap(), Rational::zero());
        let r = parse_rational("6/4").unwrap();
        assert_eq!(r.numer(), &BigInt::from(3));
        assert_eq!(r.denom(), &BigInt::from(2));
    }

    #[test]
    fn rejects_non_exact_forms() {
        for bad in ["1.5", "1e3", "", "/", "1/0", " 1", "1/ 2", "0x10", "--1"] {
            assert!(parse_rational(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn serde_uses_strings() {
        let r: Rational = serde_json::from_str("\"-3/9\"").unwrap();
        assert_eq!(serde_json::to_string(&r).unwrap(), "\"-1/3\"");
        let i: Rational = serde_json::from_str("7").unwrap();
        assert_eq!(i, Rational::integer(7));
        assert!(serde_json::from_str::<Rational>("0.5").is_err());
    }

    /// Issue #91: `r * base^exponent` with a count of the checkpoints, with the
    /// budget of an input whose coefficients are small.
    fn mul_pow(r: &str, base: &str, exponent: u32) -> (Result<Rational, MathError>, usize) {
        let mut calls = 0;
        let result = parse_rational(r).unwrap().mul_pow_with(
            &parse_rational(base).unwrap(),
            exponent,
            &mut PowerBudget::for_coefficients([]),
            &mut || {
                calls += 1;
                Ok::<(), ()>(())
            },
        );
        (result.unwrap(), calls)
    }

    fn power_of_two(bits: u64) -> Rational {
        let mut n = BigInt::one();
        n <<= bits;
        Rational(BigRational::from_integer(n))
    }

    #[test]
    fn a_unit_or_zero_constant_of_any_multiplicity_needs_no_multiplication() {
        for (base, exponent, expected) in [
            ("1", u32::MAX, "3/4"),
            ("-1", u32::MAX, "-3/4"),
            ("-1", u32::MAX - 1, "3/4"),
            ("0", u32::MAX, "0"),
            ("5", 0, "3/4"),
            ("0", 0, "3/4"),
        ] {
            let (result, calls) = mul_pow("3/4", base, exponent);
            assert_eq!(result.unwrap().to_string(), expected, "{base}^{exponent}");
            assert_eq!(calls, 0, "{base}^{exponent}");
        }
    }

    #[test]
    fn a_power_equals_the_repeated_product() {
        for (r, base, exponent) in [
            ("3/4", "2", 1),
            ("3/4", "-2/3", 5),
            ("3/4", "7", 13),
            ("3/4", "-5/2", 64),
            ("4/9", "3/2", 2),
            ("-4/9", "9/8", 3),
            ("0", "3", 2),
            ("5", "1/5", 1),
            ("6", "-1/4", 1),
        ] {
            let (result, calls) = mul_pow(r, base, exponent);
            let mut expected = parse_rational(r).unwrap();
            for _ in 0..exponent {
                expected = Rational(&expected.0 * &parse_rational(base).unwrap().0);
            }
            assert_eq!(result.unwrap(), expected, "{r} * {base}^{exponent}");
            assert!(calls <= 2, "{calls}");
        }
    }

    #[test]
    fn the_product_is_reduced_with_a_positive_denominator() {
        for (x, y) in [
            ("6/35", "-14/9"),
            ("-6/35", "-14/9"),
            ("12", "1/18"),
            ("-12", "-7"),
            ("1", "-1/3"),
            ("0", "5/7"),
            ("5/7", "0"),
        ] {
            let x = parse_rational(x).unwrap().0;
            let y = parse_rational(y).unwrap().0;
            let z = product(&x, &y);
            assert_eq!(z, &x * &y);
            assert!(z.numer().gcd(z.denom()).is_one(), "{z}");
            assert!(z.denom().is_positive(), "{z}");
        }
    }

    #[test]
    fn a_raised_checkpoint_ends_the_power() {
        let mut calls = 0;
        let result = Rational::one().mul_pow_with(
            &Rational::integer(3),
            1 << 10,
            &mut PowerBudget::for_coefficients([]),
            &mut || {
                calls += 1;
                Err("raised")
            },
        );
        assert_eq!(result, Err("raised"));
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_power_of_too_many_bits_is_refused() {
        let mut ok = || Ok::<(), ()>(());
        // 3^(2^32 - 1) is refused before it is taken.
        let (result, calls) = mul_pow("1", "3", u32::MAX);
        assert!(matches!(result, Err(MathError::TooLarge(_))), "{result:?}");
        assert_eq!(calls, 1);
        // 2^(2^16) passes the first bound, but has 2^16 + 1 bits.
        let (result, _) = mul_pow("1", "2", 1 << 16);
        assert!(matches!(result, Err(MathError::TooLarge(_))), "{result:?}");
        // The powers of integers of one answer take from one budget:
        // (2^(2^14))^2 has 2^15 + 1 bits.
        let large = power_of_two(1 << 14);
        let mut budget = PowerBudget::for_coefficients([]);
        let square = Rational::one()
            .mul_pow_with(&large, 2, &mut budget, &mut ok)
            .unwrap()
            .unwrap();
        assert_eq!(budget.integer, (1 << 16) - (1 << 15) - 1);
        let again = square.mul_pow_with(&large, 2, &mut budget, &mut ok);
        assert!(
            matches!(again, Ok(Err(MathError::TooLarge(_)))),
            "{again:?}"
        );
        // The powers of fractions take from another.
        let small = Rational(BigRational::one() / &large.0);
        let mut budget = PowerBudget::for_coefficients([]);
        let square = Rational::one()
            .mul_pow_with(&small, 2, &mut budget, &mut ok)
            .unwrap()
            .unwrap();
        assert_eq!(budget.fraction, (1 << 16) - (1 << 15) - 2);
        assert_eq!(budget.integer, 1 << 16);
        let again = square.mul_pow_with(&small, 2, &mut budget, &mut ok);
        assert!(
            matches!(again, Ok(Err(MathError::TooLarge(_)))),
            "{again:?}"
        );
        // A constant of multiplicity 1 is neither bounded nor counted, so the
        // order of the factors does not matter.
        let content = power_of_two(1 << 17);
        let mut products = Vec::new();
        for order in [[(&content, 1), (&large, 2)], [(&large, 2), (&content, 1)]] {
            let mut budget = PowerBudget::for_coefficients([]);
            let mut product = Rational::one();
            for (base, exponent) in order {
                product = product
                    .mul_pow_with(base, exponent, &mut budget, &mut ok)
                    .unwrap()
                    .unwrap();
            }
            assert_eq!(budget.integer, (1 << 16) - (1 << 15) - 1);
            products.push(product);
        }
        assert_eq!(products[0], products[1]);
        assert_eq!(products[0].numer().bits(), (1 << 17) + (1 << 15) + 1);
        // A zero constant, before or after the powers, does not change what
        // they take.
        for order in [
            [
                (Rational::zero(), 1),
                (large.clone(), 2),
                (large.clone(), 2),
            ],
            [
                (large.clone(), 2),
                (large.clone(), 2),
                (Rational::zero(), 1),
            ],
        ] {
            let mut budget = PowerBudget::for_coefficients([]);
            let mut product = Ok(Rational::one());
            for (base, exponent) in &order {
                product = product.and_then(|p| {
                    p.mul_pow_with(base, *exponent, &mut budget, &mut ok)
                        .unwrap()
                });
            }
            assert!(
                matches!(product, Err(MathError::TooLarge(_))),
                "{product:?}"
            );
        }
    }

    /// The content of an input over ZZ, which Macaulay2 gives as powers of
    /// primes, is accepted however large the coefficients are; the powers of
    /// fractions keep the bound of 2^16 bits.
    #[test]
    fn the_budget_of_integer_powers_follows_the_input() {
        let mut ok = || Ok::<(), ()>(());
        // 2^(2^18) x: the content 2 with multiplicity 2^18.
        let coefficient = power_of_two(1 << 18);
        let mut budget = PowerBudget::for_coefficients([&coefficient]);
        let content = Rational::one()
            .mul_pow_with(&Rational::integer(2), 1 << 18, &mut budget, &mut ok)
            .unwrap()
            .unwrap();
        assert_eq!(content, coefficient);
        // Twice the bits of the largest numerator, and no more.
        assert_eq!(budget.integer, ((1 << 18) + 1) * 2 - ((1 << 18) + 1));
        let more = content.mul_pow_with(&Rational::integer(2), (1 << 18) + 1, &mut budget, &mut ok);
        assert!(matches!(more, Ok(Err(MathError::TooLarge(_)))), "{more:?}");
        let half = parse_rational("1/2").unwrap();
        let mut budget = PowerBudget::for_coefficients([&coefficient]);
        let fraction = Rational::one().mul_pow_with(&half, 1 << 16, &mut budget, &mut ok);
        assert!(
            matches!(fraction, Ok(Err(MathError::TooLarge(_)))),
            "{fraction:?}"
        );
    }

    /// A product with a large integer takes about as long as the
    /// multiplication. With the product of `BigRational`, the gcd with the
    /// other denominator, 1, took about 1.5 s for 2^20 bits in a release build,
    /// and the product took three such gcds. The binary gcd with 1 takes a
    /// step, a shift of the whole number, for each one bit in the binary
    /// expansion, so the integer is a power of 3, about half of whose bits are
    /// ones.
    #[test]
    fn a_large_integer_is_multiplied_in_without_a_quadratic_gcd() {
        let n: BigInt = Pow::pow(BigInt::from(3), 1u32 << 20);
        let expected = BigInt::from(5) * &n * &n * &n;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut ok = || Ok::<(), ()>(());
            let large = Rational(BigRational::from_integer(n));
            let mut budget = PowerBudget::for_coefficients([&large]);
            let content = large
                .mul_pow_with(&Rational::integer(5), 1, &mut budget, &mut ok)
                .unwrap()
                .unwrap();
            let cubed = content
                .mul_pow_with(&large, 2, &mut budget, &mut ok)
                .unwrap()
                .unwrap();
            let _ = tx.send(cubed);
        });
        let cubed = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the products took more than 10 s");
        assert_eq!(cubed.numer(), &expected);
        assert!(cubed.denom().is_one());
    }
}
