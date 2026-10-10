//! Event-only projections shared by online appends and replay.

use boh_domain::equipment::EquipmentChanged;
use boh_domain::receiving::GoodsReceived;
use boh_domain::temperature::TemperatureLogged;
use boh_domain::{AggregateId, CommandId, EventId, UnixMillis};
use rusqlite::{Row, Transaction, params};

use crate::StorageError;
use crate::ledger::Event;

// The schema comparison test must cover every projection introduced by a migration.
const PROJECTION_TABLES: &[&str] = &[
    "equipment",
    "temperature_readings",
    "items",
    "item_units",
    "recipes",
    "recipe_versions",
    "recipe_lines",
    "suppliers",
    "waste_reasons",
    "inventory_lots",
    "inventory_unallocated",
    "inventory_movements",
    "inventory_counts",
];

pub(crate) fn apply(tx: &Transaction<'_>, event: &Event) -> Result<(), StorageError> {
    match (
        event.event_type.as_str(),
        event.schema_version,
        event.aggregate_type.as_str(),
    ) {
        ("MASTER_DATA_CHANGED", 1, "EQUIPMENT") => apply_equipment(tx, event),
        ("MASTER_DATA_CHANGED", 1, "ITEM" | "RECIPE" | "SUPPLIER" | "WASTE_REASON") => {
            crate::master_data::apply(tx, event)
        }
        ("TEMPERATURE_LOGGED", 1, "TEMPERATURE_READING") => apply_temperature(tx, event),
        ("GOODS_RECEIVED", 2, "RECEIPT") => apply_receipt(tx, event),
        ("WASTE_LOGGED", 1, "WASTE_RECORD") => crate::waste::apply(tx, event),
        _ => Err(StorageError::InvalidEvent(
            "unsupported event type or version".into(),
        )),
    }
}

fn apply_equipment(tx: &Transaction<'_>, event: &Event) -> Result<(), StorageError> {
    let payload = equipment_payload(&event.payload)?;
    let snapshot = payload.snapshot;
    // Full snapshots overwrite the projection, independent of its previous contents.
    let values = params![
        event.aggregate_id.to_string(),
        snapshot.code,
        snapshot.name,
        snapshot.equipment_type.as_str(),
        snapshot.active,
        event.aggregate_version
    ];
    let updated = tx
        .execute(
            "UPDATE equipment SET code = ?2, name = ?3, equipment_type = ?4, active = ?5,
                              revision = ?6 WHERE id = ?1",
            values,
        )
        .map_err(|error| StorageError::sqlite("更新设备", error))?;
    if updated == 0 {
        tx.execute(
            "INSERT INTO equipment (id, code, name, equipment_type, active, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            values,
        )
        .map_err(|error| StorageError::sqlite("插入设备", error))?;
    }
    Ok(())
}

fn apply_temperature(tx: &Transaction<'_>, event: &Event) -> Result<(), StorageError> {
    if event.aggregate_version != 1 {
        return Err(StorageError::InvalidEvent(
            "unsupported temperature aggregate version".into(),
        ));
    }
    let payload = temperature_payload(&event.payload)?;
    // The stored event supplies seq in both the online and replay paths.
    let inserted = tx
        .execute(
            "INSERT INTO temperature_readings (id, event_seq, equipment_id, celsius_x10, note,
             actor_id, device_id, business_date, occurred_at, recorded_at)
         SELECT ?1, seq, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9 FROM store_events WHERE id = ?10",
            params![
                event.aggregate_id.to_string(),
                payload.equipment_id.to_string(),
                payload.celsius_x10,
                payload.note,
                event.actor_id.to_string(),
                event.device_id.to_string(),
                event.business_date,
                event.occurred_at.0,
                event.recorded_at.0,
                event.id.to_string()
            ],
        )
        .map_err(|error| StorageError::sqlite("插入温度记录", error))?;
    if inserted != 1 {
        return Err(StorageError::InvalidEvent(
            "temperature event not found".into(),
        ));
    }
    Ok(())
}

