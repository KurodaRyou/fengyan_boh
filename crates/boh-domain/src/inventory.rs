//! Inventory query parameters and response rows.

use serde::{Deserialize, Serialize};

use crate::lot::LotId;
use crate::{AggregateId, UnixMillis};

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
