//! Submission and precheck share the same sequential, in-memory calculation.

use axum::http::StatusCode;
use boh_domain::inventory::{Allocation, stock_shortfall};
use boh_domain::lot::LotId;
use boh_domain::quantity::Quantity;
use boh_domain::time::{CaptureTimes, business_date, check_capture_times};
use boh_domain::waste::{
    LogWaste, PrecheckWaste, WasteEffect, WasteLine, WasteLogged, WasteRecord, WasteRequestLine,
};
use boh_domain::{AggregateId, EventId};
use boh_storage::StorageError;
use boh_storage::ledger::{self, Command, Event};
use boh_storage::rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::actor::Actor;
use crate::confirmation::{self, Confirmation};
use crate::http::{ApiError, ok};

#[tracing::instrument(name = "command", skip_all, fields(command_type = "waste.log", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn log(state: AppState, actor: Actor, command: LogWaste) -> Result<Value, ApiError> {
    let request = super::normalized(&command, None, "规范化报损命令")?;
    let writer = state.writer.clone();
    let response = writer
        .call(move |tx| {
            let recorded_at = state.clock.now();
            ledger::execute(
                tx,
                Command {
                    id: command.command_id,
                    command_type: "waste.log",
                    request: &request,
                    recorded_at,
                },
                |ledger| {
                    super::check_node_clock(&state, recorded_at)?;
                    check_capture_times(
                        CaptureTimes {
                            occurred_at: command.occurred_at,
                            clock_synced_at: command.clock_synced_at,
                            started_at: None,
                        },
                        recorded_at,
                    )?;
                    let date = business_date(
                        command.occurred_at,
                        &state.timezone,
                        state.business_day_cutoff,
                    )?;
                    let requests: Vec<_> = command
                        .lines
                        .iter()
                        .map(|line| line.request_line())
                        .collect();
                    let calculated = calculate(tx, &requests)?;
                    let pending = calculated
                        .iter()
                        .zip(&command.lines)
                        .filter_map(|(line, command)| {
                            line.confirmation().filter(|item| {
                                !confirmation::acknowledged(item, &command.acknowledged)
                            })
                        })
                        .collect();
                    confirmation::require(pending)?;
                    let waste_record_id =
                        AggregateId::from_parts(recorded_at, ledger::entropy(tx)?)
                            .map_err(StorageError::from)?;
                    let logged = WasteLogged {
                        lines: calculated.into_iter().map(|line| line.logged).collect(),
                    };
                    let payload = serde_json::to_string(&logged)
                        .map_err(|error| StorageError::external("序列化报损事件", error))?;
                    let row = WasteRecord {
                        waste_record_id,
                        lines: logged.lines,
                        business_date: date.to_string(),
                        occurred_at: command.occurred_at,
                        recorded_at,
                        actor_id: actor.employee_id,
                        device_id: actor.device_id,
                    };
                    ledger.append(&Event {
                        id: EventId::from_parts(recorded_at, ledger::entropy(tx)?)
                            .map_err(StorageError::from)?,
                        event_type: "WASTE_LOGGED".into(),
                        schema_version: 1,
                        aggregate_type: "WASTE_RECORD".into(),
                        aggregate_id: waste_record_id,
                        aggregate_version: 1,
                        command_id: command.command_id,
                        actor_id: actor.employee_id,
                        device_id: actor.device_id,
                        business_date: row.business_date.clone(),
                        occurred_at: row.occurred_at,
                        recorded_at,
                        clock_synced_at: Some(command.clock_synced_at),
                        payload,
                    })?;
                    let response = ok(json!({ "waste_record": row })).0;
                    serde_json::to_string(&response).map_err(|error| {
                        ApiError::from(StorageError::external("保存报损命令响应", error))
                    })
                },
            )
        })
        .await?;
    super::parse_response(&response)
}

#[tracing::instrument(name = "query", skip_all, fields(command_type = "waste.precheck", actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn precheck(
    state: AppState,
    actor: Actor,
    request: PrecheckWaste,
) -> Result<Value, ApiError> {
    state
        .readers
        .call(move |conn| -> Result<_, ApiError> {
            let calculated = calculate(conn, &request.lines)?;
            let confirmations: Vec<_> = calculated
                .iter()
                .filter_map(CalculatedLine::confirmation)
                .collect();
            let lines: Vec<_> = calculated
                .into_iter()
                .map(|line| PrecheckedLine {
                    book: line.book,
                    alloc: line.alloc,
                })
                .collect();
            Ok(json!({ "lines": lines, "confirmations": confirmations }))
        })
        .await
}

