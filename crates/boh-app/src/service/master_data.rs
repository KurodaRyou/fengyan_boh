//! Master data command transactions and queries.

use axum::http::StatusCode;
use boh_domain::equipment::MasterDataSource;
use boh_domain::master_data::*;
use boh_domain::time::{BusinessDayCutoff, StoreTimeZone, business_date};
use boh_domain::{AggregateId, CommandId, EventId, StoreId, UnixMillis};
use boh_storage::clock::Clock;
use boh_storage::ledger::{self, Command, Event, ExecuteError, Ledger};
use boh_storage::rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use boh_storage::{StorageError, Writer};
use serde::Serialize;
use serde_json::{Value, json};

use super::missing_reference;
use crate::{
    AppState,
    actor::Actor,
    http::{ApiError, ok},
};

fn normalized(
    command: &impl Serialize,
    path: Option<(&str, AggregateId)>,
) -> Result<String, ApiError> {
    let mut value = serde_json::to_value(command)
        .map_err(|error| ApiError::from(StorageError::external("规范化主数据命令", error)))?;
    let object = value.as_object_mut().ok_or_else(ApiError::validation)?;
    object.remove("command_id");
    if let Some((key, id)) = path {
        object.insert(key.into(), json!(id));
    }
    serde_json::to_string(&value)
        .map_err(|error| ApiError::from(StorageError::external("规范化主数据命令", error)))
}

fn response(key: &str, row: &impl Serialize) -> Result<String, ApiError> {
    serde_json::to_string(&ok(json!({key: row})).0)
        .map_err(|error| ApiError::from(StorageError::external("序列化主数据响应", error)))
}

fn revision(current: i64, submitted: i64) -> Result<(), ApiError> {
    if current != submitted {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "REVISION_CONFLICT",
            "master data revision changed",
        )
        .with_details(json!({"current_revision": current})));
    }
    Ok(())
}

fn next(current: i64) -> Result<i64, ApiError> {
    current
        .checked_add(1)
        .ok_or_else(|| ApiError::internal_message("master data version overflow"))
}

// Table and field arguments are fixed by the caller, never supplied by a client.
fn check_code(tx: &Transaction<'_>, table: &str, id_key: &str, code: &str) -> Result<(), ApiError> {
    let existing: Option<String> = tx
        .query_row(
            &format!("SELECT id FROM {table} WHERE code = ?1"),
            [code],
            |r| r.get(0),
        )
        .optional()
        .map_err(|error| StorageError::sqlite("查询主数据编码是否已占用", error))?;
    if let Some(id) = existing {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "CODE_ALREADY_EXISTS",
            "master data code already exists",
        )
        .with_details(json!({"code": code, id_key: id})));
    }
    Ok(())
}

fn item_reference(tx: &Transaction<'_>, id: AggregateId) -> Result<(), ApiError> {
    let found: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM items WHERE id = ?1)",
            [id.to_string()],
            |r| r.get(0),
        )
        .map_err(|error| StorageError::sqlite("查询物料", error))?;
    if !found {
        return Err(missing_reference("ITEM", id));
    }
    Ok(())
}

fn line_references(tx: &Transaction<'_>, lines: &[RecipeLine]) -> Result<(), ApiError> {
    for line in lines {
        item_reference(tx, line.item_id)?;
    }
    Ok(())
}

struct Write<'a> {
    tx: &'a Transaction<'a>,
    ledger: &'a Ledger<'a>,
    state: &'a AppState,
    actor: Actor,
    command_id: CommandId,
    recorded_at: UnixMillis,
}

