//! Shared projection writes. Inputs are persisted event decisions, never live policy.

use boh_domain::inventory::Allocation;
use boh_domain::lot::LotId;
use boh_domain::{AggregateId, EventId, UnixMillis};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::StorageError;
use crate::ledger::Event;

pub(crate) struct NewLot<'a> {
    pub(crate) lot_id: &'a LotId,
    pub(crate) item_id: AggregateId,
    pub(crate) source_line_no: i64,
    pub(crate) origin: &'static str,
    pub(crate) qty: i64,
    pub(crate) expires_at: Option<UnixMillis>,
    pub(crate) manufacturer_lot_no: Option<&'a str>,
}

/// Own the event-local expansion counter, shared by all of its payload lines.
pub(crate) struct Movements<'a> {
    event: &'a Event,
    seq: i64,
    number: i64,
}

impl<'a> Movements<'a> {
    pub(crate) fn new(tx: &Transaction<'_>, event: &'a Event) -> Result<Self, StorageError> {
        // Online append and replay both read only this event's stored seq.
        let seq = tx
            .query_row(
                "SELECT seq FROM store_events WHERE id = ?1",
                [event.id.to_string()],
                |row| row.get(0),
            )
            .map_err(|error| StorageError::sqlite("查询库存事件序号", error))?;
        Ok(Self {
            event,
            seq,
            number: 0,
        })
    }

    pub(crate) fn new_lot(
        &mut self,
        tx: &Transaction<'_>,
        kind: &'static str,
        line: NewLot<'_>,
    ) -> Result<(), StorageError> {
        tx.execute(
            "INSERT INTO inventory_lots (lot_id, item_id, lot_date, lot_serial, origin, source_event_seq,
                 source_line_no, remaining_qty, expires_at, manufacturer_lot_no)
             VALUES (?1, ?2, ?3, ?4, ?10, ?5, ?6, ?7, ?8, ?9)",
            params![
                line.lot_id.to_string(),
                line.item_id.to_string(),
                line.lot_id.date().to_string(),
                line.lot_id.serial(),
                self.seq,
                line.source_line_no,
                line.qty,
                line.expires_at.map(|time| time.0),
                line.manufacturer_lot_no,
                line.origin
            ],
        )
        .map_err(|error| StorageError::sqlite("插入库存批次", error))?;

        self.insert(
            tx,
            kind,
            Movement {
                item_id: line.item_id,
                lot_id: Some(line.lot_id),
                source: "NEW_LOT",
                nominal: line.qty,
                absorber: None,
            },
        )
    }

    pub(crate) fn deduct(
        &mut self,
        tx: &Transaction<'_>,
        kind: &'static str,
        item_id: AggregateId,
        alloc: &[Allocation],
    ) -> Result<(), StorageError> {
        for part in alloc {
            if let Some(lot_id) = &part.lot_id {
                let current: i64 = tx
                    .query_row(
                        "SELECT remaining_qty FROM inventory_lots WHERE lot_id = ?1",
                        [lot_id.to_string()],
                        |row| row.get(0),
                    )
                    .map_err(|error| StorageError::sqlite("查询库存批次余量", error))?;
                let remaining = current
                    .checked_sub(part.qty)
                    .ok_or_else(|| invalid("inventory lot quantity overflow"))?;
                tx.execute(
                    "UPDATE inventory_lots SET remaining_qty = ?2 WHERE lot_id = ?1",
                    params![lot_id.to_string(), remaining],
                )
                .map_err(|error| StorageError::sqlite("扣减库存批次余量", error))?;
            } else {
                let current: Option<i64> = tx
                    .query_row(
                        "SELECT qty FROM inventory_unallocated WHERE item_id = ?1",
                        [item_id.to_string()],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|error| StorageError::sqlite("查询库存账外缺口", error))?;
                let remaining = current
                    .unwrap_or(0)
                    .checked_sub(part.qty)
                    .ok_or_else(|| invalid("inventory unallocated quantity overflow"))?;
                match current {
                    Some(_) => {
                        tx.execute(
                            "UPDATE inventory_unallocated SET qty = ?2 WHERE item_id = ?1",
                            params![item_id.to_string(), remaining],
                        )
                        .map_err(|error| StorageError::sqlite("扣减库存账外缺口", error))?;
                    }
                    None => {
                        tx.execute(
                            "INSERT INTO inventory_unallocated (item_id, qty) VALUES (?1, ?2)",
                            params![item_id.to_string(), remaining],
                        )
                        .map_err(|error| StorageError::sqlite("插入库存账外缺口", error))?;
                    }
                }
            }

            let nominal = part
                .qty
                .checked_neg()
                .ok_or_else(|| invalid("inventory movement quantity overflow"))?;
            self.insert(
                tx,
                kind,
                Movement {
                    item_id,
                    lot_id: part.lot_id.as_ref(),
                    source: part.source.as_str(),
                    nominal,
                    absorber: None,
                },
            )?;
        }
        Ok(())
    }

    pub(crate) fn absorbed(
        &mut self,
        tx: &Transaction<'_>,
        kind: &'static str,
        item_id: AggregateId,
        qty: i64,
        absorber: EventId,
    ) -> Result<(), StorageError> {
        let nominal = qty
            .checked_neg()
            .ok_or_else(|| invalid("inventory movement quantity overflow"))?;
        self.insert(
            tx,
            kind,
            Movement {
                item_id,
                lot_id: None,
                source: "ABSORBED",
                nominal,
                absorber: Some(absorber),
            },
        )
    }

    fn insert(
        &mut self,
        tx: &Transaction<'_>,
        kind: &'static str,
        row: Movement<'_>,
    ) -> Result<(), StorageError> {
        tx.execute(
            "INSERT INTO inventory_movements (event_seq, movement_no, item_id, lot_id, kind, alloc_source,
                 nominal_qty, qty_delta, absorbed_by_event_id, physical_at, business_date)
             VALUES (?1, ?2, ?3, ?4, ?11, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![self.seq, self.number, row.item_id.to_string(), row.lot_id.map(ToString::to_string), row.source,
                row.nominal, if row.absorber.is_some() { 0 } else { row.nominal }, row.absorber.map(|id| id.to_string()),
                self.event.occurred_at.0, self.event.business_date, kind],
        ).map_err(|error| StorageError::sqlite("插入库存流水", error))?;
        self.number = self
            .number
            .checked_add(1)
            .ok_or_else(|| invalid("inventory movement number overflow"))?;
        Ok(())
    }
}

struct Movement<'a> {
    item_id: AggregateId,
    lot_id: Option<&'a LotId>,
    source: &'static str,
    nominal: i64,
    absorber: Option<EventId>,
}

fn invalid(message: &str) -> StorageError {
    StorageError::InvalidEvent(message.into())
}
