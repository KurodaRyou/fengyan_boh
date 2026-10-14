//! Shared item and unit conversion checks for inventory commands.

use super::missing_reference;
use crate::http::ApiError;
use axum::http::StatusCode;
use boh_domain::quantity::UnitFactor;
use boh_domain::{AggregateId, master_data::ItemCategory, receiving::ReceiptInput};
use boh_storage::{
    StorageError,
    rusqlite::{Connection, OptionalExtension, params},
};
use serde_json::json;

pub(super) fn validate_item(
    conn: &Connection,
    index: Option<usize>,
    item_id: AggregateId,
    input: &ReceiptInput,
) -> Result<(String, ItemCategory), ApiError> {
    let current: Option<(String, Option<UnitFactor>, String, String)> = conn
        .query_row(
            "SELECT items.base_unit, item_units.base_qty_per_unit, items.code, items.category FROM items
             LEFT JOIN item_units ON item_units.item_id = items.id AND item_units.unit_code = ?2
             WHERE items.id = ?1",
            params![item_id.to_string(), input.unit_code],
            |row| Ok((
                row.get(0)?,
                row.get::<_, Option<i64>>(1)?.map(UnitFactor::from_nano),
                row.get(2)?,
                row.get(3)?,
            )),
        )
        .optional()
        .map_err(|error| StorageError::sqlite("查询物料", error))?;
    let (base_unit, factor, code, category) =
        current.ok_or_else(|| missing_reference("ITEM", item_id))?;
    let mut details = json!({ "item_id": item_id, "unit_code": input.unit_code });
    if let Some(index) = index {
        details["line"] = json!(index);
    }
    let factor = if input.unit_code == base_unit {
        UnitFactor::ONE
    } else {
        factor.ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "UNKNOWN_UNIT",
                "unit is not configured",
            )
            .with_details(details.clone())
        })?
    };
    if factor != input.base_qty_per_unit {
        details["base_qty_per_unit"] = json!(factor);
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "UNIT_CONVERSION_CHANGED",
            "unit conversion changed",
        )
        .with_details(details));
    }
    Ok((
        code,
        ItemCategory::parse(&category).map_err(StorageError::from)?,
    ))
}
