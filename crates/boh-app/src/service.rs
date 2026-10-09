//! Command orchestration. Business checks execute after ledger idempotency.

pub(crate) mod master_data;
pub(crate) mod receiving;

use axum::http::StatusCode;
use boh_domain::equipment::{
    CreateEquipment, Equipment, EquipmentChanged, EquipmentEntity, EquipmentSnapshot,
    EquipmentType, MasterDataSource, UpdateEquipment,
};
use boh_domain::temperature::{
    LogTemperature, TemperatureLogged, TemperatureQuery, TemperatureReading,
};
use boh_domain::time::{CaptureTimes, business_date, calibrate};
use boh_domain::{AggregateId, CommandId, EventId, UnixMillis};
use boh_storage::StorageError;
use boh_storage::ledger::{self, Command, Event, ExecuteError, Ledger};
use boh_storage::rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    AppState,
    actor::Actor,
    http::{ApiError, WarningBody, ok},
};

impl From<ExecuteError> for ApiError {
    #[track_caller]
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

#[tracing::instrument(name = "command", skip_all, fields(command_type = "equipment.create", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
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
                        .map_err(|error| StorageError::sqlite("查询设备", error))?;
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

#[tracing::instrument(name = "command", skip_all, fields(command_type = "equipment.update", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
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
                        .map_err(|error| StorageError::sqlite("查询设备", error))?
                        .ok_or_else(|| missing_reference("EQUIPMENT", id))?;
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
                        .ok_or_else(|| ApiError::internal_message("equipment revision overflow"))?;
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
                .map_err(|error| StorageError::sqlite("准备查询设备", error))?;
            Ok(statement
                .query_map([], read_equipment)
                .map_err(|error| StorageError::sqlite("查询设备", error))?
                .collect::<Result<_, _>>()
                .map_err(|error| StorageError::sqlite("读取设备", error))?)
        })
        .await
}

#[tracing::instrument(name = "command", skip_all, fields(command_type = "temperature.log", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn log_temperature(
    state: AppState,
    actor: Actor,
    command: LogTemperature,
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
                    command_type: "temperature.log",
                    request: &request,
                    recorded_at,
                },
                |ledger| {
                    let calibrated = calibrate(
                        CaptureTimes {
                            captured_at: command.captured_at,
                            sent_at: command.sent_at,
                            started_captured_at: None,
                        },
                        recorded_at,
                    )
                    .map_err(ApiError::from)?;
                    let date = business_date(
                        calibrated.occurred_at,
                        &state.timezone,
                        state.business_day_cutoff,
                    )
                    .map_err(ApiError::from)?;
                    let exists: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM equipment WHERE id = ?1)",
                            [command.equipment_id.to_string()],
                            |row| row.get(0),
                        )
                        .map_err(|error| StorageError::sqlite("查询设备", error))?;
                    if !exists {
                        return Err(missing_reference("EQUIPMENT", command.equipment_id));
                    }
                    let id = AggregateId::from_parts(recorded_at, ledger::entropy(tx)?)
                        .map_err(StorageError::from)?;
                    let payload = serde_json::to_string(&TemperatureLogged {
                        equipment_id: command.equipment_id,
                        celsius_x10: command.celsius_x10,
                        note: command.note.clone(),
                    })
                    .map_err(|error| {
                        ApiError::from(StorageError::external("序列化温度事件", error))
                    })?;
                    let row = TemperatureReading {
                        temperature_reading_id: id,
                        equipment_id: command.equipment_id,
                        celsius_x10: command.celsius_x10,
                        note: command.note,
                        business_date: date.to_string(),
                        occurred_at: calibrated.occurred_at,
                        recorded_at,
                        actor_id: actor.employee_id,
                        device_id: actor.device_id,
                    };
                    ledger.append(&Event {
                        id: EventId::from_parts(recorded_at, ledger::entropy(tx)?)
                            .map_err(StorageError::from)?,
                        event_type: "TEMPERATURE_LOGGED".into(),
                        schema_version: 1,
                        aggregate_type: "TEMPERATURE_READING".into(),
                        aggregate_id: id,
                        aggregate_version: 1,
                        command_id: command.command_id,
                        actor_id: row.actor_id,
                        device_id: row.device_id,
                        business_date: row.business_date.clone(),
                        occurred_at: row.occurred_at,
                        recorded_at,
                        payload,
                    })?;
                    let mut response = ok(json!({ "temperature_reading": row })).0;
                    if calibrated.capture_time_adjusted {
                        response.warnings.push(WarningBody {
                            code: "CAPTURE_TIME_ADJUSTED",
                            message: "capture time adjusted to the receiving time".into(),
                            details: Default::default(),
                        });
                    }
                    // Save the entire envelope so retries preserve the original warning and times.
                    serde_json::to_string(&response).map_err(|error| {
                        ApiError::from(StorageError::external("保存温度命令响应", error))
                    })
                },
            )
        })
        .await?;
    parse_response(&response)
}