impl Write<'_> {
    fn id(&self) -> Result<AggregateId, ApiError> {
        Ok(
            AggregateId::from_parts(self.recorded_at, ledger::entropy(self.tx)?)
                .map_err(StorageError::from)?,
        )
    }

    fn append(
        &self,
        id: AggregateId,
        revision: i64,
        payload: MasterDataChanged,
    ) -> Result<(), ApiError> {
        let entity = match &payload {
            MasterDataChanged::Item { .. } => "ITEM",
            MasterDataChanged::Recipe { .. } => "RECIPE",
            MasterDataChanged::Supplier { .. } => "SUPPLIER",
            MasterDataChanged::WasteReason { .. } => "WASTE_REASON",
        };
        let date = business_date(
            self.recorded_at,
            &self.state.timezone,
            self.state.business_day_cutoff,
        )
        .map_err(|error| ApiError::from(StorageError::external("计算主数据事件营业日", error)))?;
        self.ledger.append(&Event {
            id: EventId::from_parts(self.recorded_at, ledger::entropy(self.tx)?)
                .map_err(StorageError::from)?,
            event_type: "MASTER_DATA_CHANGED".into(),
            schema_version: 1,
            aggregate_type: entity.into(),
            aggregate_id: id,
            aggregate_version: revision,
            command_id: self.command_id,
            actor_id: self.actor.employee_id,
            device_id: self.actor.device_id,
            business_date: date.to_string(),
            occurred_at: self.recorded_at,
            recorded_at: self.recorded_at,
            payload: serde_json::to_string(&payload).map_err(|error| {
                ApiError::from(StorageError::external("序列化主数据事件", error))
            })?,
        })?;
        Ok(())
    }
}

async fn execute<F>(
    state: AppState,
    actor: Actor,
    id: CommandId,
    kind: &'static str,
    request: String,
    run: F,
) -> Result<Value, ApiError>
where
    F: FnOnce(&Write<'_>) -> Result<String, ApiError> + Send + 'static,
{
    let writer = state.writer.clone();
    let saved = writer
        .call(move |tx| {
            let recorded_at = state.clock.now();
            ledger::execute(
                tx,
                Command {
                    id,
                    command_type: kind,
                    request: &request,
                    recorded_at,
                },
                |ledger| {
                    run(&Write {
                        tx,
                        ledger,
                        state: &state,
                        actor,
                        command_id: id,
                        recorded_at,
                    })
                },
            )
        })
        .await?;
    serde_json::from_str(&saved)
        .map_err(|error| ApiError::from(StorageError::external("解码已保存的命令响应", error)))
}

#[tracing::instrument(name = "command", skip_all, fields(command_type = "item.create", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn create_item(
    state: AppState,
    actor: Actor,
    command: CreateItem,
) -> Result<Value, ApiError> {
    let request = normalized(&command, None)?;
    execute(
        state,
        actor,
        command.command_id,
        "item.create",
        request,
        move |write| {
            check_code(write.tx, "items", "item_id", &command.code)?;
            let row = Item {
                item_id: write.id()?,
                snapshot: command.snapshot(),
                revision: 1,
            };
            write.append(
                row.item_id,
                row.revision,
                MasterDataChanged::Item {
                    source: MasterDataSource::Local,
                    snapshot: row.snapshot.clone(),
                },
            )?;
            response("item", &row)
        },
    )
    .await
}

#[tracing::instrument(name = "command", skip_all, fields(command_type = "item.update", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn update_item(
    state: AppState,
    actor: Actor,
    id: AggregateId,
    command: UpdateItem,
) -> Result<Value, ApiError> {
    let request = normalized(&command, Some(("item_id", id)))?;
    execute(
        state,
        actor,
        command.command_id,
        "item.update",
        request,
        move |write| {
            let mut row = read_item(write.tx, id)?.ok_or_else(|| missing_reference("ITEM", id))?;
            revision(row.revision, command.base_revision)?;
            let snapshot = command.snapshot(&row.snapshot);
            snapshot.validate().map_err(|_| ApiError::validation())?;
            if snapshot != row.snapshot {
                row.revision = next(row.revision)?;
                row.snapshot = snapshot;
                write.append(
                    id,
                    row.revision,
                    MasterDataChanged::Item {
                        source: MasterDataSource::Local,
                        snapshot: row.snapshot.clone(),
                    },
                )?;
            }
            response("item", &row)
        },
    )
    .await
}

#[tracing::instrument(name = "command", skip_all, fields(command_type = "recipe.create", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn create_recipe(
    state: AppState,
    actor: Actor,
    command: CreateRecipe,
) -> Result<Value, ApiError> {
    let request = normalized(&command, None)?;
    execute(
        state,
        actor,
        command.command_id,
        "recipe.create",
        request,
        move |write| {
            check_code(write.tx, "recipes", "recipe_id", &command.code)?;
            item_reference(write.tx, command.output_item_id)?;
            line_references(write.tx, &command.lines)?;
            let row = Recipe {
                recipe_id: write.id()?,
                snapshot: command.snapshot(),
                revision: 1,
            };
            write.append(
                row.recipe_id,
                row.revision,
                MasterDataChanged::Recipe {
                    source: MasterDataSource::Local,
                    snapshot: row.snapshot.clone(),
                },
            )?;
            response("recipe", &row)
        },
    )
    .await
}

