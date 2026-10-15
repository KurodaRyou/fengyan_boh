//! Exact decimal quantities and unit factors.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::DomainError;

/// A signed quantity in thousandths of a unit. Field validation decides its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Quantity(i64);

impl Quantity {
    pub const ZERO: Self = Self(0);

    pub const fn from_milli(value: i64) -> Self {
        Self(value)
    }

    pub const fn milli(self) -> i64 {
        self.0
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    pub fn checked_neg(self) -> Option<Self> {
        self.0.checked_neg().map(Self)
    }
}

/// A conversion factor in billionths of a base unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnitFactor(i64);

impl UnitFactor {
    pub const ONE: Self = Self(1_000_000_000);

    pub const fn from_nano(value: i64) -> Self {
        Self(value)
    }

    pub const fn nano(self) -> i64 {
        self.0
    }

    pub fn validate(self) -> Result<(), DomainError> {
        if (1..=1_000_000_000_000_000_000).contains(&self.0) {
            Ok(())
        } else {
            Err(DomainError::InvalidField("base_qty_per_unit"))
        }
    }
}

/// Convert exactly, rounding only the final thousandth; ties go away from zero.
pub fn convert_unit(qty: Quantity, factor: UnitFactor) -> Result<Quantity, DomainError> {
    factor.validate()?;
    let invalid = || DomainError::InvalidField("qty");
    let product = i128::from(qty.milli())
        .checked_mul(i128::from(factor.nano()))
        .ok_or_else(invalid)?;
    let mut milli = product.checked_div(1_000_000_000).ok_or_else(invalid)?;
    let remainder = product.checked_rem(1_000_000_000).ok_or_else(invalid)?;
    if remainder.unsigned_abs() >= 500_000_000 {
        milli = milli.checked_add(product.signum()).ok_or_else(invalid)?;
    }
    i64::try_from(milli)
        .map(Quantity::from_milli)
        .map_err(|_| invalid())
}

fn parse_decimal(text: &str, places: u32) -> Option<i64> {
    let negative = text.starts_with('-');
    let magnitude = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = match magnitude.split_once('.') {
        Some((whole, fraction)) => {
            let digits = u32::try_from(fraction.len()).ok()?;
            if digits == 0 || digits > places || !fraction.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let fraction = fraction
                .parse::<i128>()
                .ok()?
                .checked_mul(10_i128.checked_pow(places.checked_sub(digits)?)?)?;
            (whole, fraction)
        }
        None => (magnitude, 0),
    };
    if whole.is_empty()
        || (whole.starts_with('0') && whole.len() != 1)
        || !whole.bytes().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let value = whole
        .parse::<i128>()
        .ok()?
        .checked_mul(10_i128.checked_pow(places)?)?
        .checked_add(fraction)?;
    if negative && value == 0 {
        return None;
    }
    i64::try_from(if negative {
        value.checked_neg()?
    } else {
        value
    })
    .ok()
}

fn format_decimal(value: i64, places: u32, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let scale = 10_u64.checked_pow(places).ok_or(fmt::Error)?;
    let magnitude = value.unsigned_abs();
    let whole = magnitude.checked_div(scale).ok_or(fmt::Error)?;
    let fraction = magnitude.checked_rem(scale).ok_or(fmt::Error)?;
    if value < 0 {
        f.write_str("-")?;
    }
    write!(f, "{whole}")?;
    if fraction != 0 {
        let width = usize::try_from(places).map_err(|_| fmt::Error)?;
        let digits = format!("{fraction:0width$}");
        write!(f, ".{}", digits.trim_end_matches('0'))?;
    }
    Ok(())
}

macro_rules! decimal_serde {
    ($name:ident, $places:literal) => {
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                format_decimal(self.0, $places, f)
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                parse_decimal(&text, $places)
                    .map(Self)
                    .ok_or_else(|| serde::de::Error::custom("invalid decimal string"))
            }
        }
    };
}

decimal_serde!(Quantity, 3);
decimal_serde!(UnitFactor, 9);

