//! Inventory and lot lookups each use a single reader transaction.

use std::collections::BTreeMap;

use axum::http::StatusCode;
use boh_domain::inventory::{Inventory, InventoryItem, InventoryQuery, Lot, LotDetails};
use boh_domain::lot::LotId;
use boh_domain::time::business_date;
use boh_domain::{AggregateId, DomainError, UnixMillis};
use boh_storage::StorageError;
use boh_storage::rusqlite::{OptionalExtension, Row, params};
use serde_json::json;

use crate::AppState;
use crate::http::ApiError;

pub async fn list(state: AppState, query: InventoryQuery) -> Result<Inventory, ApiError> {
    let readers = state.readers.clone();
    readers
        .call(move |conn| -> Result<_, ApiError> {
            let date = business_date(
                state.clock.now(),
                &state.timezone,
                state.business_day_cutoff,
            )?;
            let item_id = query.item_id.map(|id| id.to_string());
            let mut statement = conn
                .prepare(
                    "SELECT items.id, inventory_on_hand.qty, coalesce(inventory_unallocated.qty, 0)
                     FROM items JOIN inventory_on_hand ON inventory_on_hand.item_id = items.id
                     LEFT JOIN inventory_unallocated ON inventory_unallocated.item_id = items.id
                     WHERE (?1 IS NULL OR items.id = ?1) ORDER BY items.code COLLATE BINARY",
                )
                .map_err(|error| StorageError::sqlite("准备查询库存明细", error))?;
            let mut items = statement
                .query_map([&item_id], |row| {
                    Ok(InventoryItem {
                        item_id: parse_id(row, 0, AggregateId::parse)?,
                        on_hand_qty: row.get(1)?,
                        unallocated_qty: row.get(2)?,
                        lots: Vec::new(),
                    })
                })
                .map_err(|error| StorageError::sqlite("查询库存明细", error))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| StorageError::sqlite("读取库存明细", error))?;
            let mut statement = conn
                .prepare(
                    "SELECT lots.lot_id, lots.item_id, lots.origin, lots.remaining_qty,
                            events.occurred_at, lots.expires_at, lots.manufacturer_lot_no
                     FROM inventory_lots AS lots
                     JOIN store_events AS events ON events.seq = lots.source_event_seq
                     WHERE lots.remaining_qty <> 0 AND (?1 IS NULL OR lots.item_id = ?1)
                     ORDER BY lots.item_id, lots.lot_date, lots.lot_serial",
                )
                .map_err(|error| StorageError::sqlite("准备查询库存批次", error))?;
            let rows = statement
                .query_map([&item_id], read_lot)
                .map_err(|error| StorageError::sqlite("查询库存批次", error))?;
            let mut lots: BTreeMap<AggregateId, Vec<LotDetails>> = BTreeMap::new();
            for row in rows {
                let row = row.map_err(|error| StorageError::sqlite("读取库存批次", error))?;
                lots.entry(row.item_id).or_default().push(row.details);
            }
            for item in &mut items {
                item.lots = lots.remove(&item.item_id).unwrap_or_default();
            }
            Ok(Inventory {
                business_date: date.to_string(),
                items,
            })
        })
        .await
}

pub async fn lookup(state: AppState, lot_id: LotId) -> Result<Lot, ApiError> {
    state
        .readers
        .call(move |conn| -> Result<_, ApiError> {
            conn.query_row(
                "SELECT lots.lot_id, lots.item_id, lots.origin, lots.remaining_qty,
                        events.occurred_at, lots.expires_at, lots.manufacturer_lot_no
                 FROM inventory_lots AS lots
                 JOIN store_events AS events ON events.seq = lots.source_event_seq
                 WHERE lots.lot_id = ?1",
                params![lot_id.to_string()],
                read_lot,
            )
            .optional()
            .map_err(|error| StorageError::sqlite("查询批次", error))?
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "REFERENCE_NOT_FOUND",
                    "lot not found",
                )
                .with_details(json!({ "entity": "LOT", "id": lot_id }))
            })
        })
        .await
}

fn parse_id<T>(
    row: &Row<'_>,
    index: usize,
    parse: impl FnOnce(&str) -> Result<T, DomainError>,
) -> boh_storage::rusqlite::Result<T> {
    parse(&row.get::<_, String>(index)?).map_err(|error| {
        boh_storage::rusqlite::Error::FromSqlConversionFailure(
            index,
            boh_storage::rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn read_lot(row: &Row<'_>) -> boh_storage::rusqlite::Result<Lot> {
    Ok(Lot {
        item_id: parse_id(row, 1, AggregateId::parse)?,
        details: LotDetails {
            lot_id: parse_id(row, 0, LotId::parse)?,
            origin: row.get(2)?,
            remaining_qty: row.get(3)?,
            source_occurred_at: UnixMillis(row.get(4)?),
            expires_at: row.get::<_, Option<i64>>(5)?.map(UnixMillis),
            manufacturer_lot_no: row.get(6)?,
        },
    })
}
