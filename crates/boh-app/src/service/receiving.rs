//! Receipt decisions run once, after idempotency and before appending the event.

use std::collections::BTreeMap;

use axum::http::StatusCode;
use boh_domain::receiving::{CreateReceipt, GoodsReceived, Receipt, ReceiptLine, ReceivedLine};
use boh_domain::time::{CaptureTimes, business_date, calibrate, end_of_local_date, local_date};
use boh_domain::{AggregateId, EventId, UnixMillis};
use boh_storage::StorageError;
use boh_storage::ledger::{self, Command, Event};
use boh_storage::rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};

use super::missing_reference;
use crate::AppState;
use crate::actor::Actor;
use crate::http::{ApiError, WarningBody, ok};

pub async fn create(
    state: AppState,
    actor: Actor,
    command: CreateReceipt,
) -> Result<Value, ApiError> {
    let request = super::normalized(&command, None)?;
    let writer = state.writer.clone();
    let response = writer
        .call(move |tx| {
            let recorded_at = state.clock.now();
            ledger::execute(
                tx,
                Command {
                    id: command.command_id,
                    command_type: "receipt.create",
                    request: &request,
                    recorded_at,
                },
                |ledger| {
                    let calibrated = calibrate(
                        CaptureTimes {
                            captured_at: command.captured_at,
                            sent_at: command.sent_at,
                            started_captured_at: None,
                        },
                        recorded_at,
                    )?;
                    let date = business_date(
                        calibrated.occurred_at,
                        &state.timezone,
                        state.business_day_cutoff,
                    )?;
                    let calendar_date =
                        local_date(calibrated.occurred_at, &state.timezone)?.to_string();
                    let supplier_exists: bool = tx
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM suppliers WHERE id = ?1)",
                            [command.supplier_id.to_string()],
                            |row| row.get(0),
                        )
                        .map_err(StorageError::from)?;
                    if !supplier_exists {
                        return Err(missing_reference("SUPPLIER", command.supplier_id));
                    }
                    let receipt_id = AggregateId::from_parts(recorded_at, ledger::entropy(tx)?)
                        .map_err(StorageError::from)?;
                    let mut warnings = Vec::new();
                    if calibrated.capture_time_adjusted {
                        warnings.push(WarningBody {
                            code: "CAPTURE_TIME_ADJUSTED",
                            message: "capture time adjusted to the receiving time".into(),
                            details: Default::default(),
                        });
                    }
                    let mut earlier_expiries: BTreeMap<AggregateId, UnixMillis> = BTreeMap::new();
                    let mut lines = Vec::with_capacity(command.lines.len());
                    for (index, line) in command.lines.into_iter().enumerate() {
                        validate_unit(tx, index, &line)?;
                        // Both strings are canonical YYYY-MM-DD dates, so their
                        // ordering equals calendar ordering, including year 0000.
                        let reason = if line.produced_on > calendar_date {
                            Some("PRODUCED_IN_FUTURE")
                        } else if line.expires_on < calendar_date {
                            Some("EXPIRED_ON_RECEIPT")
                        } else {
                            None
                        };
                        if let Some(reason) = reason {
                            return Err(ApiError::new(
                                StatusCode::BAD_REQUEST,
                                "INVALID_LOT_DATES",
                                "lot dates contradict the receipt date",
                            )
                            .with_details(json!({
                                "line": index, "item_id": line.item_id, "reason": reason
                            })));
                        }
                        let expires_on = line
                            .expires_on
                            .parse()
                            .map_err(|_| ApiError::validation())?;
                        let expires_at = end_of_local_date(expires_on, &state.timezone)?;
                        let lot_id = AggregateId::from_parts(recorded_at, ledger::entropy(tx)?)
                            .map_err(StorageError::from)?;
                        let older_expires_later: bool = tx
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM inventory_lots
                                 WHERE item_id = ?1 AND remaining_qty > 0 AND expires_at > ?2)",
                                params![line.item_id.to_string(), expires_at.0],
                                |row| row.get(0),
                            )
                            .map_err(StorageError::from)?;
                        if older_expires_later
                            || earlier_expiries
                                .get(&line.item_id)
                                .is_some_and(|earlier| *earlier > expires_at)
                        {
                            warnings.push(WarningBody {
                                code: "EXPIRES_BEFORE_OLDER_STOCK",
                                message: "new lot expires before stock used first".into(),
                                details: [
                                    ("line".into(), json!(index)),
                                    ("item_id".into(), json!(line.item_id)),
                                    ("lot_id".into(), json!(lot_id)),
                                ]
                                .into_iter()
                                .collect(),
                            });
                        }
                        earlier_expiries
                            .entry(line.item_id)
                            .and_modify(|earlier| *earlier = (*earlier).max(expires_at))
                            .or_insert(expires_at);
                        lines.push(ReceivedLine {
                            item_id: line.item_id,
                            qty: line.input.base_qty().map_err(|_| ApiError::validation())?,
                            input: line.input,
                            lot_id,
                            manufacturer_lot_no: line.manufacturer_lot_no,
                            produced_on: line.produced_on,
                            expires_on: line.expires_on,
                            expires_at,
                            line_cost_cents: line.line_cost_cents,
                        });
                    }
                    let received = GoodsReceived {
                        supplier_id: command.supplier_id,
                        lines,
                    };
                    let payload = serde_json::to_string(&received).map_err(ApiError::internal)?;
                    let row = Receipt {
                        receipt_id,
                        supplier_id: command.supplier_id,
                        lines: received.lines,
                        business_date: date.to_string(),
                        occurred_at: calibrated.occurred_at,
                        recorded_at,
                        actor_id: actor.employee_id,
                        device_id: actor.device_id,
                    };
                    ledger.append(&Event {
                        id: EventId::from_parts(recorded_at, ledger::entropy(tx)?)
                            .map_err(StorageError::from)?,
                        event_type: "GOODS_RECEIVED".into(),
                        schema_version: 1,
                        aggregate_type: "RECEIPT".into(),
                        aggregate_id: receipt_id,
                        aggregate_version: 1,
                        command_id: command.command_id,
                        actor_id: actor.employee_id,
                        device_id: actor.device_id,
                        business_date: row.business_date.clone(),
                        occurred_at: row.occurred_at,
                        recorded_at,
                        payload,
                    })?;
                    let mut response = ok(json!({ "receipt": row })).0;
                    response.warnings = warnings;
                    serde_json::to_string(&response).map_err(ApiError::internal)
                },
            )
        })
        .await?;
    super::parse_response(&response)
}

fn validate_unit(tx: &Transaction<'_>, index: usize, line: &ReceiptLine) -> Result<(), ApiError> {
    let current: Option<(String, Option<i64>)> = tx
        .query_row(
            "SELECT items.base_unit, item_units.base_qty_per_unit FROM items
             LEFT JOIN item_units ON item_units.item_id = items.id AND item_units.unit_code = ?2
             WHERE items.id = ?1",
            params![line.item_id.to_string(), line.input.unit_code],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(StorageError::from)?;
    let (base_unit, factor) = current.ok_or_else(|| missing_reference("ITEM", line.item_id))?;
    let factor = if line.input.unit_code == base_unit {
        1
    } else {
        factor.ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "UNKNOWN_UNIT",
                "unit is not configured",
            )
            .with_details(json!({
                "line": index, "item_id": line.item_id, "unit_code": line.input.unit_code
            }))
        })?
    };
    if factor != line.input.base_qty_per_unit {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "UNIT_CONVERSION_CHANGED",
            "unit conversion changed",
        )
        .with_details(json!({
            "line": index, "item_id": line.item_id, "unit_code": line.input.unit_code,
            "base_qty_per_unit": factor
        })));
    }
    Ok(())
}
