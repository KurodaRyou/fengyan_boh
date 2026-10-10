//! Waste requests and the frozen WASTE_LOGGED@1 payload.

use serde::{Deserialize, Serialize};

use crate::lot::LotId;
use crate::receiving::ReceiptInput;
use crate::{AggregateId, CommandId, DomainError, EventId, UnixMillis};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct WasteRequestLine {
    pub item_id: AggregateId,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::present"
    )]
    pub lot_id: Option<LotId>,
    pub input: ReceiptInput,
    pub reason_code: String,
}
object_serde!(WasteRequestLine);

impl WasteRequestLine {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_input(&self.input, &self.reason_code)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct WasteCommandLine {
    pub item_id: AggregateId,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::present"
    )]
    pub lot_id: Option<LotId>,
    pub input: ReceiptInput,
    pub reason_code: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub confirm_shortage: bool,
}
object_serde!(WasteCommandLine);

impl WasteCommandLine {
    pub fn request_line(&self) -> WasteRequestLine {
        WasteRequestLine {
            item_id: self.item_id,
            lot_id: self.lot_id.clone(),
            input: self.input.clone(),
            reason_code: self.reason_code.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct LogWaste {
    pub command_id: CommandId,
    pub lines: Vec<WasteCommandLine>,
    pub captured_at: UnixMillis,
    pub sent_at: UnixMillis,
}
object_serde!(LogWaste);

impl LogWaste {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.lines.is_empty() {
            return Err(DomainError::InvalidField("lines"));
        }
        for line in &self.lines {
            line.request_line().validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct PrecheckWaste {
    pub lines: Vec<WasteRequestLine>,
}
object_serde!(PrecheckWaste);

impl PrecheckWaste {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AllocationSource {
    Specified,
    Fifo,
    Shortfall,
}

impl AllocationSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Specified => "SPECIFIED",
            Self::Fifo => "FIFO",
            Self::Shortfall => "SHORTFALL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct Allocation {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::present"
    )]
    pub lot_id: Option<LotId>,
    pub qty: i64,
    pub source: AllocationSource,
}
object_serde!(Allocation);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasteEffect {
    Alloc(Vec<Allocation>),
    Absorbed(EventId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", try_from = "WireWasteLine", into = "WireWasteLine")]
pub struct WasteLine {
    pub item_id: AggregateId,
    pub lot_id: Option<LotId>,
    pub qty: i64,
    pub input: ReceiptInput,
    pub reason_code: String,
    pub item_book_qty: i64,
    pub lot_book_qty: Option<i64>,
    pub effect: WasteEffect,
}
object_serde!(WasteLine);

impl WasteLine {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_input(&self.input, &self.reason_code)?;
        if self.qty != self.input.base_qty()? {
            return Err(DomainError::InvalidField("qty"));
        }
        if self.lot_id.is_some() != self.lot_book_qty.is_some() {
            return Err(DomainError::InvalidField("lot_book_qty"));
        }
        if let WasteEffect::Alloc(alloc) = &self.effect {
            if alloc.is_empty() {
                return Err(DomainError::InvalidField("alloc"));
            }
            let mut total = 0_i64;
            for (index, part) in alloc.iter().enumerate() {
                if part.qty <= 0
                    || (part.source == AllocationSource::Shortfall) != part.lot_id.is_none()
                {
                    return Err(DomainError::InvalidField("alloc"));
                }
                match &self.lot_id {
                    Some(lot)
                        if alloc.len() == 1
                            && part.source == AllocationSource::Specified
                            && part.lot_id.as_ref() == Some(lot) => {}
                    None if part.source == AllocationSource::Fifo => {}
                    None if part.source == AllocationSource::Shortfall
                        && Some(index) == alloc.len().checked_sub(1) => {}
                    _ => return Err(DomainError::InvalidField("alloc")),
                }
                total = total
                    .checked_add(part.qty)
                    .ok_or(DomainError::InvalidField("alloc"))?;
            }
            if total != self.qty {
                return Err(DomainError::InvalidField("alloc"));
            }
        }
        Ok(())
    }
}

// Keep wire fields explicit: flatten would weaken unknown-field and duplicate
// checks. The order here is the published payload's byte order.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireWasteLine {
    item_id: AggregateId,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::present"
    )]
    lot_id: Option<LotId>,
    qty: i64,
    input: ReceiptInput,
    reason_code: String,
    item_book_qty: i64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::present"
    )]
    lot_book_qty: Option<i64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::present"
    )]
    alloc: Option<Vec<Allocation>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::present"
    )]
    absorbed_by_event_id: Option<EventId>,
}

