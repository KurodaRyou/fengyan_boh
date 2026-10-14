//! Inventory queries and pure command-local stock rules.

use serde::{Deserialize, Serialize};

use crate::lot::LotId;
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
    pub remaining_qty: i64,
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
    pub on_hand_qty: i64,
    pub unallocated_qty: i64,
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
    pub qty: i64,
    pub source: AllocationSource,
}
object_serde!(Allocation);

/// A lot balance supplied in FIFO order by the decision-reading layer.
#[derive(Debug)]
pub struct LotBalance {
    pub lot_id: LotId,
    pub remaining: i64,
}

/// Mutable state for one item within one command. Failed allocations leave it unchanged.
#[derive(Debug)]
pub struct ItemBalance {
    lots: Vec<LotBalance>,
    unallocated: i64,
    book: i64,
}

impl ItemBalance {
    pub fn new(lots: Vec<LotBalance>, unallocated: i64) -> Result<Self, DomainError> {
        // Intermediate sums may exceed i64 even when the final book fits.
        let total = lots
            .iter()
            .try_fold(i128::from(unallocated), |total, lot| {
                total
                    .checked_add(i128::from(lot.remaining))
                    .ok_or(DomainError::InvalidField("qty"))
            })?;
        let book = i64::try_from(total).map_err(|_| DomainError::InvalidField("qty"))?;
        Ok(Self {
            lots,
            unallocated,
            book,
        })
    }

    pub fn lots(&self) -> &[LotBalance] {
        &self.lots
    }

    pub fn book(&self) -> i64 {
        self.book
    }

    /// `specified` is the index of a previously validated lot in `lots()`.
    pub fn allocate(
        &mut self,
        specified: Option<usize>,
        qty: i64,
    ) -> Result<Vec<Allocation>, DomainError> {
        // Check the net book after every line, including the final one.
        let book = self
            .book
            .checked_sub(qty)
            .ok_or(DomainError::InvalidField("qty"))?;
        if let Some(index) = specified {
            let lot = self
                .lots
                .get_mut(index)
                .ok_or(DomainError::InvalidField("lot_id"))?;
            let remaining = lot
                .remaining
                .checked_sub(qty)
                .ok_or(DomainError::InvalidField("qty"))?;
            lot.remaining = remaining;
            self.book = book;
            return Ok(vec![Allocation {
                lot_id: Some(lot.lot_id.clone()),
                qty,
                source: AllocationSource::Specified,
            }]);
        }
        let mut remaining = qty;
        let mut alloc = Vec::new();
        let mut updates = Vec::new();
        for lot in &mut self.lots {
            if remaining == 0 {
                break;
            }
            if lot.remaining <= 0 {
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
        if remaining > 0 {
            unallocated = self
                .unallocated
                .checked_sub(remaining)
                .ok_or(DomainError::InvalidField("qty"))?;
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
}

/// Increment the existing maximum serial for one item and one lot date.
pub fn next_lot_serial(last: i64) -> Result<i64, DomainError> {
    last.checked_add(1)
        .filter(|serial| *serial <= 999)
        .ok_or(DomainError::InvalidField("lot_serial"))
}
