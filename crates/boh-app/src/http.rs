//! 路由与统一响应信封。

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use boh_storage::{StorageError, schema_version};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::AppState;
use crate::service::master_data;
use crate::{
    actor::{Actor, Manager},
    service,
};
use boh_domain::AggregateId;
use boh_domain::equipment::{CreateEquipment, UpdateEquipment};
use boh_domain::master_data::*;
use boh_domain::receiving::CreateReceipt;
use boh_domain::temperature::{LogTemperature, TemperatureQuery};
use boh_domain::time::TimeError;

/// 所有接口的统一响应格式：`{ "success", "data", "warnings", "error" }`。
#[derive(Debug, Serialize)]
pub struct Envelope<T> {
    pub success: bool,
    pub data: Option<T>,
    pub warnings: Vec<WarningBody>,
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// 稳定的大写蛇形错误码，客户端按它判断。
    pub code: &'static str,
    pub message: String,
    pub details: Map<String, Value>,
}

#[derive(Debug, Serialize)]
pub struct WarningBody {
    pub code: &'static str,
    pub message: String,
    pub details: Map<String, Value>,
}

pub fn ok<T: Serialize>(data: T) -> Json<Envelope<T>> {
    Json(Envelope {
        success: true,
        data: Some(data),
        warnings: Vec::new(),
        error: None,
    })
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Map<String, Value>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: Map::new(),
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        if let Value::Object(details) = details {
            self.details = details;
        }
        self
    }

    #[track_caller]
    pub fn internal_message(message: &'static str) -> Self {
        Self::internal(StorageError::Message(message))
    }

    pub fn validation() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "invalid request",
        )
    }

    #[track_caller]
    pub fn internal(error: impl std::error::Error + 'static) -> Self {
        let caller = std::panic::Location::caller();
        let location = (&error as &dyn std::error::Error)
            .downcast_ref::<StorageError>()
            .and_then(StorageError::location)
            .unwrap_or(caller);
        tracing::error!(error = %boh_storage::Diagnostic(&error), %location, "internal error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "internal error",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Envelope::<()> {
            success: false,
            data: None,
            warnings: Vec::new(),
            error: Some(ErrorBody {
                code: self.code,
                message: self.message,
                details: self.details,
            }),
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<StorageError> for ApiError {
    fn from(err: StorageError) -> Self {
        Self::internal(err)
    }
}

impl From<TimeError> for ApiError {
    fn from(error: TimeError) -> Self {
        match error {
            TimeError::CaptureTooOld => Self::new(
                StatusCode::BAD_REQUEST,
                "CAPTURE_TOO_OLD",
                "capture is more than 72 hours old",
            ),
            TimeError::InvalidProductionTime => Self::new(
                StatusCode::BAD_REQUEST,
                "INVALID_PRODUCTION_TIME",
                "production start is after completion",
            ),
            TimeError::OutOfRange => Self::validation(),
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route(
            "/api/v1/equipment",
            get(list_equipment).post(create_equipment),
        )
        .route("/api/v1/equipment/{equipment_id}", put(update_equipment))
        .route(
            "/api/v1/temperature-readings",
            get(list_temperature_readings).post(log_temperature),
        )
        .route("/api/v1/receipts", post(create_receipt))
        .route("/api/v1/items", get(list_items).post(create_item))
        .route("/api/v1/items/{item_id}", put(update_item))
        .route("/api/v1/recipes", get(list_recipes).post(create_recipe))
        .route("/api/v1/recipes/{recipe_id}", put(update_recipe))
        .route(
            "/api/v1/suppliers",
            get(list_suppliers).post(create_supplier),
        )
        .route("/api/v1/suppliers/{supplier_id}", put(update_supplier))
        .route(
            "/api/v1/waste-reasons",
            get(list_waste_reasons).post(create_waste_reason),
        )
        .route(
            "/api/v1/waste-reasons/{waste_reason_id}",
            put(update_waste_reason),
        )
        .route(
            "/api/v1/recipes/{recipe_id}/versions",
            post(add_recipe_version),
        )
        .fallback(route_not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state)
}

async fn route_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "ROUTE_NOT_FOUND", "route not found")
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "METHOD_NOT_ALLOWED",
        "method not allowed",
    )
}

async fn list_equipment(
    State(state): State<AppState>,
    _actor: Actor,
) -> Result<Json<Envelope<Value>>, ApiError> {
    Ok(ok(
        serde_json::json!({ "equipment": service::list(state).await? }),
    ))
}

async fn create_equipment(
    State(state): State<AppState>,
    Manager(actor): Manager,
    body: Result<Json<CreateEquipment>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(service::create(state, actor, command).await?))
}