impl TryFrom<WireWasteLine> for WasteLine {
    type Error = DomainError;

    fn try_from(line: WireWasteLine) -> Result<Self, Self::Error> {
        let effect = match (line.alloc, line.absorbed_by_event_id) {
            (Some(alloc), None) => WasteEffect::Alloc(alloc),
            (None, Some(id)) => WasteEffect::Absorbed(id),
            _ => return Err(DomainError::InvalidField("alloc")),
        };
        let line = Self {
            item_id: line.item_id,
            lot_id: line.lot_id,
            qty: line.qty,
            input: line.input,
            reason_code: line.reason_code,
            item_book_qty: line.item_book_qty,
            lot_book_qty: line.lot_book_qty,
            effect,
        };
        line.validate()?;
        Ok(line)
    }
}

impl From<WasteLine> for WireWasteLine {
    fn from(line: WasteLine) -> Self {
        let (alloc, absorbed_by_event_id) = match line.effect {
            WasteEffect::Alloc(alloc) => (Some(alloc), None),
            WasteEffect::Absorbed(id) => (None, Some(id)),
        };
        Self {
            item_id: line.item_id,
            lot_id: line.lot_id,
            qty: line.qty,
            input: line.input,
            reason_code: line.reason_code,
            item_book_qty: line.item_book_qty,
            lot_book_qty: line.lot_book_qty,
            alloc,
            absorbed_by_event_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", try_from = "WireWasteLogged")]
pub struct WasteLogged {
    pub lines: Vec<WasteLine>,
}
object_serde!(WasteLogged);

impl WasteLogged {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireWasteLogged {
    lines: Vec<WasteLine>,
}

impl TryFrom<WireWasteLogged> for WasteLogged {
    type Error = DomainError;

    fn try_from(wire: WireWasteLogged) -> Result<Self, Self::Error> {
        let payload = Self { lines: wire.lines };
        payload.validate()?;
        Ok(payload)
    }
}

#[derive(Debug, Serialize)]
pub struct WasteRecord {
    pub waste_record_id: AggregateId,
    pub lines: Vec<WasteLine>,
    pub business_date: String,
    pub occurred_at: UnixMillis,
    pub recorded_at: UnixMillis,
    pub actor_id: AggregateId,
    pub device_id: AggregateId,
}

fn is_false(value: &bool) -> bool {
    !value
}

fn validate_input(input: &ReceiptInput, reason_code: &str) -> Result<(), DomainError> {
    input.base_qty()?;
    if reason_code.is_empty() || reason_code.trim() != reason_code {
        return Err(DomainError::InvalidField("reason_code"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{LogWaste, PrecheckWaste, WasteLogged};
    use serde_json::json;

    #[test]
    fn every_json_struct_boundary_requires_an_object() {
        let item = "01890a5d-ac96-774b-bcce-b302099a8501";
        let command = "01890a5d-ac96-774b-bcce-b302099a8502";
        let lot = "RAW-FLOUR-20261006-001";
        let input = json!({ "qty": 1, "unit_code": "g", "base_qty_per_unit": 1 });
        let alloc = json!([{ "lot_id": lot, "qty": 1, "source": "SPECIFIED" }]);
        let line = json!({
            "item_id": item, "lot_id": lot, "qty": 1, "input": input,
            "reason_code": "EXPIRED", "item_book_qty": 3, "lot_book_qty": 3, "alloc": alloc,
        });
        assert!(serde_json::from_value::<WasteLogged>(json!({ "lines": [line.clone()] })).is_ok());
        let positional_line = json!([item, lot, 1, input, "EXPIRED", 3, 3, alloc]);
        let mut positional_input = line.clone();
        positional_input["input"] = json!([1, "g", 1]);
        let mut positional_alloc = line.clone();
        positional_alloc["alloc"] = json!([[lot, 1, "SPECIFIED"]]);
        for payload in [
            json!([[line]]),
            json!({ "lines": [positional_line] }),
            json!({ "lines": [positional_input] }),
            json!({ "lines": [positional_alloc] }),
        ] {
            assert!(
                serde_json::from_value::<WasteLogged>(payload.clone()).is_err(),
                "{payload}"
            );
        }
        let request_line =
            json!({ "item_id": item, "lot_id": lot, "input": input, "reason_code": "EXPIRED" });
        let mut request_array_input = request_line.clone();
        request_array_input["input"] = json!([1, "g", 1]);
        for lines in [
            json!([[item, lot, input, "EXPIRED"]]),
            json!([request_array_input]),
        ] {
            assert!(serde_json::from_value::<PrecheckWaste>(json!({ "lines": lines })).is_err());
            assert!(
                serde_json::from_value::<LogWaste>(json!({
                    "command_id": command, "lines": lines, "captured_at": 0, "sent_at": 0,
                }))
                .is_err()
            );
        }
        assert!(serde_json::from_value::<PrecheckWaste>(json!([[request_line.clone()]])).is_err());
        assert!(
            serde_json::from_value::<LogWaste>(json!([command, [request_line], 0, 0])).is_err()
        );
    }

    #[test]
    fn payload_rejects_nulls_coerced_types_duplicates_and_invalid_allocations() {
        let good = r#"{"lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8501","qty":1,"input":{"qty":1,"unit_code":"g","base_qty_per_unit":1},"reason_code":"EXPIRED","item_book_qty":3,"alloc":[{"lot_id":"RAW-FLOUR-20261006-001","qty":1,"source":"FIFO"}]}]}"#;
        assert!(serde_json::from_str::<WasteLogged>(good).is_ok());
        for json in [
            good.replace("\"qty\":1", "\"qty\":true"),
            good.replace("\"qty\":1", "\"qty\":1.0"),
            good.replace("\"qty\":1", "\"qty\":null"),
            good.replace("\"qty\":1", "\"qty\":0"),
            good.replace("\"qty\":1", "\"qty\":-1"),
            good.replace("\"item_book_qty\":3", "\"item_book_qty\":null"),
            good.replace(
                "\"item_book_qty\":3",
                "\"item_book_qty\":3,\"item_book_qty\":3",
            ),
            good.replace("\"input\":", "\"lot_id\":null,\"input\":"),
            good.replace("\"input\":", "\"lot_book_qty\":null,\"input\":"),
            good.replace("\"alloc\":[", "\"absorbed_by_event_id\":null,\"alloc\":["),
            good.replace("\"alloc\":[", "\"alloc\":[null,"),
            good.replace("\"alloc\":[", "\"extra\":true,\"alloc\":["),
            good.replace("\"lot_id\":\"RAW-FLOUR-20261006-001\"", "\"lot_id\":null"),
            good.replace("\"source\":\"FIFO\"", "\"source\":\"SHORTFALL\""),
            good.replace("\"source\":\"FIFO\"", "\"source\":\"SPECIFIED\""),
            good.replace("\"source\":\"FIFO\"", "\"source\":\"NEW_LOT\""),
            good.replace("\"source\":\"FIFO\"", "\"source\":\"FIFO\",\"extra\":1"),
            good.replace(
                "\"source\":\"FIFO\"",
                "\"source\":\"FIFO\",\"source\":\"FIFO\"",
            ),
            good.replace("\"qty\":1,\"source\"", "\"qty\":2,\"source\""),
            good.replace(
                "\"qty\":1,\"source\"",
                "\"qty\":9223372036854775807,\"source\"",
            ),
            good.replace("\"base_qty_per_unit\":1", "\"base_qty_per_unit\":2"),
            good.replace(
                "\"reason_code\":\"EXPIRED\"",
                "\"reason_code\":\" EXPIRED\"",
            ),
            good.replace("\"lines\":[", "\"unknown\":1,\"lines\":["),
        ] {
            assert!(
                serde_json::from_str::<WasteLogged>(&json).is_err(),
                "{json}"
            );
        }
    }
}
