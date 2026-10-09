//! Receipt commands and the frozen GOODS_RECEIVED@2 payload.

use jiff::civil::{Date, date as civil_date};
use serde::{Deserialize, Deserializer, Serialize};

use crate::lot::LotId;
use crate::{AggregateId, CommandId, DomainError, UnixMillis};

const LAST_EXPIRES_ON: Date = civil_date(9998, 12, 31);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptInput {
    pub qty: i64,
    pub unit_code: String,
    pub base_qty_per_unit: i64,
}

impl ReceiptInput {
    pub fn base_qty(&self) -> Result<i64, DomainError> {
        if self.qty <= 0 {
            return Err(DomainError::InvalidField("input.qty"));
        }
        if self.base_qty_per_unit <= 0 {
            return Err(DomainError::InvalidField("input.base_qty_per_unit"));
        }
        if self.unit_code.is_empty() || self.unit_code.trim() != self.unit_code {
            return Err(DomainError::InvalidField("input.unit_code"));
        }
        self.qty
            .checked_mul(self.base_qty_per_unit)
            .ok_or(DomainError::InvalidField("input.qty"))
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
        deserialize_with = "present_lot_no"
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
    pub captured_at: UnixMillis,
    pub sent_at: UnixMillis,
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

// Field order is part of the published serialized payload. Absorption is added
// with the stocktake slice; current receipts always create a lot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceivedLine {
    pub item_id: AggregateId,
    pub qty: i64,
    pub input: ReceiptInput,
    pub lot_id: LotId,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_lot_no"
    )]
    pub manufacturer_lot_no: Option<String>,
    pub produced_on: String,
    pub expires_on: String,
    pub expires_at: UnixMillis,
    pub line_cost_cents: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoodsReceived {
    pub supplier_id: AggregateId,
    pub lines: Vec<ReceivedLine>,
}

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

// A missing key defaults to None. A present key must contain a string, never null.
fn present_lot_no<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(Some)
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
    if line_cost_cents < 0 {
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
