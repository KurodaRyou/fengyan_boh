//! Inventory queries and command-local decision reads. This module never writes projections.

use std::collections::BTreeMap;

use axum::http::StatusCode;
use boh_domain::inventory::{
    Allocation, Inventory, InventoryItem, InventoryQuery, ItemBalance, Lot, LotBalance, LotDetails,
    next_lot_serial,
};
use boh_domain::lot::LotId;
use boh_domain::master_data::ItemCategory;
use boh_domain::quantity::Quantity;
use boh_domain::time::{Date, business_date};
use boh_domain::{AggregateId, DomainError, UnixMillis};
use boh_storage::StorageError;
use boh_storage::rusqlite::{Connection, OptionalExtension, Row, params};
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
                        on_hand_qty: Quantity::from_milli(row.get(1)?),
                        unallocated_qty: Quantity::from_milli(row.get(2)?),
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
            remaining_qty: Quantity::from_milli(row.get(3)?),
            source_occurred_at: UnixMillis(row.get(4)?),
            expires_at: row.get::<_, Option<i64>>(5)?.map(UnixMillis),
            manufacturer_lot_no: row.get(6)?,
        },
    })
}

/// Load lazily, after the caller has validated the current line's references.
#[derive(Default)]
pub(super) struct WorkingSet {
    balances: BTreeMap<AggregateId, ItemBalance>,
}

impl WorkingSet {
    pub(super) fn get(
        &mut self,
        conn: &Connection,
        item_id: AggregateId,
    ) -> Result<&mut ItemBalance, ApiError> {
        use std::collections::btree_map::Entry;
        Ok(match self.balances.entry(item_id) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(load_balance(conn, item_id)?),
        })
    }
}

pub(super) fn allocate(
    balance: &mut ItemBalance,
    specified: Option<usize>,
    qty: Quantity,
    index: usize,
    item_id: AggregateId,
) -> Result<Vec<Allocation>, ApiError> {
    balance
        .allocate(specified, qty)
        .map_err(|error| stock_error(error, "分配库存", index, item_id))
}

pub(super) fn receive(
    balance: &mut ItemBalance,
    lot_id: LotId,
    qty: Quantity,
    index: usize,
    item_id: AggregateId,
) -> Result<(), ApiError> {
    balance
        .receive(lot_id, qty)
        .map_err(|error| stock_error(error, "收货入库", index, item_id))
}

fn stock_error(
    error: DomainError,
    operation: &'static str,
    index: usize,
    item_id: AggregateId,
) -> ApiError {
    match error {
        DomainError::StockLimitExceeded { lot_id } => {
            let mut details = json!({ "line": index, "item_id": item_id });
            if let Some(lot_id) = lot_id {
                details["lot_id"] = json!(lot_id);
            }
            ApiError::new(
                StatusCode::CONFLICT,
                "STOCK_LIMIT_EXCEEDED",
                "inventory state exceeds the stock limit",
            )
            .with_details(details)
        }
        DomainError::InvalidId(_) | DomainError::InvalidField(_) => {
            ApiError::from(StorageError::external(operation, error))
        }
    }
}

fn load_balance(conn: &Connection, item_id: AggregateId) -> Result<ItemBalance, ApiError> {
    let mut statement = conn.prepare(
        "SELECT lot_id, remaining_qty FROM inventory_lots WHERE item_id = ?1 ORDER BY lot_date, lot_serial",
    ).map_err(|error| StorageError::sqlite("准备查询库存批次", error))?;
    let lots: Vec<LotBalance> = statement
        .query_map([item_id.to_string()], |row| {
            let lot_id = LotId::parse(&row.get::<_, String>(0)?).map_err(|error| {
                boh_storage::rusqlite::Error::FromSqlConversionFailure(
                    0,
                    boh_storage::rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(LotBalance {
                lot_id,
                remaining: Quantity::from_milli(row.get(1)?),
            })
        })
        .map_err(|error| StorageError::sqlite("查询库存批次", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StorageError::sqlite("读取库存批次", error))?;
    let unallocated = conn
        .query_row(
            "SELECT qty FROM inventory_unallocated WHERE item_id = ?1",
            [item_id.to_string()],
            |row| Ok(Quantity::from_milli(row.get(0)?)),
        )
        .optional()
        .map_err(|error| StorageError::sqlite("查询库存账外缺口", error))?
        .unwrap_or(Quantity::ZERO);
    // Stored state outside the limits is a projection fault, not a conflict caused
    // by the current request line.
    ItemBalance::new(lots, unallocated)
        .map_err(|error| ApiError::from(StorageError::external("读取库存工作集", error)))
}

/// One command's serials for its single, calibrated lot date.
pub(super) struct LotNumbers {
    lot_date: Date,
    calendar_date: String,
    last_serials: BTreeMap<AggregateId, i64>,
}

impl LotNumbers {
    pub(super) fn new(lot_date: Date) -> Self {
        let calendar_date = lot_date.to_string();
        Self {
            lot_date,
            calendar_date,
            last_serials: BTreeMap::new(),
        }
    }

    pub(super) fn calendar_date(&self) -> &str {
        &self.calendar_date
    }

    pub(super) fn allocate(
        &mut self,
        conn: &Connection,
        index: usize,
        item_id: AggregateId,
        category: ItemCategory,
        code: &str,
    ) -> Result<LotId, ApiError> {
        let last_serial = match self.last_serials.get(&item_id) {
            Some(serial) => *serial,
            None => conn
                .query_row(
                    "SELECT coalesce(max(lot_serial), 0) FROM inventory_lots
                 WHERE item_id = ?1 AND lot_date = ?2",
                    params![item_id.to_string(), self.calendar_date],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| StorageError::sqlite("查询批次流水号", error))?,
        };
        let serial = next_lot_serial(last_serial).map_err(|_| {
            ApiError::new(
                StatusCode::CONFLICT,
                "LOT_SERIAL_EXHAUSTED",
                "lot serials exhausted for this item and date",
            )
            .with_details(
                json!({ "line": index, "item_id": item_id, "lot_date": self.calendar_date }),
            )
        })?;
        self.last_serials.insert(item_id, serial);
        LotId::from_parts(category, code, self.lot_date, serial)
            .map_err(|error| ApiError::from(StorageError::from(error)))
    }
}