fn apply_receipt(tx: &Transaction<'_>, event: &Event) -> Result<(), StorageError> {
    if event.aggregate_version != 1 {
        return Err(StorageError::InvalidEvent(
            "unsupported receipt aggregate version".into(),
        ));
    }
    let payload = receipt_payload(&event.payload)?;
    // seq comes from the stored event in both the online and replay paths.
    let seq: i64 = tx
        .query_row(
            "SELECT seq FROM store_events WHERE id = ?1",
            [event.id.to_string()],
            |row| row.get(0),
        )
        .map_err(|error| StorageError::sqlite("查询事件账本", error))?;
    for (index, line) in payload.lines.into_iter().enumerate() {
        let index = i64::try_from(index)
            .map_err(|_| StorageError::InvalidEvent("receipt line number overflow".into()))?;
        tx.execute(
            "INSERT INTO inventory_lots (lot_id, item_id, lot_date, lot_serial, origin, source_event_seq,
                 source_line_no, remaining_qty, expires_at, manufacturer_lot_no)
             VALUES (?1, ?2, ?3, ?4, 'RECEIPT', ?5, ?6, ?7, ?8, ?9)",
            params![
                line.lot_id.to_string(),
                line.item_id.to_string(),
                line.lot_id.date().to_string(),
                line.lot_id.serial(),
                seq,
                index,
                line.qty,
                line.expires_at.0,
                line.manufacturer_lot_no
            ],
        )
        .map_err(|error| StorageError::sqlite("插入库存批次", error))?;
        tx.execute(
            "INSERT INTO inventory_movements (event_seq, movement_no, item_id, lot_id,
                 kind, alloc_source, nominal_qty, qty_delta, absorbed_by_event_id,
                 physical_at, business_date)
             VALUES (?1, ?2, ?3, ?4, 'RECEIPT', 'NEW_LOT', ?5, ?5, NULL, ?6, ?7)",
            params![
                seq,
                index,
                line.item_id.to_string(),
                line.lot_id.to_string(),
                line.qty,
                event.occurred_at.0,
                event.business_date
            ],
        )
        .map_err(|error| StorageError::sqlite("插入库存流水", error))?;
    }
    Ok(())
}

fn equipment_payload(json: &str) -> Result<EquipmentChanged, StorageError> {
    serde_json::from_str(json)
        .map_err(|error| StorageError::external("解析设备事件 payload", error))
}

fn temperature_payload(json: &str) -> Result<TemperatureLogged, StorageError> {
    let payload: TemperatureLogged = serde_json::from_str(json)
        .map_err(|error| StorageError::external("解析温度事件 payload", error))?;
    payload
        .validate()
        .map_err(|error| StorageError::external("校验温度事件 payload", error))?;
    Ok(payload)
}

fn receipt_payload(json: &str) -> Result<GoodsReceived, StorageError> {
    let payload: GoodsReceived = serde_json::from_str(json)
        .map_err(|error| StorageError::external("解析收货事件 payload", error))?;
    payload
        .validate()
        .map_err(|error| StorageError::external("校验收货事件 payload", error))?;
    Ok(payload)
}

fn row_id<T>(
    row: &Row<'_>,
    index: usize,
    parse: impl FnOnce(&str) -> Result<T, boh_domain::DomainError>,
) -> rusqlite::Result<T> {
    parse(&row.get::<_, String>(index)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn read_event(row: &Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: row_id(row, 0, EventId::parse)?,
        event_type: row.get(1)?,
        schema_version: row.get(2)?,
        aggregate_type: row.get(3)?,
        aggregate_id: row_id(row, 4, AggregateId::parse)?,
        aggregate_version: row.get(5)?,
        command_id: row_id(row, 6, CommandId::parse)?,
        actor_id: row_id(row, 7, AggregateId::parse)?,
        device_id: row_id(row, 8, AggregateId::parse)?,
        business_date: row.get(9)?,
        occurred_at: UnixMillis(row.get(10)?),
        recorded_at: UnixMillis(row.get(11)?),
        payload: row.get(12)?,
    })
}

