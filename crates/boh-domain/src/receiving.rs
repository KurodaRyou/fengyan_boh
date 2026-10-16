//! Receipt commands and the frozen GOODS_RECEIVED@2 payload.

use jiff::civil::{Date, date as civil_date};
use serde::{Deserialize, Serialize};

use crate::lot::LotId;
use crate::quantity::{Quantity, UnitFactor, convert_unit};
use crate::{AggregateId, CommandId, DomainError, UnixMillis};

const LAST_EXPIRES_ON: Date = civil_date(9998, 12, 31);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct ReceiptInput {
    pub qty: Quantity,
    pub unit_code: String,
    pub base_qty_per_unit: UnitFactor,
}
object_serde!(ReceiptInput);

impl ReceiptInput {
    pub fn base_qty(&self) -> Result<Quantity, DomainError> {
        if !(1..=9_999_999).contains(&self.qty.milli()) {
            return Err(DomainError::InvalidField("input.qty"));
        }
        self.base_qty_per_unit
            .validate()
            .map_err(|_| DomainError::InvalidField("input.base_qty_per_unit"))?;
        if self.unit_code.is_empty() || self.unit_code.trim() != self.unit_code {
            return Err(DomainError::InvalidField("input.unit_code"));
        }
        let qty = convert_unit(self.qty, self.base_qty_per_unit)?;
        if qty == Quantity::ZERO {
            return Err(DomainError::InvalidField("input.qty"));
        }
        Ok(qty)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptLine {
    pub item_id: AggregateId,
    pub input: ReceiptInput,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::present"
    )]
    pub manufacturer_lot_no: Option<String>,
    pub produced_on: String,
    pub expires_on: String,
    pub line_cost_cents: i64,
}

impl ReceiptLine {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_line(
            &self.input,
            self.manufacturer_lot_no.as_deref(),
            &self.produced_on,
            &self.expires_on,
            self.line_cost_cents,
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateReceipt {
    pub command_id: CommandId,
    pub supplier_id: AggregateId,
    pub lines: Vec<ReceiptLine>,
    pub occurred_at: UnixMillis,
    pub clock_synced_at: UnixMillis,
}

impl CreateReceipt {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.lines.is_empty() {
            return Err(DomainError::InvalidField("lines"));
        }
        for line in &self.lines {
            line.validate()?;
        }
        Ok(())
    }
}

// Field order is part of the published serialized payload. Receipt lines create
// new lots and are never absorbed by stocktakes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct ReceivedLine {
    pub item_id: AggregateId,
    pub qty: Quantity,
    pub input: ReceiptInput,
    pub lot_id: LotId,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::present"
    )]
    pub manufacturer_lot_no: Option<String>,
    pub produced_on: String,
    pub expires_on: String,
    pub expires_at: UnixMillis,
    pub line_cost_cents: i64,
}
object_serde!(ReceivedLine);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct GoodsReceived {
    pub supplier_id: AggregateId,
    pub lines: Vec<ReceivedLine>,
}
object_serde!(GoodsReceived);

impl GoodsReceived {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.lines.is_empty() {
            return Err(DomainError::InvalidField("lines"));
        }
        for line in &self.lines {
            validate_line(
                &line.input,
                line.manufacturer_lot_no.as_deref(),
                &line.produced_on,
                &line.expires_on,
                line.line_cost_cents,
            )?;
            if line.qty != line.input.base_qty()? {
                return Err(DomainError::InvalidField("qty"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub receipt_id: AggregateId,
    pub supplier_id: AggregateId,
    pub lines: Vec<ReceivedLine>,
    pub business_date: String,
    pub occurred_at: UnixMillis,
    pub recorded_at: UnixMillis,
    pub actor_id: AggregateId,
    pub device_id: AggregateId,
}

fn date(value: &str, field: &'static str) -> Result<Date, DomainError> {
    let invalid = || DomainError::InvalidField(field);
    let date = value.parse::<Date>().map_err(|_| invalid())?;
    if !(0..=9999).contains(&date.year()) || date.to_string() != value {
        return Err(invalid());
    }
    Ok(date)
}

fn validate_line(
    input: &ReceiptInput,
    manufacturer_lot_no: Option<&str>,
    produced_on: &str,
    expires_on: &str,
    line_cost_cents: i64,
) -> Result<(), DomainError> {
    input.base_qty()?;
    if !(0..=99_999_999).contains(&line_cost_cents) {
        return Err(DomainError::InvalidField("line_cost_cents"));
    }
    if let Some(lot_no) = manufacturer_lot_no
        && (lot_no.is_empty() || lot_no.trim() != lot_no || lot_no.chars().count() > 64)
    {
        return Err(DomainError::InvalidField("manufacturer_lot_no"));
    }
    let expires_on = date(expires_on, "expires_on")?;
    // expires_at is derived from the next local midnight, which must stay in range.
    if expires_on > LAST_EXPIRES_ON {
        return Err(DomainError::InvalidField("expires_on"));
    }
    if date(produced_on, "produced_on")? > expires_on {
        return Err(DomainError::InvalidField("produced_on"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ReceiptInput, ReceiptLine};
    use serde_json::json;

    fn input(qty: &str, factor: &str) -> ReceiptInput {
        serde_json::from_value(json!({
            "qty": qty, "unit_code": "unit", "base_qty_per_unit": factor
        }))
        .unwrap()
    }

    #[test]
    fn input_limits_apply_before_conversion_and_positive_values_cannot_round_to_zero() {
        assert_eq!(
            input("9999.999", "1000000000")
                .base_qty()
                .unwrap()
                .to_string(),
            "9999999000000"
        );
        assert_eq!(
            input("0.5", "0.001").base_qty().unwrap().to_string(),
            "0.001"
        );
        for (qty, factor) in [
            ("0", "1"),
            ("-0.001", "1"),
            ("10000", "0.001"),
            ("0.4", "0.001"),
            ("1", "0"),
            ("1", "-1"),
            ("1", "1000000000.000000001"),
        ] {
            assert!(input(qty, factor).base_qty().is_err(), "{qty} × {factor}");
        }
        for code in ["", " g", "g ", "\u{2003}g"] {
            let mut value = input("1", "1");
            value.unit_code = code.to_owned();
            assert!(value.base_qty().is_err(), "{code}");
        }
    }

    #[test]
    fn receipt_cost_limits_are_inclusive_integer_cents() {
        let mut line: ReceiptLine = serde_json::from_value(json!({
            "item_id": "01890a5d-ac96-774b-bcce-b302099a8501",
            "input": {"qty": "1", "unit_code": "g", "base_qty_per_unit": "1"},
            "produced_on": "2026-10-01", "expires_on": "2026-10-02", "line_cost_cents": 0
        }))
        .unwrap();
        assert!(line.validate().is_ok());
        line.line_cost_cents = 99_999_999;
        assert!(line.validate().is_ok());
        for cents in [-1, 100_000_000, i64::MAX] {
            line.line_cost_cents = cents;
            assert!(line.validate().is_err());
        }
    }
}
