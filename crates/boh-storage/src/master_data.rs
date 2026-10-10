//! Decode frozen domain snapshots, then project only their stored facts.

use boh_domain::master_data::{
    ItemSnapshot, MasterDataChanged, RecipeSnapshot, SupplierSnapshot, WasteReasonSnapshot,
};
use rusqlite::{Transaction, params};

use crate::{StorageError, ledger::Event};

fn invalid() -> StorageError {
    StorageError::InvalidEvent("invalid master data payload".into())
}

fn payload(json: &str) -> Result<MasterDataChanged, StorageError> {
    let payload: MasterDataChanged = serde_json::from_str(json)
        .map_err(|error| StorageError::external("解析主数据事件 payload", error))?;
    match &payload {
        MasterDataChanged::Item { snapshot, .. } => snapshot.validate(),
        MasterDataChanged::Recipe { snapshot, .. } => snapshot.validate(),
        MasterDataChanged::Supplier { snapshot, .. } => snapshot.validate(),
        MasterDataChanged::WasteReason { snapshot, .. } => snapshot.validate(),
    }
    .map_err(|error| StorageError::external("校验主数据事件 payload", error))?;
    Ok(payload)
}

pub(crate) fn apply(tx: &Transaction<'_>, event: &Event) -> Result<(), StorageError> {
    if event.aggregate_version <= 0 {
        return Err(invalid());
    }
    match (payload(&event.payload)?, event.aggregate_type.as_str()) {
        (MasterDataChanged::Item { snapshot, .. }, "ITEM") => item(tx, event, &snapshot),
        (MasterDataChanged::Recipe { snapshot, .. }, "RECIPE") => recipe(tx, event, &snapshot),
        (MasterDataChanged::Supplier { snapshot, .. }, "SUPPLIER") => {
            supplier(tx, event, &snapshot)
        }
        (MasterDataChanged::WasteReason { snapshot, .. }, "WASTE_REASON") => {
            waste_reason(tx, event, &snapshot)
        }
        _ => Err(invalid()),
    }
}

fn item(tx: &Transaction<'_>, event: &Event, snapshot: &ItemSnapshot) -> Result<(), StorageError> {
    let values = params![
        event.aggregate_id.to_string(),
        snapshot.code,
        snapshot.name,
        snapshot.base_unit.as_str(),
        snapshot.category.as_str(),
        snapshot.default_shelf_life_ms,
        snapshot.active,
        event.aggregate_version
    ];
    if tx
        .execute(
            "UPDATE items SET code = ?2, name = ?3, base_unit = ?4, category = ?5,
        default_shelf_life_ms = ?6, active = ?7, revision = ?8 WHERE id = ?1",
            values,
        )
        .map_err(|error| StorageError::sqlite("更新物料", error))?
        == 0
    {
        tx.execute("INSERT INTO items (id, code, name, base_unit, category, default_shelf_life_ms, active, revision)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", values).map_err(|error| StorageError::sqlite("插入物料", error))?;
    }
    tx.execute(
        "DELETE FROM item_units WHERE item_id = ?1",
        [event.aggregate_id.to_string()],
    )
    .map_err(|error| StorageError::sqlite("删除物料单位", error))?;
    for unit in &snapshot.units {
        tx.execute(
            "INSERT INTO item_units (item_id, unit_code, base_qty_per_unit) VALUES (?1, ?2, ?3)",
            params![
                event.aggregate_id.to_string(),
                unit.unit_code,
                unit.base_qty_per_unit
            ],
        )
        .map_err(|error| StorageError::sqlite("插入物料单位", error))?;
    }
    Ok(())
}