#[cfg(test)]
mod tests {
    use super::{Quantity, UnitFactor, convert_unit};

    #[test]
    fn decimal_strings_are_exact_and_canonical_at_i64_boundaries() {
        for (text, milli, canonical) in [
            ("0.000", 0, "0"),
            ("5.500", 5500, "5.5"),
            ("-0.001", -1, "-0.001"),
            ("-5.050", -5050, "-5.05"),
            ("9223372036854775.807", i64::MAX, "9223372036854775.807"),
            ("-9223372036854775.808", i64::MIN, "-9223372036854775.808"),
        ] {
            let value: Quantity = serde_json::from_value(text.into()).unwrap();
            assert_eq!(value.milli(), milli);
            assert_eq!(value.to_string(), canonical);
            assert_eq!(serde_json::to_value(value).unwrap(), canonical);
        }
        let factor: UnitFactor = serde_json::from_str("\"28.349523125\"").unwrap();
        assert_eq!(factor.nano(), 28_349_523_125);
        assert_eq!(factor.to_string(), "28.349523125");
        assert_eq!(UnitFactor::ONE.to_string(), "1");
    }

    #[test]
    fn decimal_parser_rejects_non_strings_invalid_forms_and_excess_precision() {
        for invalid in [
            "",
            "+1",
            "01",
            "-01",
            ".5",
            "5.",
            "1e3",
            " 1",
            "1 ",
            "-0",
            "-0.000",
            "0.0000",
            "1.0000",
            "--1",
            "1.2.3",
            "１",
            "1.１",
            "9223372036854775.808",
            "-9223372036854775.809",
            "999999999999999999999999999999999999999999999999999999999",
        ] {
            assert!(
                serde_json::from_value::<Quantity>(invalid.into()).is_err(),
                "{invalid}"
            );
        }
        for invalid in ["0", "1.25", "null", "true", "[]", "{}"] {
            assert!(
                serde_json::from_str::<Quantity>(invalid).is_err(),
                "{invalid}"
            );
            assert!(
                serde_json::from_str::<UnitFactor>(invalid).is_err(),
                "{invalid}"
            );
        }
        assert!(serde_json::from_str::<UnitFactor>("\"1.0000000000\"").is_err());
    }

    #[test]
    fn conversion_rounds_each_sign_away_from_zero_at_midpoints() {
        let mg = UnitFactor::from_nano(1_000_000);
        for (milli, expected) in [
            (400, 0),
            (500, 1),
            (1500, 2),
            (-400, 0),
            (-500, -1),
            (-1500, -2),
        ] {
            assert_eq!(
                convert_unit(Quantity::from_milli(milli), mg)
                    .unwrap()
                    .milli(),
                expected
            );
        }
        assert_eq!(
            convert_unit(
                Quantity::from_milli(1250),
                UnitFactor::from_nano(28_349_523_125)
            )
            .unwrap(),
            Quantity::from_milli(35_437)
        );
        // The intermediate product does not fit i64, although the rounded result does.
        assert_eq!(
            convert_unit(
                Quantity::from_milli(9_999_999),
                UnitFactor::from_nano(1_000_000_000_000_000_000)
            )
            .unwrap(),
            Quantity::from_milli(9_999_999_000_000_000)
        );
        assert!(
            convert_unit(
                Quantity::from_milli(i64::MAX),
                UnitFactor::from_nano(2_000_000_000)
            )
            .is_err()
        );
    }

    #[test]
    fn checked_quantity_arithmetic_does_not_wrap() {
        let one = Quantity::from_milli(1);
        assert_eq!(one.checked_sub(one), Some(Quantity::ZERO));
        assert_eq!(one.checked_neg(), Some(Quantity::from_milli(-1)));
        assert_eq!(Quantity::from_milli(i64::MAX).checked_add(one), None);
        assert_eq!(Quantity::from_milli(i64::MIN).checked_sub(one), None);
        assert_eq!(Quantity::from_milli(i64::MIN).checked_neg(), None);
    }
}
