//! Exact decimal money representation.
//! CRITICAL: financial calculations in XFlow must never use f32/f64.

use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::ops::{Add, Mul, Sub};

/// Number of fractional digits in the canonical API/storage representation.
/// This matches the `NUMERIC(14,4)` scale used by every monetary column, so what the API
/// returns is exactly what PostgreSQL would store. Arithmetic itself is always exact and
/// unrounded; rounding happens only once, at the serialization boundary, using
/// half-away-from-zero (identical to PostgreSQL's NUMERIC rounding on insert).
pub const WIRE_SCALE: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Money(Decimal);

impl Money {
    pub fn zero() -> Self { Money(Decimal::ZERO) }
    pub fn from_str_exact(s: &str) -> Result<Self, rust_decimal::Error> {
        Ok(Money(s.parse::<Decimal>()?))
    }
    pub fn from_decimal(d: Decimal) -> Self { Money(d) }
    pub fn as_decimal(&self) -> Decimal { self.0 }
    pub fn is_zero(&self) -> bool { self.0 == Decimal::ZERO }
    pub fn is_negative(&self) -> bool { self.0 < Decimal::ZERO }
    pub fn abs(&self) -> Self { Money(self.0.abs()) }
    pub fn rounded_cents(&self) -> Self { Money(self.0.round_dp(2)) }

    /// Canonical wire string: always exactly `WIRE_SCALE` fractional digits, e.g. "4500.0000".
    pub fn to_wire_string(&self) -> String {
        let rounded = self.0.round_dp_with_strategy(WIRE_SCALE, RoundingStrategy::MidpointAwayFromZero);
        format!("{:.*}", WIRE_SCALE as usize, rounded)
    }
}

impl Serialize for Money {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_wire_string())
    }
}

impl<'de> Deserialize<'de> for Money {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        <Decimal as Deserialize>::deserialize(deserializer).map(Money)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${:.2}", self.0)
    }
}

impl Add for Money {
    type Output = Money;
    fn add(self, rhs: Self) -> Self::Output { Money(self.0 + rhs.0) }
}
impl Sub for Money {
    type Output = Money;
    fn sub(self, rhs: Self) -> Self::Output { Money(self.0 - rhs.0) }
}
impl Mul<Decimal> for Money {
    type Output = Money;
    fn mul(self, rhs: Decimal) -> Self::Output { Money(self.0 * rhs) }
}

/// Request-side decimal: monetary amounts and quantities MUST arrive as decimal strings (or
/// JSON integers). JSON floats are rejected outright, so no binary floating point value ever
/// enters the financial path. At most `WIRE_SCALE` fractional digits are accepted (the storage
/// scale), so nothing is silently rounded on insert, and the magnitude must fit NUMERIC(14,4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactDecimal(pub Decimal);

impl ExactDecimal {
    pub fn get(&self) -> Decimal { self.0 }

    pub fn parse(s: &str) -> Result<Self, String> {
        let d: Decimal = s.trim().parse().map_err(|_| format!("'{s}' is not a valid decimal number"))?;
        Self::validate(d)
    }

    fn validate(d: Decimal) -> Result<Self, String> {
        if d.normalize().scale() > WIRE_SCALE {
            return Err(format!("at most {WIRE_SCALE} decimal places are allowed"));
        }
        // NUMERIC(14,4) => at most 10 integer digits.
        if d.abs() >= Decimal::from(10_000_000_000i64) {
            return Err("value is too large".to_string());
        }
        Ok(ExactDecimal(d))
    }
}

impl<'de> Deserialize<'de> for ExactDecimal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = ExactDecimal;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a decimal string such as \"450.00\" (JSON floats are not accepted)")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> { ExactDecimal::parse(v).map_err(E::custom) }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> { ExactDecimal::validate(Decimal::from(v)).map_err(E::custom) }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> { ExactDecimal::validate(Decimal::from(v)).map_err(E::custom) }
            fn visit_f64<E: serde::de::Error>(self, _v: f64) -> Result<Self::Value, E> {
                Err(E::custom("monetary amounts and quantities must be sent as decimal strings, not floating point numbers"))
            }
        }
        deserializer.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_decimal_addition_has_no_float_drift() {
        let a = Money::from_str_exact("0.10").unwrap();
        let b = Money::from_str_exact("0.20").unwrap();
        let c = Money::from_str_exact("0.30").unwrap();
        assert_eq!(a + b, c);
    }

    #[test]
    fn quantity_times_rate_is_exact() {
        let rate = Money::from_str_exact("400.00").unwrap();
        let qty: Decimal = "10".parse().unwrap();
        let expected = Money::from_str_exact("4000.00").unwrap();
        assert_eq!(rate * qty, expected);
    }

    #[test]
    fn fractional_quantity_times_rate_is_exact() {
        let rate = Money::from_str_exact("120.50").unwrap();
        let qty: Decimal = "3.5".parse().unwrap();
        let expected = Money::from_str_exact("421.75").unwrap();
        assert_eq!(rate * qty, expected);
    }

    #[test]
    fn wire_format_always_has_exactly_four_decimals() {
        for (input, expected) in [("0", "0.0000"), ("5200", "5200.0000"), ("4500.00000000", "4500.0000"), ("0.30", "0.3000"), ("-12.5", "-12.5000")] {
            assert_eq!(Money::from_str_exact(input).unwrap().to_wire_string(), expected);
        }
    }

    #[test]
    fn wire_rounding_is_half_away_from_zero_and_only_at_the_boundary() {
        let m = Money::from_str_exact("0.00005").unwrap();
        assert_eq!(m.to_wire_string(), "0.0001");
        assert_eq!(Money::from_str_exact("-0.00005").unwrap().to_wire_string(), "-0.0001");
        // Internal value is untouched by serialization.
        assert_eq!(m.as_decimal(), "0.00005".parse::<Decimal>().unwrap());
    }

    #[test]
    fn serde_round_trip_is_exact_and_values_of_different_scale_serialize_identically() {
        let a = Money::from_str_exact("4500.00000000").unwrap();
        let b = Money::from_str_exact("4500.0000").unwrap();
        assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
        assert_eq!(serde_json::to_string(&a).unwrap(), "\"4500.0000\"");
        let back: Money = serde_json::from_str("\"4500.0000\"").unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn exact_decimal_accepts_strings_and_integers_and_rejects_floats() {
        assert_eq!(serde_json::from_str::<ExactDecimal>("\"450.25\"").unwrap().get(), "450.25".parse::<Decimal>().unwrap());
        assert_eq!(serde_json::from_str::<ExactDecimal>("10").unwrap().get(), Decimal::from(10));
        assert!(serde_json::from_str::<ExactDecimal>("450.25").is_err(), "JSON floats must be rejected");
        assert!(serde_json::from_str::<ExactDecimal>("\"abc\"").is_err());
    }

    #[test]
    fn exact_decimal_rejects_excess_precision_and_overflow() {
        assert!(ExactDecimal::parse("1.23456").is_err());
        assert!(ExactDecimal::parse("1.2300").is_ok());
        assert!(ExactDecimal::parse("10000000000").is_err());
        assert!(ExactDecimal::parse("9999999999.9999").is_ok());
        assert!(ExactDecimal::parse("-25.50").is_ok());
    }
}
