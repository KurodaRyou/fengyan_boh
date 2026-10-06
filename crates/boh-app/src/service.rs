//! Equipment command orchestration. Business checks execute after ledger idempotency.

use axum::http::StatusCode;
use boh_domain::equipment::{
    CreateEquipment, Equipment, EquipmentChanged, EquipmentEntity, EquipmentSnapshot,
    EquipmentType, MasterDataSource, UpdateEquipment,
};
use boh_domain::time::business_date;
use boh_domain::{AggregateId, CommandId, EventId, UnixMillis};
use boh_storage::StorageError;
use boh_storage::ledger::{self, Command, Event, ExecuteError, Ledger};
use boh_storage::rusqlite::{OptionalExtension, Row};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    AppState,
    actor::Actor,
    http::{ApiError, ok},
};

impl From<ExecuteError> for ApiError {
    fn from(error: ExecuteError) -> Self {
        match error {
            ExecuteError::Storage(error) => error.into(),
            ExecuteError::IdempotencyConflict(fields) => Self::new(
                StatusCode::CONFLICT,
                "IDEMPOTENCY_CONFLICT",
                "command ID already used with different content",
            )
            .with_details(json!({ "fields": fields })),
        }
    }
}

pub async fn create(
    state: AppState,
    actor: Actor,
    command: CreateEquipment,
) -> Result<Value, ApiError> {
    let request = normalized(&command, None)?;
    let writer = state.writer.clone();
    let response = writer
        .call(move |tx| {
            let recorded_at = state.clock.now();
            ledger::execute(
                tx,
                Command {
                    id: command.command_id,
                    command_type: "equipment.create",
                    request: &request,
                    recorded_at,
                },
                |ledger| {
                    let existing: Option<String> = tx
                        .query_row(
                            "SELECT id FROM equipment WHERE code = ?1",
                            [&command.code],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(StorageError::from)?;
                    if let Some(id) = existing {
                        return Err(ApiError::new(
                            StatusCode::CONFLICT,
                            "CODE_ALREADY_EXISTS",
                            "equipment code already exists",
                        )
                        .with_details(json!({ "code": command.code, "equipment_id": id })));
                    }
                    let id = AggregateId::from_parts(recorded_at, ledger::entropy(tx)?)
                        .map_err(StorageError::from)?;
                    let row = equipment(id, command.snapshot(), 1);
                    append(
                        ledger,
                        tx,
                        &state,
                        actor,
                        command.command_id,
                        recorded_at,
                        &row,
                    )?;
                    response(&row)
                },
            )
        })
        .await?;
    parse_response(&response)
}

pub async fn update(
    state: AppState,
    actor: Actor,
    id: AggregateId,
    command: UpdateEquipment,
) -> Result<Value, ApiError> {
    let request = normalized(&command, Some(id))?;
    let writer = state.writer.clone();
    let response = writer
        .call(move |tx| {
            let recorded_at = state.clock.now();
            ledger::execute(
                tx,
                Command {
                    id: command.command_id,
                    command_type: "equipment.update",
                    request: &request,
                    recorded_at,
                },
                |ledger| {
                    let current = tx
                        .query_row(
                            "SELECT id, code, name, equipment_type, active, revision
                             FROM equipment WHERE id = ?1",
                            [id.to_string()],
                            read_equipment,
                        )
                        .optional()
                        .map_err(StorageError::from)?
                        .ok_or_else(|| {
                            ApiError::new(
                                StatusCode::NOT_FOUND,
                                "REFERENCE_NOT_FOUND",
                                "equipment not found",
                            )
                            .with_details(json!({ "entity": "EQUIPMENT", "id": id.to_string() }))
                        })?;
                    if current.revision != command.base_revision {
                        return Err(ApiError::new(
                            StatusCode::CONFLICT,
                            "REVISION_CONFLICT",
                            "equipment revision changed",
                        )
                        .with_details(json!({ "current_revision": current.revision })));
                    }
                    let snapshot = command.snapshot(current.code.clone());
                    if snapshot == current.snapshot() {
                        return response(&current);
                    }
                    let revision = current
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| ApiError::internal("equipment revision overflow"))?;
                    let row = equipment(id, snapshot, revision);
                    append(
                        ledger,
                        tx,
                        &state,
                        actor,
                        command.command_id,
                        recorded_at,
                        &row,
                    )?;
                    response(&row)
                },
            )
        })
        .await?;
    parse_response(&response)
}

