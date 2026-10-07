//! 路由与统一响应信封。

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use boh_storage::{StorageError, schema_version};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::AppState;
use crate::{
    actor::{Actor, Manager},
    service,
};
use boh_domain::AggregateId;
use boh_domain::equipment::{CreateEquipment, UpdateEquipment};
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

    pub fn validation() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "VALIDATION_FAILED",
            "invalid request",
        )
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "internal error");
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

#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    schema_version: i64,
}

async fn health(State(state): State<AppState>) -> Result<Json<Envelope<Health>>, ApiError> {
    let version = state.readers.call(schema_version).await?;
    Ok(ok(Health {
        status: "ok",
        schema_version: version,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let response =
            ApiError::from(StorageError::Join("private SQL details".into())).into_response();
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
