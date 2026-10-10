//! Waste projection: consume persisted effects without making allocation decisions.

use boh_domain::lot::LotId;
use boh_domain::waste::{WasteEffect, WasteLogged};
use boh_domain::{AggregateId, EventId};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::StorageError;
use crate::ledger::Event;

pub(crate) fn apply(tx: &Transaction<'_>, event: &Event) -> Result<(), StorageError> {
    if event.aggregate_version != 1 {
        return Err(invalid("unsupported waste aggregate version"));
    }
    let payload = payload(&event.payload)?;
    let seq: i64 = tx
        .query_row(
            "SELECT seq FROM store_events WHERE id = ?1",
            [event.id.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| StorageError::sqlite("查询报损事件序号", error))?;
    let mut movement_no = 0_i64;
    for line in payload.lines {
        match line.effect {
            WasteEffect::Absorbed(id) => {
                insert_movement(
                    tx,
                    event,
                    Movement {
                        seq,
                        number: movement_no,
                        item_id: line.item_id,
                        lot_id: None,
                        source: "ABSORBED",
                        qty: line.qty,
                        absorber: Some(id),
                    },
                )?;
                movement_no = next_movement(movement_no)?;
            }
            WasteEffect::Alloc(alloc) => {
                for part in alloc {
                    if let Some(lot_id) = &part.lot_id {
                        let current: i64 = tx
                            .query_row(
                                "SELECT remaining_qty FROM inventory_lots WHERE lot_id = ?1",
                                [lot_id.to_string()],
                                |row| row.get(0),
                            )
                            .map_err(|error| StorageError::sqlite("查询报损批次余量", error))?;
                        let remaining = current
                            .checked_sub(part.qty)
                            .ok_or_else(|| invalid("waste lot quantity overflow"))?;
                        tx.execute(
                            "UPDATE inventory_lots SET remaining_qty = ?2 WHERE lot_id = ?1",
                            params![lot_id.to_string(), remaining],
                        )
                        .map_err(|error| StorageError::sqlite("扣减报损批次余量", error))?;
                    } else {
                        let current: Option<i64> = tx
                            .query_row(
                                "SELECT qty FROM inventory_unallocated WHERE item_id = ?1",
                                [line.item_id.to_string()],
                                |row| row.get(0),
                            )
                            .optional()
                            .map_err(|error| StorageError::sqlite("查询报损账外缺口", error))?;
                        let remaining = current
                            .unwrap_or(0)
                            .checked_sub(part.qty)
                            .ok_or_else(|| invalid("waste unallocated quantity overflow"))?;
                        match current {
                            Some(_) => {
                                tx.execute(
                                    "UPDATE inventory_unallocated SET qty = ?2 WHERE item_id = ?1",
                                    params![line.item_id.to_string(), remaining],
                                )
                                .map_err(|error| StorageError::sqlite("扣减报损账外缺口", error))?;
                            }
                            None => {
                                tx.execute("INSERT INTO inventory_unallocated (item_id, qty) VALUES (?1, ?2)", params![line.item_id.to_string(), remaining])
                                    .map_err(|error| StorageError::sqlite("插入报损账外缺口", error))?;
                            }
                        }
                    }
                    insert_movement(
                        tx,
                        event,
                        Movement {
                            seq,
                            number: movement_no,
                            item_id: line.item_id,
                            lot_id: part.lot_id.as_ref(),
                            source: part.source.as_str(),
                            qty: part.qty,
                            absorber: None,
                        },
                    )?;
                    movement_no = next_movement(movement_no)?;
                }
            }
        }
    }
    Ok(())
}

struct Movement<'a> {
    seq: i64,
    number: i64,
    item_id: AggregateId,
    lot_id: Option<&'a LotId>,
    source: &'static str,
    qty: i64,
    absorber: Option<EventId>,
}

fn insert_movement(
    tx: &Transaction<'_>,
    event: &Event,
    row: Movement<'_>,
) -> Result<(), StorageError> {
    let nominal = row
        .qty
        .checked_neg()
        .ok_or_else(|| invalid("waste movement quantity overflow"))?;
    tx.execute(
        "INSERT INTO inventory_movements (event_seq, movement_no, item_id, lot_id, kind, alloc_source,
             nominal_qty, qty_delta, absorbed_by_event_id, physical_at, business_date)
         VALUES (?1, ?2, ?3, ?4, 'WASTE', ?5, ?6, ?7, ?8, ?9, ?10)",
        params![row.seq, row.number, row.item_id.to_string(), row.lot_id.map(ToString::to_string), row.source,
            nominal, if row.absorber.is_some() { 0 } else { nominal }, row.absorber.map(|id| id.to_string()),
            event.occurred_at.0, event.business_date],
    ).map_err(|error| StorageError::sqlite("插入报损库存流水", error))?;
    Ok(())
}

fn next_movement(number: i64) -> Result<i64, StorageError> {
    number
        .checked_add(1)
        .ok_or_else(|| invalid("waste movement number overflow"))
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
        let good = r#"{"lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8501","qty":1,"input":{"qty":1,"unit_code":"g","base_qty_per_unit":1},"reason_code":"EXPIRED","item_book_qty":3,"alloc":[{"lot_id":"RAW-FLOUR-20261006-001","qty":1,"source":"FIFO"}]}]}"#;
        assert_strict_payload(payload, good, &[], "解析报损事件 payload");
        let shortfall = good
            .replace("\"lot_id\":\"RAW-FLOUR-20261006-001\",", "")
            .replace("\"FIFO\"", "\"SHORTFALL\"");
        let specified = good
            .replace(
                "\"qty\":1,\"input\"",
                "\"lot_id\":\"RAW-FLOUR-20261006-001\",\"lot_book_qty\":3,\"qty\":1,\"input\"",
            )
            .replace("\"FIFO\"", "\"SPECIFIED\"");
        let absorbed = good.replace(
            "\"alloc\":[{\"lot_id\":\"RAW-FLOUR-20261006-001\",\"qty\":1,\"source\":\"FIFO\"}]",
            "\"absorbed_by_event_id\":\"01890a5d-ac96-774b-bcce-b302099a8502\"",
        );
        for json in [&shortfall, &specified, &absorbed] {
            assert_strict_payload(payload, json, &[], "解析报损事件 payload");
        }
        for json in [
            good.replace("\"qty\":1", "\"qty\":-1"),
            good.replace("\"input\":", "\"lot_id\":null,\"input\":"),
            good.replace("\"input\":", "\"lot_book_qty\":null,\"input\":"),
            good.replace("\"alloc\":[", "\"absorbed_by_event_id\":null,\"alloc\":["),
            good.replace(
                "\"alloc\":[",
                "\"absorbed_by_event_id\":\"01890a5d-ac96-774b-bcce-b302099a8502\",\"alloc\":[",
            ),
            good.replace("\"alloc\":[", "\"alloc\":[null,"),
            good.replace("\"source\":\"FIFO\"", "\"source\":\"SHORTFALL\""),
            good.replace("\"qty\":1,\"source\"", "\"qty\":2,\"source\""),
            good.replace("\"base_qty_per_unit\":1", "\"base_qty_per_unit\":2"),
            good.replace("\"lines\":[", "\"lines\":[false,"),
            "{\"lines\":[]}".into(),
        ] {
            assert!(payload(&json).is_err(), "{json}");
        }
    }
}
