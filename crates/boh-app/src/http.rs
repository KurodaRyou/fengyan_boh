//! 路由与统一响应信封。

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boh_storage::{StorageError, schema_version};
use serde::Serialize;

use crate::AppState;

/// 所有接口的统一响应格式：`{ "success", "data", "error" }`。
#[derive(Debug, Serialize)]
pub struct Envelope<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<ErrorBody>,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// 稳定的大写蛇形错误码，客户端按它判断。
    pub code: &'static str,
    pub message: String,
}

pub fn ok<T: Serialize>(data: T) -> Json<Envelope<T>> {
    Json(Envelope {
        success: true,
        data: Some(data),
        error: None,
    })
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Envelope::<()> {
            success: false,
            data: None,
            error: Some(ErrorBody {
                code: self.code,
                message: self.message,
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
            serde_json::json!({ "success": true, "data": 1, "error": null })
        );
    }
}
