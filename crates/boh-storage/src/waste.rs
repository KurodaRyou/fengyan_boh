//! Waste projection: consume persisted effects without making allocation decisions.

use boh_domain::waste::{WasteEffect, WasteLogged};
use rusqlite::Transaction;

use crate::StorageError;
use crate::ledger::Event;

pub(crate) fn apply(tx: &Transaction<'_>, event: &Event) -> Result<(), StorageError> {
    if event.aggregate_version != 1 {
        return Err(invalid("unsupported waste aggregate version"));
    }
    let payload = payload(&event.payload)?;
    let mut movements =
        crate::inventory::Movements::new(tx, event, crate::inventory::MovementKind::Waste)?;
    for line in payload.lines {
        match line.effect {
            WasteEffect::Absorbed(id) => movements.absorbed(line.item_id, line.qty, id)?,
            WasteEffect::Alloc(alloc) => movements.deduct(line.item_id, &alloc)?,
        }
    }
    Ok(())
}

fn invalid(message: &str) -> StorageError {
    StorageError::InvalidEvent(message.into())
}

fn payload(json: &str) -> Result<WasteLogged, StorageError> {
    serde_json::from_str::<WasteLogged>(json)
        .map_err(|error| StorageError::external("解析报损事件 payload", error))
}

#[cfg(test)]
mod tests {
    use super::payload;
    use crate::tests::assert_strict_payload;

    #[test]
    fn decoder_rejects_coerced_types_nulls_duplicates_and_unknown_fields() {
        let good = r#"{"lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8501","qty":"1","input":{"qty":"1","unit_code":"g","base_qty_per_unit":"1"},"reason_code":"EXPIRED","item_book_qty":"3","alloc":[{"lot_id":"RAW-FLOUR-20261006-001","qty":"1","source":"FIFO"}]}]}"#;
        assert_strict_payload(payload, good, &[], "解析报损事件 payload");
        let shortfall = good
            .replace("\"lot_id\":\"RAW-FLOUR-20261006-001\",", "")
            .replace("\"FIFO\"", "\"SHORTFALL\"");
        let specified = good
            .replace(
                "\"qty\":\"1\",\"input\"",
                "\"lot_id\":\"RAW-FLOUR-20261006-001\",\"lot_book_qty\":\"3\",\"qty\":\"1\",\"input\"",
            )
            .replace("\"FIFO\"", "\"SPECIFIED\"");
        let absorbed = good.replace(
            "\"alloc\":[{\"lot_id\":\"RAW-FLOUR-20261006-001\",\"qty\":\"1\",\"source\":\"FIFO\"}]",
            "\"absorbed_by_event_id\":\"01890a5d-ac96-774b-bcce-b302099a8502\"",
        );
        for json in [&shortfall, &specified, &absorbed] {
            assert_strict_payload(payload, json, &[], "解析报损事件 payload");
        }
        for json in [
            good.replace("\"qty\":\"1\"", "\"qty\":\"-1\""),
            good.replace("\"input\":", "\"lot_id\":null,\"input\":"),
            good.replace("\"input\":", "\"lot_book_qty\":null,\"input\":"),
            good.replace("\"alloc\":[", "\"absorbed_by_event_id\":null,\"alloc\":["),
            good.replace(
                "\"alloc\":[",
                "\"absorbed_by_event_id\":\"01890a5d-ac96-774b-bcce-b302099a8502\",\"alloc\":[",
            ),
            good.replace("\"alloc\":[", "\"alloc\":[null,"),
            good.replace("\"source\":\"FIFO\"", "\"source\":\"SHORTFALL\""),
            good.replace("\"qty\":\"1\",\"source\"", "\"qty\":\"2\",\"source\""),
            good.replace("\"base_qty_per_unit\":\"1\"", "\"base_qty_per_unit\":\"2\""),
            good.replace("\"lines\":[", "\"lines\":[false,"),
            "{\"lines\":[]}".into(),
        ] {
            assert!(payload(&json).is_err(), "{json}");
        }
    }
}