#[tracing::instrument(name = "command", skip_all, fields(command_type = "recipe.update", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn update_recipe(
    state: AppState,
    actor: Actor,
    id: AggregateId,
    command: UpdateRecipe,
) -> Result<Value, ApiError> {
    let request = normalized(&command, Some(("recipe_id", id)))?;
    execute(
        state,
        actor,
        command.command_id,
        "recipe.update",
        request,
        move |write| {
            let mut row =
                read_recipe(write.tx, id)?.ok_or_else(|| missing_reference("RECIPE", id))?;
            revision(row.revision, command.base_revision)?;
            if row.snapshot.name != command.name || row.snapshot.active != command.active {
                row.revision = next(row.revision)?;
                row.snapshot.name = command.name;
                row.snapshot.active = command.active;
                write.append(
                    id,
                    row.revision,
                    MasterDataChanged::Recipe {
                        source: MasterDataSource::Local,
                        snapshot: row.snapshot.clone(),
                    },
                )?;
            }
            response("recipe", &row)
        },
    )
    .await
}

#[tracing::instrument(name = "command", skip_all, fields(command_type = "recipe.add_version", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn add_recipe_version(
    state: AppState,
    actor: Actor,
    id: AggregateId,
    command: AddRecipeVersion,
) -> Result<Value, ApiError> {
    let request = normalized(&command, Some(("recipe_id", id)))?;
    execute(
        state,
        actor,
        command.command_id,
        "recipe.add_version",
        request,
        move |write| {
            let mut row =
                read_recipe(write.tx, id)?.ok_or_else(|| missing_reference("RECIPE", id))?;
            revision(row.revision, command.base_revision)?;
            line_references(write.tx, &command.lines)?;
            let latest = row
                .snapshot
                .versions
                .last()
                .ok_or_else(|| ApiError::internal_message("recipe has no versions"))?;
            if latest.output_qty_per_batch != command.output_qty_per_batch
                || latest.lines != command.lines
            {
                let version = next(latest.version)?;
                row.revision = next(row.revision)?;
                row.snapshot.versions.push(RecipeVersion {
                    version,
                    output_qty_per_batch: command.output_qty_per_batch,
                    lines: command.lines,
                });
                write.append(
                    id,
                    row.revision,
                    MasterDataChanged::Recipe {
                        source: MasterDataSource::Local,
                        snapshot: row.snapshot.clone(),
                    },
                )?;
            }
            response("recipe", &row)
        },
    )
    .await
}

#[tracing::instrument(name = "command", skip_all, fields(command_type = "supplier.create", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn create_supplier(
    state: AppState,
    actor: Actor,
    command: CreateSupplier,
) -> Result<Value, ApiError> {
    let request = normalized(&command, None)?;
    execute(
        state,
        actor,
        command.command_id,
        "supplier.create",
        request,
        move |write| {
            check_code(write.tx, "suppliers", "supplier_id", &command.code)?;
            let row = Supplier {
                supplier_id: write.id()?,
                snapshot: command.snapshot(),
                revision: 1,
            };
            write.append(
                row.supplier_id,
                row.revision,
                MasterDataChanged::Supplier {
                    source: MasterDataSource::Local,
                    snapshot: row.snapshot.clone(),
                },
            )?;
            response("supplier", &row)
        },
    )
    .await
}

#[tracing::instrument(name = "command", skip_all, fields(command_type = "supplier.update", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn update_supplier(
    state: AppState,
    actor: Actor,
    id: AggregateId,
    command: UpdateSupplier,
) -> Result<Value, ApiError> {
    let request = normalized(&command, Some(("supplier_id", id)))?;
    execute(state, actor, command.command_id, "supplier.update", request, move |write| {
        let mut row = write.tx.query_row("SELECT id, code, name, contact_phone, active, revision FROM suppliers WHERE id = ?1", [id.to_string()], read_supplier)
            .optional().map_err(|error| StorageError::sqlite("查询供应商", error))?.ok_or_else(|| missing_reference("SUPPLIER", id))?;
        revision(row.revision, command.base_revision)?;
        let snapshot = command.snapshot(&row.snapshot);
        if snapshot != row.snapshot {
            row.revision = next(row.revision)?;
            row.snapshot = snapshot;
            write.append(id, row.revision, MasterDataChanged::Supplier { source: MasterDataSource::Local, snapshot: row.snapshot.clone() })?;
        }
        response("supplier", &row)
    }).await
}