pub async fn list_temperature_readings(
    state: AppState,
    query: TemperatureQuery,
) -> Result<Vec<TemperatureReading>, ApiError> {
    state
        .readers
        .call(move |conn| -> Result<_, ApiError> {
            let mut statement = conn
                .prepare(
                    "SELECT id, equipment_id, celsius_x10, note, business_date, occurred_at,
                            recorded_at, actor_id, device_id
                     FROM temperature_readings
                     WHERE business_date = ?1 AND (?2 IS NULL OR equipment_id = ?2)
                     ORDER BY occurred_at, event_seq",
                )
                .map_err(|error| StorageError::sqlite("准备查询温度记录", error))?;
            Ok(statement
                .query_map(
                    params![
                        query.business_date,
                        query.equipment_id.map(|id| id.to_string())
                    ],
                    read_temperature,
                )
                .map_err(|error| StorageError::sqlite("查询温度记录", error))?
                .collect::<Result<_, _>>()
                .map_err(|error| StorageError::sqlite("读取温度记录", error))?)
        })
        .await
}

fn read_temperature(row: &Row<'_>) -> boh_storage::rusqlite::Result<TemperatureReading> {
    let id = |index| {
        AggregateId::parse(&row.get::<_, String>(index)?).map_err(|error| {
            boh_storage::rusqlite::Error::FromSqlConversionFailure(
                index,
                boh_storage::rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    };
    Ok(TemperatureReading {
        temperature_reading_id: id(0)?,
        equipment_id: id(1)?,
        celsius_x10: row.get(2)?,
        note: row.get(3)?,
        business_date: row.get(4)?,
        occurred_at: UnixMillis(row.get(5)?),
        recorded_at: UnixMillis(row.get(6)?),
        actor_id: id(7)?,
        device_id: id(8)?,
    })
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

fn missing_reference(entity: &str, id: AggregateId) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "REFERENCE_NOT_FOUND",
        "referenced entity not found",
    )
    .with_details(json!({ "entity": entity, "id": id }))
}

fn normalized(command: &impl Serialize, id: Option<AggregateId>) -> Result<String, ApiError> {
    let mut value = serde_json::to_value(command)
        .map_err(|error| ApiError::from(StorageError::external("规范化设备命令", error)))?;
    let object = value.as_object_mut().ok_or_else(ApiError::validation)?;
    object.remove("command_id");
    object.remove("sent_at");
    if let Some(id) = id {
        object.insert("equipment_id".into(), json!(id));
    }
    serde_json::to_string(&value)
        .map_err(|error| ApiError::from(StorageError::external("规范化设备命令", error)))
}

fn response(row: &Equipment) -> Result<String, ApiError> {
    serde_json::to_string(&ok(json!({ "equipment": row })).0)
        .map_err(|error| ApiError::from(StorageError::external("序列化设备响应", error)))
}

fn parse_response(response: &str) -> Result<Value, ApiError> {
    serde_json::from_str(response)
        .map_err(|error| ApiError::from(StorageError::external("解码已保存的命令响应", error)))
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
        .map_err(|error| ApiError::from(StorageError::external("计算设备事件营业日", error)))?;
    let payload = serde_json::to_string(&EquipmentChanged {
        entity: EquipmentEntity::Equipment,
        source: MasterDataSource::Local,
        snapshot: row.snapshot(),
    })
    .map_err(|error| ApiError::from(StorageError::external("序列化设备事件", error)))?;
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
