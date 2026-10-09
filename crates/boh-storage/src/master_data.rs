//! Decode frozen snapshots with SQLite JSON, then project only their stored facts.

use boh_domain::AggregateId;
use boh_domain::master_data::{
    BaseUnit, ItemCategory, ItemSnapshot, ItemUnit, RecipeLine, RecipeSnapshot, RecipeVersion,
    SupplierSnapshot, WasteReasonSnapshot,
};
use rusqlite::{Transaction, params};

use crate::{StorageError, ledger::Event};

fn invalid() -> StorageError {
    StorageError::InvalidEvent("invalid master data payload".into())
}

// Reject missing, unknown, duplicate and mistyped fields, including null optionals.
fn object(
    tx: &Transaction<'_>,
    json: &str,
    required: &[(&str, &str)],
    optional: &[(&str, &str)],
) -> Result<(), StorageError> {
    let kind: String = tx
        .query_row("SELECT json_type(?1)", [json], |r| r.get(0))
        .map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
    if kind != "object" {
        return Err(invalid());
    }
    let mut statement = tx
        .prepare("SELECT key, type FROM json_each(?1)")
        .map_err(|error| StorageError::sqlite("准备查询事件 JSON", error))?;
    let fields: Vec<(String, String)> = statement
        .query_map([json], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|error| StorageError::sqlite("查询事件 JSON", error))?
        .collect::<Result<_, _>>()
        .map_err(|error| StorageError::sqlite("读取事件 JSON", error))?;
    let mut seen = Vec::new();
    for (key, kind) in &fields {
        let expected = required
            .iter()
            .chain(optional)
            .find(|(name, _)| *name == key);
        let Some((_, expected)) = expected else {
            return Err(invalid());
        };
        if seen.contains(&key.as_str())
            || !(kind == expected
                || (*expected == "boolean" && matches!(kind.as_str(), "true" | "false")))
        {
            return Err(invalid());
        }
        seen.push(key.as_str());
    }
    if required.iter().any(|(name, _)| !seen.contains(name)) {
        return Err(invalid());
    }
    Ok(())
}

fn array(tx: &Transaction<'_>, json: &str, path: &str) -> Result<Vec<String>, StorageError> {
    let mut statement = tx
        .prepare("SELECT value, type FROM json_each(?1, ?2) ORDER BY CAST(key AS INTEGER)")
        .map_err(|error| StorageError::sqlite("准备查询事件 JSON", error))?;
    let rows = statement
        .query_map(params![json, path], |row| {
            let kind: String = row.get(1)?;
            if kind == "object" {
                Ok(Some(row.get::<_, String>(0)?))
            } else {
                Ok(None)
            }
        })
        .map_err(|error| StorageError::sqlite("查询主数据数组", error))?;
    let mut objects = Vec::new();
    for row in rows {
        let object = row
            .map_err(|error| StorageError::sqlite("解码主数据数组", error))?
            .ok_or_else(invalid)?;
        objects.push(object);
    }
    Ok(objects)
}

