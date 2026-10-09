//! Event-only projections shared by online appends and replay.

use boh_domain::equipment::EquipmentType;
use boh_domain::receiving::{GoodsReceived, ReceiptInput, ReceivedLine};
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
        ("GOODS_RECEIVED", 1, "RECEIPT") => apply_receipt(tx, event),
        _ => Err(StorageError::InvalidEvent(
            "unsupported event type or version".into(),
        )),
    }
}

fn apply_equipment(tx: &Transaction<'_>, event: &Event) -> Result<(), StorageError> {
    // Decode only the event JSON, without consulting current master data or making decisions.
    let valid: bool = tx
        .query_row(
            "SELECT json_type(?1) = 'object'
          AND (SELECT count(*) FROM json_each(?1)) = 3
          AND json_extract(?1, '$.entity') = 'EQUIPMENT'
          AND json_extract(?1, '$.source') IN ('LOCAL', 'HQ_PACKAGE')
          AND json_type(?1, '$.snapshot') = 'object'
          AND (SELECT count(*) FROM json_each(?1, '$.snapshot')) = 4
          AND json_type(?1, '$.snapshot.code') = 'text'
          AND json_type(?1, '$.snapshot.name') = 'text'
          AND json_type(?1, '$.snapshot.equipment_type') = 'text'
          AND json_type(?1, '$.snapshot.active') IN ('true', 'false')",
            [&event.payload],
            |r| Ok(r.get::<_, Option<bool>>(0)?.unwrap_or(false)),
        )
        .map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
    if !valid {
        return Err(StorageError::InvalidEvent(
            "invalid equipment payload".into(),
        ));
    }
    let (code, name, kind, active): (String, String, String, bool) = tx.query_row(
        "SELECT json_extract(?1, '$.snapshot.code'), json_extract(?1, '$.snapshot.name'),
                json_extract(?1, '$.snapshot.equipment_type'), json_extract(?1, '$.snapshot.active')",
        [&event.payload],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
    EquipmentType::parse(&kind)?;
    // Full snapshots overwrite the projection, independent of its previous contents.
    let values = params![
        event.aggregate_id.to_string(),
        code,
        name,
        kind,
        active,
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
    let valid: bool = tx
        .query_row(
            "SELECT json_type(?1) = 'object'
          AND (SELECT count(*) FROM json_each(?1)) =
              CASE WHEN json_type(?1, '$.note') IS NULL THEN 2 ELSE 3 END
          AND NOT EXISTS (SELECT 1 FROM json_each(?1)
                          WHERE key NOT IN ('equipment_id', 'celsius_x10', 'note'))
          AND json_type(?1, '$.equipment_id') = 'text'
          AND json_type(?1, '$.celsius_x10') = 'integer'
          AND (json_type(?1, '$.note') IS NULL OR json_type(?1, '$.note') = 'text')",
            [&event.payload],
            |row| Ok(row.get::<_, Option<bool>>(0)?.unwrap_or(false)),
        )
        .map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
    if !valid {
        return Err(StorageError::InvalidEvent(
            "invalid temperature payload".into(),
        ));
    }
    let (equipment_id, celsius_x10, note): (String, i64, Option<String>) = tx
        .query_row(
            "SELECT json_extract(?1, '$.equipment_id'), json_extract(?1, '$.celsius_x10'),
                json_extract(?1, '$.note')",
            [&event.payload],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
    let payload = TemperatureLogged {
        equipment_id: AggregateId::parse(&equipment_id)?,
        celsius_x10,
        note,
    };
    payload.validate()?;
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
    let payload = receipt_payload(tx, &event.payload)?;
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
            "INSERT INTO inventory_lots (lot_id, item_id, origin, source_event_seq,
                 source_line_no, remaining_qty, expires_at, manufacturer_lot_no)
             VALUES (?1, ?2, 'RECEIPT', ?3, ?4, ?5, ?6, ?7)",
            params![
                line.lot_id.to_string(),
                line.item_id.to_string(),
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

fn receipt_payload(tx: &Transaction<'_>, json: &str) -> Result<GoodsReceived, StorageError> {
    // Validate JSON types before extraction: SQLite otherwise coerces booleans
    // and floating-point numbers when retrieving integer columns.
    let valid: bool = tx
        .query_row(
            "SELECT json_type(?1) = 'object'
          AND (SELECT count(*) FROM json_each(?1)) = 2
          AND json_type(?1, '$.supplier_id') = 'text'
          AND json_type(?1, '$.lines') = 'array'
          AND json_array_length(?1, '$.lines') > 0
          AND NOT EXISTS (
              SELECT 1 FROM json_each(?1, '$.lines') AS line
              WHERE (CASE WHEN line.type = 'object' THEN
                  (SELECT count(*) FROM json_each(line.value)) =
                      CASE WHEN json_type(line.value, '$.manufacturer_lot_no') IS NULL
                           THEN 8 ELSE 9 END
                  AND json_type(line.value, '$.item_id') = 'text'
                  AND json_type(line.value, '$.qty') = 'integer'
                  AND json_type(line.value, '$.input') = 'object'
                  AND (SELECT count(*) FROM json_each(line.value, '$.input')) = 3
                  AND json_type(line.value, '$.input.qty') = 'integer'
                  AND json_type(line.value, '$.input.unit_code') = 'text'
                  AND json_type(line.value, '$.input.base_qty_per_unit') = 'integer'
                  AND json_type(line.value, '$.lot_id') = 'text'
                  AND (json_type(line.value, '$.manufacturer_lot_no') IS NULL
                       OR json_type(line.value, '$.manufacturer_lot_no') = 'text')
                  AND json_type(line.value, '$.produced_on') = 'text'
                  AND json_type(line.value, '$.expires_on') = 'text'
                  AND json_type(line.value, '$.expires_at') = 'integer'
                  AND json_type(line.value, '$.line_cost_cents') = 'integer'
              ELSE 0 END) IS NOT 1)",
            [json],
            |row| Ok(row.get::<_, Option<bool>>(0)?.unwrap_or(false)),
        )
        .map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
    if !valid {
        return Err(StorageError::InvalidEvent("invalid receipt payload".into()));
    }
    let invalid = |error: boh_domain::DomainError| StorageError::external("校验收货事件", error);
    let supplier_id: String = tx
        .query_row("SELECT json_extract(?1, '$.supplier_id')", [json], |row| {
            row.get(0)
        })
        .map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
    let mut statement = tx
        .prepare(
            "SELECT json_extract(value, '$.item_id'), json_extract(value, '$.qty'),
                json_extract(value, '$.input.qty'), json_extract(value, '$.input.unit_code'),
                json_extract(value, '$.input.base_qty_per_unit'), json_extract(value, '$.lot_id'),
                json_extract(value, '$.manufacturer_lot_no'), json_extract(value, '$.produced_on'),
                json_extract(value, '$.expires_on'), json_extract(value, '$.expires_at'),
                json_extract(value, '$.line_cost_cents')
         FROM json_each(?1, '$.lines') ORDER BY key",
        )
        .map_err(|error| StorageError::sqlite("准备查询事件 JSON", error))?;
    let lines = statement
        .query_map([json], read_received_line)
        .map_err(|error| StorageError::sqlite("查询收货事件行", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StorageError::sqlite("解码收货事件行", error))?;
    let payload = GoodsReceived {
        supplier_id: AggregateId::parse(&supplier_id).map_err(invalid)?,
        lines,
    };
    payload.validate().map_err(invalid)?;
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

fn read_received_line(row: &Row<'_>) -> rusqlite::Result<ReceivedLine> {
    Ok(ReceivedLine {
        item_id: row_id(row, 0, AggregateId::parse)?,
        qty: row.get(1)?,
        input: ReceiptInput {
            qty: row.get(2)?,
            unit_code: row.get(3)?,
            base_qty_per_unit: row.get(4)?,
        },
        lot_id: row_id(row, 5, AggregateId::parse)?,
        manufacturer_lot_no: row.get(6)?,
        produced_on: row.get(7)?,
        expires_on: row.get(8)?,
        expires_at: UnixMillis(row.get(9)?),
        line_cost_cents: row.get(10)?,
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
    use super::{PROJECTION_TABLES, receipt_payload};
    use crate::{StorageError, open};
    use std::num::NonZeroUsize;

    #[tokio::test]
    async fn receipt_decoder_rejects_coerced_types_missing_keys_and_non_object_lines() {
        let dir = tempfile::tempdir().unwrap();
        let storage = open(&dir.path().join("boh.db"), NonZeroUsize::MIN).unwrap();
        let good = r#"{"supplier_id":"01890a5d-ac96-774b-bcce-b302099a8601","lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8602","qty":5,"input":{"qty":5,"unit_code":"g","base_qty_per_unit":1},"lot_id":"01890a5d-ac96-774b-bcce-b302099a8701","produced_on":"2026-10-01","expires_on":"2026-10-20","expires_at":1792511999999,"line_cost_cents":0}]}"#;
        storage
            .writer
            .call(move |tx| -> Result<(), StorageError> {
                assert_eq!(receipt_payload(tx, good)?.lines.len(), 1);
                for json in [
                    good.replace("\"qty\":5", "\"qty\":true"),
                    good.replace("\"qty\":5", "\"qty\":5.0"),
                    good.replace("\"lot_id\":", "\"lot\":"),
                    good.replace("\"qty\":5,\"input\"", "\"qty\":6,\"input\""),
                    good.replace("\"expires_at\":1792511999999", "\"expires_at\":null"),
                    good.replace("\"line_cost_cents\":0", "\"line_cost_cents\":0,\"extra\":1"),
                    good.replace("\"lines\":[", "\"lines\":[\"text\","),
                    good.replace("\"lines\":[", "\"lines\":[null,"),
                ] {
                    assert!(
                        matches!(
                            receipt_payload(tx, &json),
                            Err(StorageError::InvalidEvent(_) | StorageError::External { .. })
                        ),
                        "{json}"
                    );
                }
                Ok(())
            })
            .await
            .unwrap();
        storage.writer_handle.shutdown().await.unwrap();
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