#[tracing::instrument(name = "command", skip_all, fields(command_type = "waste_reason.create", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn create_waste_reason(
    state: AppState,
    actor: Actor,
    command: CreateWasteReason,
) -> Result<Value, ApiError> {
    let request = normalized(&command, None)?;
    execute(
        state,
        actor,
        command.command_id,
        "waste_reason.create",
        request,
        move |write| {
            check_code(write.tx, "waste_reasons", "waste_reason_id", &command.code)?;
            let row = WasteReason {
                waste_reason_id: write.id()?,
                snapshot: command.snapshot(),
                revision: 1,
            };
            write.append(
                row.waste_reason_id,
                row.revision,
                MasterDataChanged::WasteReason {
                    source: MasterDataSource::Local,
                    snapshot: row.snapshot.clone(),
                },
            )?;
            response("waste_reason", &row)
        },
    )
    .await
}

#[tracing::instrument(name = "command", skip_all, fields(command_type = "waste_reason.update", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn update_waste_reason(
    state: AppState,
    actor: Actor,
    id: AggregateId,
    command: UpdateWasteReason,
) -> Result<Value, ApiError> {
    let request = normalized(&command, Some(("waste_reason_id", id)))?;
    execute(
        state,
        actor,
        command.command_id,
        "waste_reason.update",
        request,
        move |write| {
            let mut row = write
                .tx
                .query_row(
                    "SELECT id, code, name, active, revision FROM waste_reasons WHERE id = ?1",
                    [id.to_string()],
                    read_waste_reason,
                )
                .optional()
                .map_err(|error| StorageError::sqlite("查询报损原因", error))?
                .ok_or_else(|| missing_reference("WASTE_REASON", id))?;
            revision(row.revision, command.base_revision)?;
            let snapshot = command.snapshot(&row.snapshot);
            if snapshot != row.snapshot {
                row.revision = next(row.revision)?;
                row.snapshot = snapshot;
                write.append(
                    id,
                    row.revision,
                    MasterDataChanged::WasteReason {
                        source: MasterDataSource::Local,
                        snapshot: row.snapshot.clone(),
                    },
                )?;
            }
            response("waste_reason", &row)
        },
    )
    .await
}

fn conversion(index: usize, error: boh_domain::DomainError) -> boh_storage::rusqlite::Error {
    boh_storage::rusqlite::Error::FromSqlConversionFailure(
        index,
        boh_storage::rusqlite::types::Type::Text,
        Box::new(error),
    )
}

fn row_id(row: &Row<'_>, index: usize) -> boh_storage::rusqlite::Result<AggregateId> {
    AggregateId::parse(&row.get::<_, String>(index)?).map_err(|e| conversion(index, e))
}

fn read_item(conn: &Connection, id: AggregateId) -> Result<Option<Item>, ApiError> {
    let mut row = conn.query_row("SELECT id, code, name, base_unit, category, default_shelf_life_ms, active, revision FROM items WHERE id = ?1",
        [id.to_string()], |r| Ok(Item {
            item_id: row_id(r, 0)?, revision: r.get(7)?,
            snapshot: ItemSnapshot { code: r.get(1)?, name: r.get(2)?,
                base_unit: BaseUnit::parse(&r.get::<_, String>(3)?).map_err(|e| conversion(3, e))?,
                category: ItemCategory::parse(&r.get::<_, String>(4)?).map_err(|e| conversion(4, e))?,
                default_shelf_life_ms: r.get(5)?, active: r.get(6)?, units: Vec::new() },
        })).optional().map_err(|error| StorageError::sqlite("查询物料", error))?;
    if let Some(row) = &mut row {
        let mut statement = conn.prepare("SELECT unit_code, base_qty_per_unit FROM item_units WHERE item_id = ?1 ORDER BY unit_code COLLATE BINARY")
            .map_err(|error| StorageError::sqlite("准备查询物料单位", error))?;
        row.snapshot.units = statement
            .query_map([id.to_string()], |r| {
                Ok(ItemUnit {
                    unit_code: r.get(0)?,
                    base_qty_per_unit: r.get(1)?,
                })
            })
            .map_err(|error| StorageError::sqlite("查询物料单位", error))?
            .collect::<Result<_, _>>()
            .map_err(|error| StorageError::sqlite("读取物料单位", error))?;
    }
    Ok(row)
}