pub(crate) fn apply(tx: &Transaction<'_>, event: &Event) -> Result<(), StorageError> {
    object(
        tx,
        &event.payload,
        &[
            ("entity", "text"),
            ("source", "text"),
            ("snapshot", "object"),
        ],
        &[],
    )?;
    let (entity, source, snapshot): (String, String, String) = tx.query_row(
        "SELECT json_extract(?1, '$.entity'), json_extract(?1, '$.source'), json_extract(?1, '$.snapshot')",
        [&event.payload], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
    if entity != event.aggregate_type
        || !matches!(source.as_str(), "LOCAL" | "HQ_PACKAGE")
        || event.aggregate_version <= 0
    {
        return Err(invalid());
    }
    match entity.as_str() {
        "ITEM" => item(tx, event, &snapshot),
        "RECIPE" => recipe(tx, event, &snapshot),
        "SUPPLIER" => supplier(tx, event, &snapshot),
        "WASTE_REASON" => waste_reason(tx, event, &snapshot),
        _ => Err(invalid()),
    }
}

fn item(tx: &Transaction<'_>, event: &Event, json: &str) -> Result<(), StorageError> {
    object(
        tx,
        json,
        &[
            ("code", "text"),
            ("name", "text"),
            ("base_unit", "text"),
            ("category", "text"),
            ("units", "array"),
            ("active", "boolean"),
        ],
        &[("default_shelf_life_ms", "integer")],
    )?;
    let (code, name, base, category, shelf_life, active): (String, String, String, String, Option<i64>, bool) = tx.query_row(
        "SELECT json_extract(?1, '$.code'), json_extract(?1, '$.name'), json_extract(?1, '$.base_unit'),
         json_extract(?1, '$.category'), json_extract(?1, '$.default_shelf_life_ms'), json_extract(?1, '$.active')",
        [json], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ).map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
    let mut units = Vec::new();
    for unit in array(tx, json, "$.units")? {
        object(
            tx,
            &unit,
            &[("unit_code", "text"), ("base_qty_per_unit", "integer")],
            &[],
        )?;
        units.push(
            tx.query_row(
                "SELECT json_extract(?1, '$.unit_code'), json_extract(?1, '$.base_qty_per_unit')",
                [&unit],
                |r| {
                    Ok(ItemUnit {
                        unit_code: r.get(0)?,
                        base_qty_per_unit: r.get(1)?,
                    })
                },
            )
            .map_err(|error| StorageError::sqlite("查询事件 JSON", error))?,
        );
    }
    let snapshot = ItemSnapshot {
        code,
        name,
        base_unit: BaseUnit::parse(&base)?,
        category: ItemCategory::parse(&category)?,
        default_shelf_life_ms: shelf_life,
        units,
        active,
    };
    snapshot.validate()?;
    let values = params![
        event.aggregate_id.to_string(),
        snapshot.code,
        snapshot.name,
        base,
        category,
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
    for unit in snapshot.units {
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

fn recipe(tx: &Transaction<'_>, event: &Event, json: &str) -> Result<(), StorageError> {
    object(
        tx,
        json,
        &[
            ("code", "text"),
            ("name", "text"),
            ("output_item_id", "text"),
            ("versions", "array"),
            ("active", "boolean"),
        ],
        &[],
    )?;
    let (code, name, output, active): (String, String, String, bool) = tx.query_row(
        "SELECT json_extract(?1, '$.code'), json_extract(?1, '$.name'), json_extract(?1, '$.output_item_id'),
            json_extract(?1, '$.active')", [json], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
    let mut versions = Vec::new();
    for version in array(tx, json, "$.versions")? {
        object(
            tx,
            &version,
            &[
                ("version", "integer"),
                ("output_qty_per_batch", "integer"),
                ("lines", "array"),
            ],
            &[],
        )?;
        let (number, output_qty_per_batch): (i64, i64) = tx
            .query_row(
                "SELECT json_extract(?1, '$.version'), json_extract(?1, '$.output_qty_per_batch')",
                [&version],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
        let mut lines = Vec::new();
        for line in array(tx, &version, "$.lines")? {
            object(
                tx,
                &line,
                &[("item_id", "text"), ("qty_per_batch", "integer")],
                &[],
            )?;
            let (item, qty_per_batch): (String, i64) = tx
                .query_row(
                    "SELECT json_extract(?1, '$.item_id'), json_extract(?1, '$.qty_per_batch')",
                    [&line],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
            lines.push(RecipeLine {
                item_id: AggregateId::parse(&item)?,
                qty_per_batch,
            });
        }
        versions.push(RecipeVersion {
            version: number,
            output_qty_per_batch,
            lines,
        });
    }
    let snapshot = RecipeSnapshot {
        code,
        name,
        output_item_id: AggregateId::parse(&output)?,
        versions,
        active,
    };
    snapshot.validate()?;
    let values = params![
        event.aggregate_id.to_string(),
        snapshot.code,
        snapshot.name,
        output,
        snapshot.active,
        event.aggregate_version
    ];
    if tx.execute("UPDATE recipes SET code = ?2, name = ?3, output_item_id = ?4, active = ?5, revision = ?6 WHERE id = ?1", values).map_err(|error| StorageError::sqlite("更新配方", error))? == 0 {
        tx.execute("INSERT INTO recipes (id, code, name, output_item_id, active, revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", values).map_err(|error| StorageError::sqlite("插入配方", error))?;
    }
    for version in snapshot.versions {
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
        for (index, line) in version.lines.into_iter().enumerate() {
            let line_no = i64::try_from(index).map_err(|_| invalid())?;
            tx.execute("INSERT INTO recipe_lines (recipe_id, version, line_no, item_id, qty_per_batch) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![event.aggregate_id.to_string(), version.version, line_no, line.item_id.to_string(), line.qty_per_batch]).map_err(|error| StorageError::sqlite("插入配方明细", error))?;
        }
    }
    Ok(())
}

fn supplier(tx: &Transaction<'_>, event: &Event, json: &str) -> Result<(), StorageError> {
    object(
        tx,
        json,
        &[("code", "text"), ("name", "text"), ("active", "boolean")],
        &[("contact_phone", "text")],
    )?;
    let snapshot = tx
        .query_row(
            "SELECT json_extract(?1, '$.code'), json_extract(?1, '$.name'),
        json_extract(?1, '$.contact_phone'), json_extract(?1, '$.active')",
            [json],
            |r| {
                Ok(SupplierSnapshot {
                    code: r.get(0)?,
                    name: r.get(1)?,
                    contact_phone: r.get(2)?,
                    active: r.get(3)?,
                })
            },
        )
        .map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
    snapshot.validate()?;
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

fn waste_reason(tx: &Transaction<'_>, event: &Event, json: &str) -> Result<(), StorageError> {
    object(
        tx,
        json,
        &[("code", "text"), ("name", "text"), ("active", "boolean")],
        &[],
    )?;
    let snapshot = tx.query_row("SELECT json_extract(?1, '$.code'), json_extract(?1, '$.name'), json_extract(?1, '$.active')", [json],
        |r| Ok(WasteReasonSnapshot { code: r.get(0)?, name: r.get(1)?, active: r.get(2)? })).map_err(|error| StorageError::sqlite("查询事件 JSON", error))?;
    snapshot.validate()?;
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

    use super::{array, recipe};
    use crate::{StorageError, ledger::Event, open};

    #[tokio::test]
    async fn array_rejects_non_object_elements_as_invalid_events() -> Result<(), StorageError> {
        let dir = tempfile::tempdir().map_err(|error| StorageError::io("创建测试目录", error))?;
        let storage = open(&dir.path().join("boh.db"), NonZeroUsize::MIN)?;
        storage
            .writer
            .call(|tx| -> Result<(), StorageError> {
                for value in ["null", "true", "false", "1", "1.5", "\"text\"", "[]"] {
                    let json = format!(r#"{{"values":[{{}},{value}]}}"#);
                    assert!(matches!(
                        array(tx, &json, "$.values"),
                        Err(StorageError::InvalidEvent(_))
                    ));
                }
                assert_eq!(
                    array(tx, r#"{"values":[{"n":2},{"n":1}]}"#, "$.values")?,
                    [r#"{"n":2}"#, r#"{"n":1}"#]
                );
                assert!(array(tx, r#"{"values":[]}"#, "$.values")?.is_empty());
                Ok(())
            })
            .await?;
        storage.writer_handle.shutdown().await
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
                        r#"{{"code":"RECIPE","name":"{name}","output_item_id":"{item_id}","versions":[{versions}],"active":true}}"#
                    )
                };
                recipe(tx, &event, &snapshot("Original", &v1))?;

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
                recipe(tx, &event, &snapshot("Renamed", &v1))?;
                event.aggregate_version = 3;
                recipe(tx, &event, &snapshot("Renamed", &format!("{v1},{v2}")))?;

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