async fn update_equipment(
    State(state): State<AppState>,
    Manager(actor): Manager,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<UpdateEquipment>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::validation())?;
    let id = AggregateId::parse(&id).map_err(|_| ApiError::validation())?;
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(service::update(state, actor, id, command).await?))
}

async fn log_temperature(
    State(state): State<AppState>,
    actor: Actor,
    body: Result<Json<LogTemperature>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(service::log_temperature(state, actor, command).await?))
}

async fn list_temperature_readings(
    State(state): State<AppState>,
    _actor: Actor,
    query: Result<Query<TemperatureQuery>, QueryRejection>,
) -> Result<Json<Envelope<Value>>, ApiError> {
    let Query(query) = query.map_err(|_| ApiError::validation())?;
    query.validate().map_err(|_| ApiError::validation())?;
    Ok(ok(serde_json::json!({
        "temperature_readings": service::list_temperature_readings(state, query).await?
    })))
}

async fn create_receipt(
    State(state): State<AppState>,
    actor: Actor,
    body: Result<Json<CreateReceipt>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(
        service::receiving::create(state, actor, command).await?,
    ))
}

async fn list_items(
    State(state): State<AppState>,
    _actor: Actor,
) -> Result<Json<Envelope<Value>>, ApiError> {
    Ok(ok(
        serde_json::json!({ "items": master_data::list_items(state).await? }),
    ))
}

async fn create_item(
    State(state): State<AppState>,
    Manager(actor): Manager,
    body: Result<Json<CreateItem>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(master_data::create_item(state, actor, command).await?))
}

async fn update_item(
    State(state): State<AppState>,
    Manager(actor): Manager,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<UpdateItem>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::validation())?;
    let id = AggregateId::parse(&id).map_err(|_| ApiError::validation())?;
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(
        master_data::update_item(state, actor, id, command).await?,
    ))
}

async fn list_recipes(
    State(state): State<AppState>,
    _actor: Actor,
) -> Result<Json<Envelope<Value>>, ApiError> {
    Ok(ok(
        serde_json::json!({ "recipes": master_data::list_recipes(state).await? }),
    ))
}

async fn create_recipe(
    State(state): State<AppState>,
    Manager(actor): Manager,
    body: Result<Json<CreateRecipe>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(
        master_data::create_recipe(state, actor, command).await?,
    ))
}

async fn update_recipe(
    State(state): State<AppState>,
    Manager(actor): Manager,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<UpdateRecipe>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::validation())?;
    let id = AggregateId::parse(&id).map_err(|_| ApiError::validation())?;
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(
        master_data::update_recipe(state, actor, id, command).await?,
    ))
}

async fn list_suppliers(
    State(state): State<AppState>,
    _actor: Actor,
) -> Result<Json<Envelope<Value>>, ApiError> {
    Ok(ok(
        serde_json::json!({ "suppliers": master_data::list_suppliers(state).await? }),
    ))
}

async fn create_supplier(
    State(state): State<AppState>,
    Manager(actor): Manager,
    body: Result<Json<CreateSupplier>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(
        master_data::create_supplier(state, actor, command).await?,
    ))
}

async fn update_supplier(
    State(state): State<AppState>,
    Manager(actor): Manager,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<UpdateSupplier>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::validation())?;
    let id = AggregateId::parse(&id).map_err(|_| ApiError::validation())?;
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(
        master_data::update_supplier(state, actor, id, command).await?,
    ))
}

async fn list_waste_reasons(
    State(state): State<AppState>,
    _actor: Actor,
) -> Result<Json<Envelope<Value>>, ApiError> {
    Ok(ok(
        serde_json::json!({ "waste_reasons": master_data::list_waste_reasons(state).await? }),
    ))
}

async fn create_waste_reason(
    State(state): State<AppState>,
    Manager(actor): Manager,
    body: Result<Json<CreateWasteReason>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(
        master_data::create_waste_reason(state, actor, command).await?,
    ))
}

async fn update_waste_reason(
    State(state): State<AppState>,
    Manager(actor): Manager,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<UpdateWasteReason>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::validation())?;
    let id = AggregateId::parse(&id).map_err(|_| ApiError::validation())?;
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(
        master_data::update_waste_reason(state, actor, id, command).await?,
    ))
}