#[derive(Debug, Clone, Serialize)]
struct LineBook {
    line: usize,
    item_id: AggregateId,
    #[serde(skip_serializing_if = "Option::is_none")]
    lot_id: Option<LotId>,
    qty: Quantity,
    item_book_qty: Quantity,
    #[serde(skip_serializing_if = "Option::is_none")]
    lot_book_qty: Option<Quantity>,
}

struct CalculatedLine {
    book: LineBook,
    needs_confirmation: bool,
    alloc: Vec<Allocation>,
    logged: WasteLine,
}

impl CalculatedLine {
    fn confirmation(&self) -> Option<Confirmation> {
        if !self.needs_confirmation {
            return None;
        }
        let book = &self.book;
        let mut details = json!({
            "item_id": book.item_id, "qty": book.qty, "item_book_qty": book.item_book_qty,
        });
        if let (Some(lot), Some(qty)) = (&book.lot_id, book.lot_book_qty) {
            details["lot_id"] = json!(lot);
            details["lot_book_qty"] = json!(qty);
        }
        Some(Confirmation {
            line: book.line,
            code: "STOCK_SHORTFALL",
            details,
        })
    }
}

#[derive(Serialize)]
struct PrecheckedLine {
    #[serde(flatten)]
    book: LineBook,
    alloc: Vec<Allocation>,
}

fn calculate(
    conn: &Connection,
    requests: &[WasteRequestLine],
) -> Result<Vec<CalculatedLine>, ApiError> {
    let mut balances = super::inventory::WorkingSet::default();
    let mut result = Vec::with_capacity(requests.len());
    for (index, line) in requests.iter().enumerate() {
        validate_references(conn, index, line)?;
        let balance = balances.get(conn, line.item_id)?;
        if balance.lots().is_empty() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "ITEM_HAS_NO_LOTS",
                "item has never had a lot",
            )
            .with_details(json!({ "line": index, "item_id": line.item_id })));
        }
        let qty = line.input.base_qty().map_err(|_| ApiError::validation())?;
        let lot_index = line
            .lot_id
            .as_ref()
            .map(|lot_id| {
                balance
                    .lots()
                    .iter()
                    .position(|lot| &lot.lot_id == lot_id)
                    .ok_or_else(|| ApiError::internal_message("validated waste lot missing"))
            })
            .transpose()?;
        let lot_book_qty = lot_index.map(|index| balance.lots()[index].remaining);
        let book = LineBook {
            line: index,
            item_id: line.item_id,
            lot_id: line.lot_id.clone(),
            qty,
            item_book_qty: balance.book(),
            lot_book_qty,
        };
        let needs_confirmation = stock_shortfall(qty, balance.book(), lot_book_qty);
        let alloc = super::inventory::allocate(balance, lot_index, qty, index, line.item_id)?;
        let logged = WasteLine {
            item_id: line.item_id,
            lot_id: line.lot_id.clone(),
            qty,
            input: line.input.clone(),
            reason_code: line.reason_code.clone(),
            item_book_qty: book.item_book_qty,
            lot_book_qty,
            effect: WasteEffect::Alloc(alloc.clone()),
        };
        result.push(CalculatedLine {
            book,
            needs_confirmation,
            alloc,
            logged,
        });
    }
    Ok(result)
}

fn validate_references(
    conn: &Connection,
    index: usize,
    line: &WasteRequestLine,
) -> Result<(), ApiError> {
    super::item_units::validate_item(conn, Some(index), line.item_id, &line.input)?;
    let reason_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM waste_reasons WHERE code = ?1)",
            [&line.reason_code],
            |row| row.get(0),
        )
        .map_err(|error| StorageError::sqlite("查询报损原因", error))?;
    if !reason_exists {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "REFERENCE_NOT_FOUND",
            "waste reason not found",
        )
        .with_details(json!({ "entity": "WASTE_REASON", "code": line.reason_code })));
    }
    if let Some(lot_id) = &line.lot_id {
        let owner: Option<String> = conn
            .query_row(
                "SELECT item_id FROM inventory_lots WHERE lot_id = ?1",
                [lot_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| StorageError::sqlite("查询指定报损批次", error))?;
        let owner = owner.ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "REFERENCE_NOT_FOUND",
                "lot not found",
            )
            .with_details(json!({ "entity": "LOT", "id": lot_id }))
        })?;
        if owner != line.item_id.to_string() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "LOT_ITEM_MISMATCH",
                "lot belongs to another item",
            )
            .with_details(json!({ "line": index, "item_id": line.item_id, "lot_id": lot_id })));
        }
    }
    Ok(())
}
