//! Inventory queries and pure command-local stock rules.

use serde::{Deserialize, Serialize};

use crate::lot::LotId;
use crate::quantity::Quantity;
use crate::{AggregateId, DomainError, UnixMillis};

#[cfg(test)]
#[path = "inventory_tests.rs"]
mod tests;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryQuery {
    pub item_id: Option<AggregateId>,
}

#[derive(Debug, Serialize)]
pub struct LotDetails {
    pub lot_id: LotId,
    pub origin: String,
    pub remaining_qty: Quantity,
    pub source_occurred_at: UnixMillis,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<UnixMillis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer_lot_no: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Lot {
    pub item_id: AggregateId,
    #[serde(flatten)]
    pub details: LotDetails,
}

#[derive(Debug, Serialize)]
pub struct InventoryItem {
    pub item_id: AggregateId,
    pub on_hand_qty: Quantity,
    pub unallocated_qty: Quantity,
    pub lots: Vec<LotDetails>,
}

#[derive(Debug, Serialize)]
pub struct Inventory {
    pub business_date: String,
    pub items: Vec<InventoryItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AllocationSource {
    Specified,
    Fifo,
    Shortfall,
}
string_enum_serde!(AllocationSource);

impl AllocationSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Specified => "SPECIFIED",
            Self::Fifo => "FIFO",
            Self::Shortfall => "SHORTFALL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct Allocation {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::present"
    )]
    pub lot_id: Option<LotId>,
    pub qty: Quantity,
    pub source: AllocationSource,
}
object_serde!(Allocation);

/// A lot balance supplied in FIFO order by the decision-reading layer.
#[derive(Debug)]
pub struct LotBalance {
    pub lot_id: LotId,
    pub remaining: Quantity,
}

/// Mutable state for one item within one command. Failed allocations leave it unchanged.
#[derive(Debug)]
pub struct ItemBalance {
    lots: Vec<LotBalance>,
    unallocated: Quantity,
    book: Quantity,
}

const STOCK_LIMIT_MILLI: i128 = 1_000_000_000_000;

fn stock_quantity(value: i128, lot_id: Option<&LotId>) -> Result<Quantity, DomainError> {
    let exceeded = || DomainError::StockLimitExceeded {
        lot_id: lot_id.cloned(),
    };
    if !(-STOCK_LIMIT_MILLI..=STOCK_LIMIT_MILLI).contains(&value) {
        return Err(exceeded());
    }
    i64::try_from(value)
        .map(Quantity::from_milli)
        .map_err(|_| exceeded())
}

fn adjust_stock(
    current: Quantity,
    qty: Quantity,
    operation: fn(i128, i128) -> Option<i128>,
    lot_id: Option<&LotId>,
) -> Result<Quantity, DomainError> {
    let total =
        operation(i128::from(current.milli()), i128::from(qty.milli())).ok_or_else(|| {
            DomainError::StockLimitExceeded {
                lot_id: lot_id.cloned(),
            }
        })?;
    stock_quantity(total, lot_id)
}

impl ItemBalance {
    pub fn new(lots: Vec<LotBalance>, unallocated: Quantity) -> Result<Self, DomainError> {
        // Check existing lots first so a lot violation retains its identity.
        for lot in &lots {
            stock_quantity(i128::from(lot.remaining.milli()), Some(&lot.lot_id))?;
        }
        stock_quantity(i128::from(unallocated.milli()), None)?;
        // Intermediate sums may exceed the stock limit or i64 before cancellation.
        let total = lots
            .iter()
            .try_fold(i128::from(unallocated.milli()), |total, lot| {
                total
                    .checked_add(i128::from(lot.remaining.milli()))
                    .ok_or(DomainError::StockLimitExceeded { lot_id: None })
            })?;
        let book = stock_quantity(total, None)?;
        Ok(Self {
            lots,
            unallocated,
            book,
        })
    }

    pub fn lots(&self) -> &[LotBalance] {
        &self.lots
    }

    pub fn book(&self) -> Quantity {
        self.book
    }

    /// Add a newly created lot and retain FIFO order for subsequent lines.
    pub fn receive(&mut self, lot_id: LotId, qty: Quantity) -> Result<(), DomainError> {
        if qty <= Quantity::ZERO {
            return Err(DomainError::InvalidField("qty"));
        }
        stock_quantity(i128::from(qty.milli()), None)?;
        let book = adjust_stock(self.book, qty, i128::checked_add, None)?;
        let index = self.lots.partition_point(|lot| {
            (lot.lot_id.date(), lot.lot_id.serial()) <= (lot_id.date(), lot_id.serial())
        });
        self.lots.insert(
            index,
            LotBalance {
                lot_id,
                remaining: qty,
            },
        );
        self.book = book;
        Ok(())
    }

    /// `specified` is the index of a previously validated lot in `lots()`.
    pub fn allocate(
        &mut self,
        specified: Option<usize>,
        qty: Quantity,
    ) -> Result<Vec<Allocation>, DomainError> {
        if qty <= Quantity::ZERO {
            return Err(DomainError::InvalidField("qty"));
        }
        if let Some(index) = specified {
            let lot = self
                .lots
                .get(index)
                .ok_or(DomainError::InvalidField("lot_id"))?;
            let remaining = adjust_stock(lot.remaining, qty, i128::checked_sub, Some(&lot.lot_id))?;
            let book = self.book_after_deduction(qty)?;
            let lot_id = lot.lot_id.clone();
            self.lots[index].remaining = remaining;
            self.book = book;
            return Ok(vec![Allocation {
                lot_id: Some(lot_id),
                qty,
                source: AllocationSource::Specified,
            }]);
        }
        // FIFO only reduces positive lots toward zero, so only the net book and
        // unallocated shortfall can cross a limit in this branch.
        let book = self.book_after_deduction(qty)?;
        let mut remaining = qty;
        let mut alloc = Vec::new();
        let mut updates = Vec::new();
        for lot in &mut self.lots {
            if remaining == Quantity::ZERO {
                break;
            }
            if lot.remaining <= Quantity::ZERO {
                continue;
            }
            let qty = remaining.min(lot.remaining);
            let lot_remaining = lot
                .remaining
                .checked_sub(qty)
                .ok_or(DomainError::InvalidField("qty"))?;
            remaining = remaining
                .checked_sub(qty)
                .ok_or(DomainError::InvalidField("qty"))?;
            alloc.push(Allocation {
                lot_id: Some(lot.lot_id.clone()),
                qty,
                source: AllocationSource::Fifo,
            });
            updates.push((lot, lot_remaining));
        }
        let mut unallocated = self.unallocated;
        if remaining > Quantity::ZERO {
            unallocated = adjust_stock(self.unallocated, remaining, i128::checked_sub, None)?;
            alloc.push(Allocation {
                lot_id: None,
                qty: remaining,
                source: AllocationSource::Shortfall,
            });
        }
        for (lot, remaining) in updates {
            lot.remaining = remaining;
        }
        self.unallocated = unallocated;
        self.book = book;
        Ok(alloc)
    }

    fn book_after_deduction(&self, qty: Quantity) -> Result<Quantity, DomainError> {
        adjust_stock(self.book, qty, i128::checked_sub, None)
    }
}

/// Increment the existing maximum serial for one item and one lot date.
pub fn next_lot_serial(last: i64) -> Result<i64, DomainError> {
    last.checked_add(1)
        .filter(|serial| *serial <= 999)
        .ok_or(DomainError::InvalidField("lot_serial"))
}