pub async fn list(state: AppState) -> Result<Vec<Equipment>, ApiError> {
    state
        .readers
        .call(|conn| -> Result<_, ApiError> {
            let mut statement = conn
                .prepare(
                    "SELECT id, code, name, equipment_type, active, revision
                     FROM equipment ORDER BY code COLLATE BINARY",
                )
                .map_err(StorageError::from)?;
            Ok(statement
                .query_map([], read_equipment)
                .map_err(StorageError::from)?
                .collect::<Result<_, _>>()
                .map_err(StorageError::from)?)
        })
        .await
}

fn read_equipment(row: &Row<'_>) -> boh_storage::rusqlite::Result<Equipment> {
    let convert = |error: boh_domain::DomainError| {
        boh_storage::rusqlite::Error::FromSqlConversionFailure(
            0,
            boh_storage::rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    Ok(Equipment {
        equipment_id: AggregateId::parse(&row.get::<_, String>(0)?).map_err(convert)?,
        code: row.get(1)?,
        name: row.get(2)?,
        equipment_type: EquipmentType::parse(&row.get::<_, String>(3)?).map_err(convert)?,
        active: row.get(4)?,
        revision: row.get(5)?,
    })
}

fn equipment(id: AggregateId, snapshot: EquipmentSnapshot, revision: i64) -> Equipment {
    Equipment {
        equipment_id: id,
        code: snapshot.code,
        name: snapshot.name,
        equipment_type: snapshot.equipment_type,
        active: snapshot.active,
        revision,
    }
}

fn normalized(command: &impl Serialize, id: Option<AggregateId>) -> Result<String, ApiError> {
    let mut value = serde_json::to_value(command).map_err(ApiError::internal)?;
    let object = value.as_object_mut().ok_or_else(ApiError::validation)?;
    object.remove("command_id");
    object.remove("sent_at");
    if let Some(id) = id {
        object.insert("equipment_id".into(), json!(id));
    }
    serde_json::to_string(&value).map_err(ApiError::internal)
}

fn response(row: &Equipment) -> Result<String, ApiError> {
    serde_json::to_string(&ok(json!({ "equipment": row })).0).map_err(ApiError::internal)
}

fn parse_response(response: &str) -> Result<Value, ApiError> {
    serde_json::from_str(response).map_err(ApiError::internal)
}

fn append(
    ledger: &Ledger<'_>,
    tx: &boh_storage::rusqlite::Transaction<'_>,
    state: &AppState,
    actor: Actor,
    command_id: CommandId,
    recorded_at: UnixMillis,
    row: &Equipment,
) -> Result<(), ApiError> {
    let date = business_date(recorded_at, &state.timezone, state.business_day_cutoff)
        .map_err(ApiError::internal)?;
    let payload = serde_json::to_string(&EquipmentChanged {
        entity: EquipmentEntity::Equipment,
        source: MasterDataSource::Local,
        snapshot: row.snapshot(),
    })
    .map_err(ApiError::internal)?;
    ledger.append(&Event {
        id: EventId::from_parts(recorded_at, ledger::entropy(tx)?).map_err(StorageError::from)?,
        event_type: "MASTER_DATA_CHANGED".into(),
        schema_version: 1,
        aggregate_type: "EQUIPMENT".into(),
        aggregate_id: row.equipment_id,
        aggregate_version: row.revision,
        command_id,
        actor_id: actor.employee_id,
        device_id: actor.device_id,
        business_date: date.to_string(),
        occurred_at: recorded_at,
        recorded_at,
        payload,
    })?;
    Ok(())
}
