//! Shared item and unit conversion checks for inventory commands.

use super::missing_reference;
use crate::http::ApiError;
use axum::http::StatusCode;
use boh_domain::{AggregateId, master_data::ItemCategory, receiving::ReceiptInput};
use boh_storage::{
    StorageError,
    rusqlite::{Connection, OptionalExtension, params},
};
use serde_json::json;

pub(super) fn validate_item(
    conn: &Connection,
    index: usize,
    item_id: AggregateId,
    input: &ReceiptInput,
) -> Result<(String, ItemCategory), ApiError> {
    let current: Option<(String, Option<i64>, String, String)> = conn
        .query_row(
            "SELECT items.base_unit, item_units.base_qty_per_unit, items.code, items.category FROM items
             LEFT JOIN item_units ON item_units.item_id = items.id AND item_units.unit_code = ?2
             WHERE items.id = ?1",
            params![item_id.to_string(), input.unit_code],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| StorageError::sqlite("查询物料", error))?;
    let (base_unit, factor, code, category) =
        current.ok_or_else(|| missing_reference("ITEM", item_id))?;
    let factor = if input.unit_code == base_unit {
        1
    } else {
        factor.ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "UNKNOWN_UNIT",
                "unit is not configured",
            )
            .with_details(json!({
                "line": index, "item_id": item_id, "unit_code": input.unit_code
            }))
        })?
    };
    if factor != input.base_qty_per_unit {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "UNIT_CONVERSION_CHANGED",
            "unit conversion changed",
        )
        .with_details(json!({
            "line": index, "item_id": item_id, "unit_code": input.unit_code,
            "base_qty_per_unit": factor
        })));
    }
    Ok((
        code,
        ItemCategory::parse(&category).map_err(StorageError::from)?,
    ))
}