fn read_recipe(conn: &Connection, id: AggregateId) -> Result<Option<Recipe>, ApiError> {
    let mut row = conn
        .query_row(
            "SELECT id, code, name, output_item_id, active, revision FROM recipes WHERE id = ?1",
            [id.to_string()],
            |r| {
                Ok(Recipe {
                    recipe_id: row_id(r, 0)?,
                    revision: r.get(5)?,
                    snapshot: RecipeSnapshot {
                        code: r.get(1)?,
                        name: r.get(2)?,
                        output_item_id: row_id(r, 3)?,
                        active: r.get(4)?,
                        versions: Vec::new(),
                    },
                })
            },
        )
        .optional()
        .map_err(|error| StorageError::sqlite("查询配方", error))?;
    if let Some(row) = &mut row {
        let mut statement = conn.prepare("SELECT version, output_qty_per_batch FROM recipe_versions WHERE recipe_id = ?1 ORDER BY version")
            .map_err(|error| StorageError::sqlite("准备查询配方版本", error))?;
        let mut versions: Vec<RecipeVersion> = statement
            .query_map([id.to_string()], |r| {
                Ok(RecipeVersion {
                    version: r.get(0)?,
                    output_qty_per_batch: r.get(1)?,
                    lines: Vec::new(),
                })
            })
            .map_err(|error| StorageError::sqlite("查询配方版本", error))?
            .collect::<Result<_, _>>()
            .map_err(|error| StorageError::sqlite("读取配方版本", error))?;
        let mut lines = conn.prepare("SELECT item_id, qty_per_batch FROM recipe_lines WHERE recipe_id = ?1 AND version = ?2 ORDER BY line_no")
            .map_err(|error| StorageError::sqlite("准备查询配方明细", error))?;
        for version in &mut versions {
            version.lines = lines
                .query_map(params![id.to_string(), version.version], |r| {
                    Ok(RecipeLine {
                        item_id: row_id(r, 0)?,
                        qty_per_batch: r.get(1)?,
                    })
                })
                .map_err(|error| StorageError::sqlite("查询配方明细", error))?
                .collect::<Result<_, _>>()
                .map_err(|error| StorageError::sqlite("读取配方明细", error))?;
        }
        row.snapshot.versions = versions;
    }
    Ok(row)
}

fn read_supplier(row: &Row<'_>) -> boh_storage::rusqlite::Result<Supplier> {
    Ok(Supplier {
        supplier_id: row_id(row, 0)?,
        revision: row.get(5)?,
        snapshot: SupplierSnapshot {
            code: row.get(1)?,
            name: row.get(2)?,
            contact_phone: row.get(3)?,
            active: row.get(4)?,
        },
    })
}

fn read_waste_reason(row: &Row<'_>) -> boh_storage::rusqlite::Result<WasteReason> {
    Ok(WasteReason {
        waste_reason_id: row_id(row, 0)?,
        revision: row.get(4)?,
        snapshot: WasteReasonSnapshot {
            code: row.get(1)?,
            name: row.get(2)?,
            active: row.get(3)?,
        },
    })
}

fn ids(conn: &Connection, table: &str) -> Result<Vec<AggregateId>, ApiError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT id FROM {table} ORDER BY code COLLATE BINARY"
        ))
        .map_err(|error| StorageError::sqlite("准备查询主数据 ID 列表", error))?;
    Ok(statement
        .query_map([], |r| row_id(r, 0))
        .map_err(|error| StorageError::sqlite("查询主数据 ID 列表", error))?
        .collect::<Result<_, _>>()
        .map_err(|error| StorageError::sqlite("读取主数据 ID 列表", error))?)
}

pub async fn list_items(state: AppState) -> Result<Vec<Item>, ApiError> {
    state
        .readers
        .call(|conn| {
            ids(conn, "items")?
                .into_iter()
                .map(|id| {
                    read_item(conn, id)?.ok_or_else(|| {
                        ApiError::internal_message("item disappeared from read snapshot")
                    })
                })
                .collect()
        })
        .await
}

