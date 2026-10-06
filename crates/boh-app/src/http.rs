//! 路由与统一响应信封。

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boh_storage::{StorageError, schema_version};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::AppState;

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
        // 内部细节只进日志，不返回给客户端。
        tracing::error!(error = %err, "storage error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "STORAGE_ERROR",
            "internal storage error",
        )
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .with_state(state)
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
                "error": { "code": "STORAGE_ERROR", "message": "internal storage error", "details": {} }
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