async fn add_recipe_version(
    State(state): State<AppState>,
    Manager(actor): Manager,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<AddRecipeVersion>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Path(id) = path.map_err(|_| ApiError::validation())?;
    let id = AggregateId::parse(&id).map_err(|_| ApiError::validation())?;
    let Json(command) = body.map_err(|_| ApiError::validation())?;
    command.validate().map_err(|_| ApiError::validation())?;
    Ok(Json(
        master_data::add_recipe_version(state, actor, id, command).await?,
    ))
}

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    schema_version: i64,
    clock_regression_ms: u64,
    last_backup_ok_at: Option<boh_domain::UnixMillis>,
    last_backup_seq: Option<i64>,
    last_backup_failed_at: Option<boh_domain::UnixMillis>,
    wal_size_bytes: u64,
}

async fn health(State(state): State<AppState>) -> Result<Json<Envelope<Health>>, ApiError> {
    let (version, latest) = state
        .readers
        .call(|conn| -> Result<_, ApiError> {
            Ok((
                schema_version(conn)?,
                conn.query_row("SELECT max(recorded_at) FROM store_events", [], |r| {
                    r.get::<_, Option<i64>>(0)
                })
                .map_err(|error| StorageError::sqlite("查询事件账本", error))?,
            ))
        })
        .await?;
    let now = state.clock.now();
    let clock_regression_ms = clock_regression(latest, now);
    let mut wal_path = state.db_path.into_os_string();
    wal_path.push("-wal");
    let span = tracing::Span::current();
    let dispatch = tracing::dispatcher::get_default(Clone::clone);
    let wal_size_bytes = tokio::task::spawn_blocking(move || {
        tracing::dispatcher::with_default(&dispatch, || {
            span.in_scope(|| match std::fs::metadata(wal_path) {
                Ok(metadata) => Ok(metadata.len()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
                Err(error) => Err(ApiError::from(StorageError::io("读取 WAL 文件大小", error))),
            })
        })
    })
    .await
    .map_err(|error| ApiError::from(StorageError::join("等待 WAL 文件查询", error)))??;
    let backup = state.backup_health.snapshot();
    Ok(ok(Health {
        status: if clock_regression_ms > 300_000 || backup.last_failed_at.is_some() {
            "degraded"
        } else {
            "ok"
        },
        schema_version: version,
        clock_regression_ms,
        last_backup_ok_at: backup.last_ok_at,
        last_backup_seq: backup.last_seq,
        last_backup_failed_at: backup.last_failed_at,
        wal_size_bytes,
    }))
}

fn clock_regression(latest: Option<i64>, now: boh_domain::UnixMillis) -> u64 {
    latest
        .and_then(|time| i128::from(time).checked_sub(i128::from(now.0)))
        .and_then(|difference| u64::try_from(difference).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_regression_preserves_the_full_timestamp_difference() {
        use boh_domain::UnixMillis;
        assert_eq!(clock_regression(None, UnixMillis(i64::MIN)), 0);
        assert_eq!(
            clock_regression(Some(i64::MAX), UnixMillis(i64::MIN)),
            u64::MAX
        );
        assert_eq!(clock_regression(Some(i64::MIN), UnixMillis(i64::MAX)), 0);
    }

    #[test]
    fn success_envelope_shape() {
        let Json(body) = ok(1);
        assert_eq!(
            serde_json::to_value(body).unwrap(),
            serde_json::json!({ "success": true, "data": 1, "warnings": [], "error": null })
        );
    }

    #[tokio::test]
    async fn storage_error_envelope_hides_internal_details() {
        let response = ApiError::from(StorageError::io(
            "读取 WAL 文件",
            std::io::Error::other("private SQL details"),
        ))
        .into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "success": false, "data": null, "warnings": [],
                "error": { "code": "INTERNAL_ERROR", "message": "internal error", "details": {} }
            })
        );
    }

    #[test]
    fn warning_details_serialize_as_an_object() {
        let body = WarningBody {
            code: "CAPTURE_TIME_ADJUSTED",
            message: "adjusted".into(),
            details: Map::new(),
        };
        assert_eq!(
            serde_json::to_value(body).unwrap(),
            serde_json::json!({
                "code": "CAPTURE_TIME_ADJUSTED", "message": "adjusted", "details": {}
            })
        );
    }
}