pub async fn list_recipes(state: AppState) -> Result<Vec<Recipe>, ApiError> {
    state
        .readers
        .call(|conn| {
            ids(conn, "recipes")?
                .into_iter()
                .map(|id| {
                    read_recipe(conn, id)?.ok_or_else(|| {
                        ApiError::internal_message("recipe disappeared from read snapshot")
                    })
                })
                .collect()
        })
        .await
}

pub async fn list_suppliers(state: AppState) -> Result<Vec<Supplier>, ApiError> {
    state.readers.call(|conn| -> Result<_, ApiError> {
        let mut statement = conn.prepare("SELECT id, code, name, contact_phone, active, revision FROM suppliers ORDER BY code COLLATE BINARY").map_err(|error| StorageError::sqlite("准备查询供应商", error))?;
        Ok(statement.query_map([], read_supplier).map_err(|error| StorageError::sqlite("查询供应商", error))?.collect::<Result<_, _>>().map_err(|error| StorageError::sqlite("读取供应商", error))?)
    }).await
}

pub async fn list_waste_reasons(state: AppState) -> Result<Vec<WasteReason>, ApiError> {
    state.readers.call(|conn| -> Result<_, ApiError> {
        let mut statement = conn.prepare("SELECT id, code, name, active, revision FROM waste_reasons ORDER BY code COLLATE BINARY").map_err(|error| StorageError::sqlite("准备查询报损原因", error))?;
        Ok(statement.query_map([], read_waste_reason).map_err(|error| StorageError::sqlite("查询报损原因", error))?.collect::<Result<_, _>>().map_err(|error| StorageError::sqlite("读取报损原因", error))?)
    }).await
}

/// CLI initialization; test nodes deliberately initialize only store_meta.
pub async fn initialize_store(
    writer: Writer,
    store_id: StoreId,
    clock: Clock,
    timezone: StoreTimeZone,
    cutoff: BusinessDayCutoff,
) -> Result<(), ExecuteError> {
    writer
        .call(move |tx| {
            let recorded_at = clock.now();
            let command_id = CommandId::from_parts(recorded_at, ledger::entropy(tx)?)
                .map_err(StorageError::from)?;
            let request = json!({"store_id": store_id}).to_string();
            ledger::execute(
                tx,
                Command {
                    id: command_id,
                    command_type: "store.init",
                    request: &request,
                    recorded_at,
                },
                |ledger| -> Result<String, ExecuteError> {
                    boh_storage::store::initialize(tx, store_id, recorded_at)?;
                    let date = business_date(recorded_at, &timezone, cutoff)
                        .map_err(|error| StorageError::external("计算门店初始化营业日", error))?;
                    let actor_id = AggregateId::parse("00000000-0000-7000-8000-000000000000")
                        .map_err(StorageError::from)?;
                    let device_id = AggregateId::parse("00000000-0000-7000-8000-000000000001")
                        .map_err(StorageError::from)?;
                    for (code, name) in [
                        ("EXPIRED", "过期"),
                        ("DAMAGED", "损坏"),
                        ("PRODUCTION_DEFECT", "生产不良"),
                        ("TASTING", "试吃"),
                        ("OTHER", "其他"),
                    ] {
                        let payload = MasterDataChanged::WasteReason {
                            source: MasterDataSource::Local,
                            snapshot: WasteReasonSnapshot {
                                code: code.into(),
                                name: name.into(),
                                active: true,
                            },
                        };
                        ledger.append(&Event {
                            id: EventId::from_parts(recorded_at, ledger::entropy(tx)?)
                                .map_err(StorageError::from)?,
                            event_type: "MASTER_DATA_CHANGED".into(),
                            schema_version: 1,
                            aggregate_type: "WASTE_REASON".into(),
                            aggregate_id: AggregateId::from_parts(
                                recorded_at,
                                ledger::entropy(tx)?,
                            )
                            .map_err(StorageError::from)?,
                            aggregate_version: 1,
                            command_id,
                            actor_id,
                            device_id,
                            business_date: date.to_string(),
                            occurred_at: recorded_at,
                            recorded_at,
                            payload: serde_json::to_string(&payload).map_err(|error| {
                                StorageError::external("序列化预置报损原因事件", error)
                            })?,
                        })?;
                    }
                    Ok("{}".into())
                },
            )?;
            Ok(())
        })
        .await
}
