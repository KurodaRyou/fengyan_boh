//! Shared matching and rejection for per-line confirmations.

use axum::http::StatusCode;
use serde::Serialize;
use serde_json::{Value, json};

use crate::http::ApiError;

#[derive(Debug, Serialize)]
pub(crate) struct Confirmation {
    pub line: usize,
    pub code: &'static str,
    pub details: Value,
}

pub(crate) fn acknowledged(item: &Confirmation, codes: &[String]) -> bool {
    codes.iter().any(|code| code == item.code)
}

pub(crate) fn require(mut pending: Vec<Confirmation>) -> Result<(), ApiError> {
    if pending.is_empty() {
        return Ok(());
    }
    pending.sort_by(|a, b| (a.line, a.code).cmp(&(b.line, b.code)));
    Err(ApiError::new(
        StatusCode::CONFLICT,
        "CONFIRMATION_REQUIRED",
        "confirm actual conditions",
    )
    .with_details(json!({ "confirmations": pending })))
}