fn recipe(
    tx: &Transaction<'_>,
    event: &Event,
    snapshot: &RecipeSnapshot,
) -> Result<(), StorageError> {
    let values = params![
        event.aggregate_id.to_string(),
        snapshot.code,
        snapshot.name,
        snapshot.output_item_id.to_string(),
        snapshot.active,
        event.aggregate_version
    ];
    if tx.execute("UPDATE recipes SET code = ?2, name = ?3, output_item_id = ?4, active = ?5, revision = ?6 WHERE id = ?1", values).map_err(|error| StorageError::sqlite("更新配方", error))? == 0 {
        tx.execute("INSERT INTO recipes (id, code, name, output_item_id, active, revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", values).map_err(|error| StorageError::sqlite("插入配方", error))?;
    }
    for version in &snapshot.versions {
        let inserted = tx
            .execute(
                "INSERT INTO recipe_versions (recipe_id, version, output_qty_per_batch)
             SELECT ?1, ?2, ?3 WHERE NOT EXISTS (
                 SELECT 1 FROM recipe_versions WHERE recipe_id = ?1 AND version = ?2
             )",
                params![
                    event.aggregate_id.to_string(),
                    version.version,
                    version.output_qty_per_batch
                ],
            )
            .map_err(|error| StorageError::sqlite("插入配方版本", error))?;
        if inserted == 0 {
            continue;
        }
        for (index, line) in version.lines.iter().enumerate() {
            let line_no = i64::try_from(index).map_err(|_| invalid())?;
            tx.execute("INSERT INTO recipe_lines (recipe_id, version, line_no, item_id, qty_per_batch) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![event.aggregate_id.to_string(), version.version, line_no, line.item_id.to_string(), line.qty_per_batch]).map_err(|error| StorageError::sqlite("插入配方明细", error))?;
        }
    }
    Ok(())
}

fn supplier(
    tx: &Transaction<'_>,
    event: &Event,
    snapshot: &SupplierSnapshot,
) -> Result<(), StorageError> {
    let values = params![
        event.aggregate_id.to_string(),
        snapshot.code,
        snapshot.name,
        snapshot.contact_phone,
        snapshot.active,
        event.aggregate_version
    ];
    if tx.execute("UPDATE suppliers SET code = ?2, name = ?3, contact_phone = ?4, active = ?5, revision = ?6 WHERE id = ?1", values).map_err(|error| StorageError::sqlite("更新供应商", error))? == 0 {
        tx.execute("INSERT INTO suppliers (id, code, name, contact_phone, active, revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", values).map_err(|error| StorageError::sqlite("插入供应商", error))?;
    }
    Ok(())
}

fn waste_reason(
    tx: &Transaction<'_>,
    event: &Event,
    snapshot: &WasteReasonSnapshot,
) -> Result<(), StorageError> {
    let values = params![
        event.aggregate_id.to_string(),
        snapshot.code,
        snapshot.name,
        snapshot.active,
        event.aggregate_version
    ];
    if tx.execute(
        "UPDATE waste_reasons SET code = ?2, name = ?3, active = ?4, revision = ?5 WHERE id = ?1",
        values,
    ).map_err(|error| StorageError::sqlite("更新报损原因", error))? == 0
    {
        tx.execute("INSERT INTO waste_reasons (id, code, name, active, revision) VALUES (?1, ?2, ?3, ?4, ?5)", values).map_err(|error| StorageError::sqlite("插入报损原因", error))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use boh_domain::{AggregateId, CommandId, EventId, UnixMillis};

    use super::{apply, payload};
    use crate::tests::assert_strict_payload;
    use crate::{StorageError, ledger::Event, open};

    #[test]
    fn item_decoder_rejects_coerced_types_nulls_duplicates_and_unknown_fields() {
        let good = r#"{"entity":"ITEM","source":"LOCAL","snapshot":{"code":"FLOUR","name":"Flour","base_unit":"g","category":"RAW","default_shelf_life_ms":1000,"units":[{"unit_code":"bag","base_qty_per_unit":25000}],"active":true}}"#;
        assert_strict_payload(
            payload,
            good,
            &["default_shelf_life_ms"],
            "解析主数据事件 payload",
        );
        for json in [
            good.replace("\"ITEM\"", r#"{"ITEM":null}"#),
            good.replace("\"LOCAL\"", r#"{"LOCAL":null}"#),
            good.replace("\"g\"", r#"{"g":null}"#),
            good.replace("\"RAW\"", r#"{"RAW":null}"#),
            good.replace("\"RAW\"", "\"UNKNOWN\""),
            good.replace(r#"{"unit_code":"bag","base_qty_per_unit":25000}"#, r#"["bag",25000]"#),
            good.replace(r#"{"code":"FLOUR","name":"Flour","base_unit":"g","category":"RAW","default_shelf_life_ms":1000,"units":[{"unit_code":"bag","base_qty_per_unit":25000}],"active":true}"#, r#"["FLOUR","Flour","g","RAW",1000,[{"unit_code":"bag","base_qty_per_unit":25000}],true]"#),
            good.replace(r#""units":["#, r#""units":[null,"#),
            good.replace("25000", "0"),
            good.replace("1000", "0"),
        ] {
            assert!(payload(&json).is_err(), "{json}");
        }
        assert!(payload(&good.replace("LOCAL", "HQ_PACKAGE")).is_ok());
    }

    #[test]
    fn recipe_decoder_rejects_coerced_types_nulls_duplicates_and_unknown_fields() {
        let good = r#"{"entity":"RECIPE","source":"LOCAL","snapshot":{"code":"BREAD","name":"Bread","output_item_id":"01890a5d-ac96-774b-bcce-b302099a8301","versions":[{"version":1,"output_qty_per_batch":10,"lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8302","qty_per_batch":100}]}],"active":true}}"#;
        assert_strict_payload(payload, good, &[], "解析主数据事件 payload");
        for json in [
            good.replace("\"LOCAL\"", r#"{"LOCAL":null}"#),
            good.replace(r#"{"item_id":"01890a5d-ac96-774b-bcce-b302099a8302","qty_per_batch":100}"#, r#"["01890a5d-ac96-774b-bcce-b302099a8302",100]"#),
            good.replace(r#"{"version":1,"output_qty_per_batch":10,"lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8302","qty_per_batch":100}]}"#, r#"[1,10,[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8302","qty_per_batch":100}]]"#),
            good.replace(r#"{"code":"BREAD","name":"Bread","output_item_id":"01890a5d-ac96-774b-bcce-b302099a8301","versions":[{"version":1,"output_qty_per_batch":10,"lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8302","qty_per_batch":100}]}],"active":true}"#, r#"["BREAD","Bread","01890a5d-ac96-774b-bcce-b302099a8301",[{"version":1,"output_qty_per_batch":10,"lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8302","qty_per_batch":100}]}],true]"#),
            good.replace(r#""versions":["#, r#""versions":[true,"#),
            good.replace(r#""lines":["#, r#""lines":[null,"#),
            good.replace(r#""version":1"#, r#""version":2"#),
            good.replace(r#""qty_per_batch":100"#, r#""qty_per_batch":0"#),
        ] {
            assert!(payload(&json).is_err(), "{json}");
        }
        assert!(payload(&good.replace("LOCAL", "HQ_PACKAGE")).is_ok());
    }

    #[test]
    fn supplier_decoder_rejects_coerced_types_nulls_duplicates_and_unknown_fields() {
        let good = r#"{"entity":"SUPPLIER","source":"LOCAL","snapshot":{"code":"S1","name":"Supplier","contact_phone":"021-5555","active":true}}"#;
        assert_strict_payload(payload, good, &["contact_phone"], "解析主数据事件 payload");
        for json in [
            good.replace("\"LOCAL\"", r#"{"LOCAL":null}"#),
            good.replace(
                r#"{"code":"S1","name":"Supplier","contact_phone":"021-5555","active":true}"#,
                r#"["S1","Supplier","021-5555",true]"#,
            ),
            good.replace("021-5555", ""),
            good.replace("021-5555", " 021-5555"),
        ] {
            assert!(payload(&json).is_err(), "{json}");
        }
        assert!(payload(&good.replace("LOCAL", "HQ_PACKAGE")).is_ok());
    }

    #[test]
    fn waste_reason_decoder_rejects_coerced_types_nulls_duplicates_and_unknown_fields() {
        let good = r#"{"entity":"WASTE_REASON","source":"LOCAL","snapshot":{"code":"EXPIRED","name":"Expired","active":true}}"#;
        assert_strict_payload(payload, good, &[], "解析主数据事件 payload");
        for json in [
            good.replace("\"LOCAL\"", r#"{"LOCAL":null}"#),
            good.replace(
                r#"{"code":"EXPIRED","name":"Expired","active":true}"#,
                r#"["EXPIRED","Expired",true]"#,
            ),
            good.replace("Expired", ""),
            good.replace("Expired", " Expired"),
        ] {
            assert!(payload(&json).is_err(), "{json}");
        }
        assert!(payload(&good.replace("LOCAL", "HQ_PACKAGE")).is_ok());
    }

    #[tokio::test]
    async fn master_data_rejects_mismatched_entities_and_nonpositive_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let storage = open(&dir.path().join("boh.db"), NonZeroUsize::MIN).unwrap();
        storage.writer.call(|tx| -> Result<(), StorageError> {
            let id = "01890a5d-ac96-774b-bcce-b302099a8301";
            let mut event = Event {
                id: EventId::parse(id)?,
                event_type: "MASTER_DATA_CHANGED".into(),
                schema_version: 1,
                aggregate_type: String::new(),
                aggregate_id: AggregateId::parse(id)?,
                aggregate_version: 1,
                command_id: CommandId::parse(id)?,
                actor_id: AggregateId::parse(id)?,
                device_id: AggregateId::parse(id)?,
                business_date: "2026-10-07".into(),
                occurred_at: UnixMillis(0),
                recorded_at: UnixMillis(0),
                payload: String::new(),
            };
            for (entity, snapshot) in [
                ("ITEM", r#"{"code":"FLOUR","name":"Flour","base_unit":"g","category":"RAW","units":[],"active":true}"#),
                ("RECIPE", r#"{"code":"BREAD","name":"Bread","output_item_id":"01890a5d-ac96-774b-bcce-b302099a8301","versions":[{"version":1,"output_qty_per_batch":1,"lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8301","qty_per_batch":1}]}],"active":true}"#),
                ("SUPPLIER", r#"{"code":"S1","name":"Supplier","active":true}"#),
                ("WASTE_REASON", r#"{"code":"EXPIRED","name":"Expired","active":true}"#),
            ] {
                event.payload = format!(r#"{{"entity":"{entity}","source":"LOCAL","snapshot":{snapshot}}}"#);
                assert!(payload(&event.payload).is_ok());
                for aggregate_type in ["ITEM", "RECIPE", "SUPPLIER", "WASTE_REASON"] {
                    event.aggregate_type = aggregate_type.into();
                    event.aggregate_version = 1;
                    if aggregate_type != entity {
                        assert!(matches!(apply(tx, &event), Err(StorageError::InvalidEvent(_))));
                    }
                    for revision in [0, -1] {
                        event.aggregate_version = revision;
                        assert!(matches!(apply(tx, &event), Err(StorageError::InvalidEvent(_))));
                    }
                }
            }
            Ok(())
        }).await.unwrap();
        storage.writer_handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn recipe_updates_and_additions_do_not_rewrite_existing_versions()
    -> Result<(), StorageError> {
        let dir = tempfile::tempdir().map_err(|error| StorageError::io("创建测试目录", error))?;
        let storage = open(&dir.path().join("boh.db"), NonZeroUsize::MIN)?;
        storage
            .writer
            .call(|tx| -> Result<(), StorageError> {
                let item_id = "01890a5d-ac96-774b-bcce-b302099a8301";
                let recipe_id = "01890a5d-ac96-774b-bcce-b302099a8302";
                tx.execute(
                    "INSERT INTO items (id, code, name, base_unit, category, active, revision)
                     VALUES (?1, 'ITEM', 'Item', 'g', 'RAW', 1, 1)",
                    [item_id],
                ).map_err(|error| StorageError::sqlite("插入物料", error))?;
                let mut event = Event {
                    id: EventId::parse(recipe_id)?,
                    event_type: "MASTER_DATA_CHANGED".into(),
                    schema_version: 1,
                    aggregate_type: "RECIPE".into(),
                    aggregate_id: AggregateId::parse(recipe_id)?,
                    aggregate_version: 1,
                    command_id: CommandId::parse(recipe_id)?,
                    actor_id: AggregateId::parse(recipe_id)?,
                    device_id: AggregateId::parse(recipe_id)?,
                    business_date: "2026-10-07".into(),
                    occurred_at: UnixMillis(0),
                    recorded_at: UnixMillis(0),
                    payload: String::new(),
                };
                let v1 = format!(
                    r#"{{"version":1,"output_qty_per_batch":10,"lines":[{{"item_id":"{item_id}","qty_per_batch":100}}]}}"#
                );
                let v2 = format!(
                    r#"{{"version":2,"output_qty_per_batch":20,"lines":[{{"item_id":"{item_id}","qty_per_batch":200}}]}}"#
                );
                let snapshot = |name: &str, versions: &str| {
                    format!(
                        r#"{{"entity":"RECIPE","source":"LOCAL","snapshot":{{"code":"RECIPE","name":"{name}","output_item_id":"{item_id}","versions":[{versions}],"active":true}}}}"#
                    )
                };
                event.payload = snapshot("Original", &v1);
                apply(tx, &event)?;

                // Catch delete/reinsert cycles even when the final rows would be identical.
                for table in ["recipe_versions", "recipe_lines"] {
                    for (operation, row) in [("INSERT", "NEW"), ("UPDATE", "OLD"), ("DELETE", "OLD")] {
                        tx.execute_batch(&format!(
                            "CREATE TEMP TRIGGER preserve_{table}_{operation}
                             BEFORE {operation} ON {table} WHEN {row}.version = 1
                             BEGIN SELECT RAISE(ABORT, 'existing recipe version changed'); END;"
                        )).map_err(|error| StorageError::sqlite("执行数据库", error))?;
                    }
                }

                event.aggregate_version = 2;
                event.payload = snapshot("Renamed", &v1);
                apply(tx, &event)?;
                event.aggregate_version = 3;
                event.payload = snapshot("Renamed", &format!("{v1},{v2}"));
                apply(tx, &event)?;

                let mut statement = tx.prepare(
                    "SELECT v.version, v.output_qty_per_batch, l.line_no, l.qty_per_batch
                     FROM recipe_versions v JOIN recipe_lines l USING (recipe_id, version)
                     WHERE v.recipe_id = ?1 ORDER BY v.version, l.line_no",
                ).map_err(|error| StorageError::sqlite("准备查询配方版本", error))?;
                let rows: Vec<(i64, i64, i64, i64)> = statement
                    .query_map([recipe_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(|error| StorageError::sqlite("查询配方版本", error))?
                    .collect::<Result<_, _>>().map_err(|error| StorageError::sqlite("读取配方版本", error))?;
                assert_eq!(rows, [(1, 10, 0, 100), (2, 20, 0, 200)]);
                let metadata: (String, i64) = tx.query_row(
                    "SELECT name, revision FROM recipes WHERE id = ?1",
                    [recipe_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                ).map_err(|error| StorageError::sqlite("查询配方", error))?;
                assert_eq!(metadata, ("Renamed".into(), 3));
                Ok(())
            })
            .await?;
        storage.writer_handle.shutdown().await
    }
}