pub(crate) fn rebuild(tx: &Transaction<'_>) -> Result<u64, StorageError> {
    for table in PROJECTION_TABLES {
        tx.execute(&format!("DELETE FROM \"{table}\""), [])
            .map_err(|error| StorageError::sqlite("清空投影表", error))?;
    }
    let mut statement = tx.prepare(
        "SELECT id, event_type, schema_version, aggregate_type, aggregate_id, aggregate_version,
                command_id, actor_id, device_id, business_date, occurred_at, recorded_at, payload, seq
         FROM store_events ORDER BY seq",
    ).map_err(|error| StorageError::sqlite("准备查询事件账本", error))?;
    let mut rows = statement
        .query([])
        .map_err(|error| StorageError::sqlite("查询事件账本", error))?;
    let mut count = 0_u64;
    while let Some(row) = rows
        .next()
        .map_err(|error| StorageError::sqlite("读取重放事件", error))?
    {
        let seq: i64 = row
            .get(13)
            .map_err(|error| StorageError::sqlite("解码重放事件序号", error))?;
        let event_type: String = row
            .get(1)
            .map_err(|error| StorageError::sqlite("解码重放事件类型", error))?;
        let schema_version: i64 = row
            .get(2)
            .map_err(|error| StorageError::sqlite("解码重放事件版本", error))?;
        let result = read_event(row)
            .map_err(|error| StorageError::sqlite("解码重放事件", error))
            .and_then(|event| apply(tx, &event));
        result.map_err(|error| StorageError::rebuild(seq, event_type, schema_version, error))?;
        count = count
            .checked_add(1)
            .ok_or_else(|| StorageError::InvalidEvent("replay count overflow".into()))?;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::{PROJECTION_TABLES, equipment_payload, receipt_payload, temperature_payload};
    use crate::tests::assert_strict_payload;
    use crate::{StorageError, open};
    use std::num::NonZeroUsize;

    #[test]
    fn equipment_decoder_rejects_coerced_types_nulls_duplicates_and_unknown_fields() {
        let good = r#"{"entity":"EQUIPMENT","source":"LOCAL","snapshot":{"code":"F1","name":"Fridge","equipment_type":"FRIDGE","active":true}}"#;
        assert_strict_payload(equipment_payload, good, &[], "解析设备事件 payload");
        for json in [
            good.replace("\"EQUIPMENT\"", r#"{"EQUIPMENT":null}"#),
            good.replace("\"LOCAL\"", r#"{"LOCAL":null}"#),
            good.replace("\"FRIDGE\"", r#"{"FRIDGE":null}"#),
            good.replace("\"FRIDGE\"", "\"UNKNOWN\""),
            good.replace(r#"{"code":"F1","name":"Fridge","equipment_type":"FRIDGE","active":true}"#, r#"["F1","Fridge","FRIDGE",true]"#),
            r#"["EQUIPMENT","LOCAL",{"code":"F1","name":"Fridge","equipment_type":"FRIDGE","active":true}]"#.into(),
        ] {
            assert!(equipment_payload(&json).is_err(), "{json}");
        }
        assert!(equipment_payload(&good.replace("LOCAL", "HQ_PACKAGE")).is_ok());
        for kind in [
            "FRIDGE",
            "FREEZER",
            "BLAST_FREEZER",
            "OVEN",
            "PROOFER",
            "MIXER",
            "OTHER",
        ] {
            let payload = equipment_payload(&good.replace("FRIDGE", kind)).unwrap();
            assert_eq!(payload.snapshot.equipment_type.as_str(), kind);
        }
    }

    #[test]
    fn temperature_decoder_rejects_coerced_types_nulls_duplicates_and_unknown_fields() {
        let good = r#"{"equipment_id":"01890a5d-ac96-774b-bcce-b302099a8301","celsius_x10":-185,"note":"Door seal"}"#;
        assert_strict_payload(temperature_payload, good, &["note"], "解析温度事件 payload");
        assert!(temperature_payload(&good.replace(r#","note":"Door seal""#, "")).is_ok());
        for json in [
            good.replace("-185", "5001"),
            good.replace("Door seal", " Door seal"),
            good.replace("Door seal", ""),
            good.replace("Door seal", &"x".repeat(201)),
            good.replace(
                "01890a5d-ac96-774b-bcce-b302099a8301",
                "01890a5d-ac96-474b-bcce-b302099a8301",
            ),
            r#"["01890a5d-ac96-774b-bcce-b302099a8301",-185,"Door seal"]"#.into(),
        ] {
            assert!(temperature_payload(&json).is_err(), "{json}");
        }
    }

    #[test]
    fn receipt_decoder_rejects_coerced_types_nulls_duplicates_and_unknown_fields() {
        let good = r#"{"supplier_id":"01890a5d-ac96-774b-bcce-b302099a8601","lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8602","qty":5,"input":{"qty":5,"unit_code":"g","base_qty_per_unit":1},"lot_id":"RAW-FLOUR-20261006-001","manufacturer_lot_no":"M1","produced_on":"2026-10-01","expires_on":"2026-10-20","expires_at":1792511999999,"line_cost_cents":0}]}"#;
        assert_strict_payload(
            receipt_payload,
            good,
            &["manufacturer_lot_no"],
            "解析收货事件 payload",
        );
        for json in [
            good.replace(r#""qty":5,"input""#, r#""qty":6,"input""#),
            good.replace(r#""lines":["#, r#""lines":["text","#),
            good.replace(r#""lines":["#, r#""lines":[null,"#),
            good.replace(r#"{"qty":5,"unit_code":"g","base_qty_per_unit":1}"#, r#"[5,"g",1]"#),
            good.replace(r#"{"item_id":"01890a5d-ac96-774b-bcce-b302099a8602","qty":5,"input":{"qty":5,"unit_code":"g","base_qty_per_unit":1},"lot_id":"RAW-FLOUR-20261006-001","manufacturer_lot_no":"M1","produced_on":"2026-10-01","expires_on":"2026-10-20","expires_at":1792511999999,"line_cost_cents":0}"#, r#"["01890a5d-ac96-774b-bcce-b302099a8602",5,{"qty":5,"unit_code":"g","base_qty_per_unit":1},"RAW-FLOUR-20261006-001","M1","2026-10-01","2026-10-20",1792511999999,0]"#),
            format!(r#"["01890a5d-ac96-774b-bcce-b302099a8601",{}]"#, serde_json::from_str::<serde_json::Value>(good).unwrap()["lines"]),
            good.replace("RAW-FLOUR-20261006-001", "RAW-FLOUR-20261006-000"),
            good.replace("2026-10-01", "2026-10-21"),
            good.replace("M1", " M1"),
            good.replace(r#""base_qty_per_unit":1"#, r#""base_qty_per_unit":9223372036854775807"#),
            good.replace(r#""line_cost_cents":0"#, r#""line_cost_cents":-1"#),
        ] {
            assert!(receipt_payload(&json).is_err(), "{json}");
        }
        assert!(
            receipt_payload(r#"{"supplier_id":"01890a5d-ac96-774b-bcce-b302099a8601","lines":[]}"#)
                .is_err()
        );
    }

    #[tokio::test]
    async fn rebuild_table_list_matches_migrated_schema() {
        let dir = tempfile::tempdir().unwrap();
        let storage = open(&dir.path().join("boh.db"), NonZeroUsize::MIN).unwrap();
        let tables: Vec<String> = storage
            .readers
            .call(|conn| -> Result<_, StorageError> {
                // Authentication state tables must join this exclusion list when introduced.
                let mut statement = conn
                    .prepare(
                        "SELECT name FROM sqlite_master
                     WHERE type = 'table' AND name NOT GLOB 'sqlite_*'
                       AND name NOT IN ('store_meta', 'processed_commands', 'store_events')
                     ORDER BY name COLLATE BINARY",
                    )
                    .map_err(|error| StorageError::sqlite("准备查询数据库表", error))?;
                statement
                    .query_map([], |row| row.get(0))
                    .map_err(|error| StorageError::sqlite("查询数据库表", error))?
                    .collect::<Result<_, _>>()
                    .map_err(|error| StorageError::sqlite("读取数据库表", error))
            })
            .await
            .unwrap();
        storage.writer_handle.shutdown().await.unwrap();
        let mut expected = PROJECTION_TABLES.to_vec();
        expected.sort_unstable();
        assert_eq!(
            tables, expected,
            "rebuild must clear every projection table"
        );
    }
}
