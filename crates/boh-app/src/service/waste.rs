//! Submission and precheck share the same sequential, in-memory calculation.

use std::collections::BTreeMap;

use axum::http::StatusCode;
use boh_domain::lot::LotId;
use boh_domain::time::{CaptureTimes, business_date, calibrate};
use boh_domain::waste::{
    Allocation, AllocationSource, LogWaste, PrecheckWaste, WasteEffect, WasteLine, WasteLogged,
    WasteRecord, WasteRequestLine,
};
use boh_domain::{AggregateId, EventId};
use boh_storage::StorageError;
use boh_storage::ledger::{self, Command, Event};
use boh_storage::rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{Value, json};

use crate::AppState;
use crate::actor::Actor;
use crate::http::{ApiError, WarningBody, ok};

#[tracing::instrument(name = "command", skip_all, fields(command_type = "waste.log", command_id = %command.command_id, actor_id = %actor.employee_id, device_id = %actor.device_id))]
pub async fn log(state: AppState, actor: Actor, command: LogWaste) -> Result<Value, ApiError> {
    // WasteCommandLine omits false confirmation flags, making false and absent
    // identical in the saved normalized request. True remains business content.
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
                    let requests: Vec<_> = command
                        .lines
                        .iter()
                        .map(|line| line.request_line())
                        .collect();
                    let calculated = calculate(tx, &requests)?;
                    let shortages: Vec<_> = calculated
                        .iter()
                        .zip(&command.lines)
                        .filter(|(line, command)| {
                            line.needs_confirmation && !command.confirm_shortage
                        })
                        .map(|(line, _)| line.book.clone())
                        .collect();
                    if !shortages.is_empty() {
                        return Err(ApiError::new(
                            StatusCode::CONFLICT,
                            "WASTE_CONFIRMATION_REQUIRED",
                            "confirm actual waste quantities",
                        )
                        .with_details(json!({ "lines": shortages })));
                    }
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
                        occurred_at: calibrated.occurred_at,
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
                        payload,
                    })?;
                    let mut response = ok(json!({ "waste_record": row })).0;
                    if calibrated.capture_time_adjusted {
                        response.warnings.push(WarningBody {
                            code: "CAPTURE_TIME_ADJUSTED",
                            message: "capture time adjusted to the receiving time".into(),
                            details: Default::default(),
                        });
                    }
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
            let lines: Vec<_> = calculate(conn, &request.lines)?
                .into_iter()
                .map(|line| PrecheckedLine {
                    book: line.book,
                    needs_confirmation: line.needs_confirmation,
                    alloc: line.alloc,
                })
                .collect();
            Ok(json!({ "lines": lines }))
        })
        .await
}

#[derive(Debug, Clone, Serialize)]
struct LineBook {
    line: usize,
    item_id: AggregateId,
    #[serde(skip_serializing_if = "Option::is_none")]
    lot_id: Option<LotId>,
    qty: i64,
    item_book_qty: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    lot_book_qty: Option<i64>,
}

struct CalculatedLine {
    book: LineBook,
    needs_confirmation: bool,
    alloc: Vec<Allocation>,
    logged: WasteLine,
}

#[derive(Serialize)]
struct PrecheckedLine {
    #[serde(flatten)]
    book: LineBook,
    needs_confirmation: bool,
    alloc: Vec<Allocation>,
}

struct LotBalance {
    lot_id: LotId,
    remaining: i64,
}

struct ItemBalance {
    lots: Vec<LotBalance>,
    unallocated: i64,
    book: i64,
}

fn calculate(
    conn: &Connection,
    requests: &[WasteRequestLine],
) -> Result<Vec<CalculatedLine>, ApiError> {
    let mut balances = BTreeMap::new();
    let mut result = Vec::with_capacity(requests.len());
    for (index, line) in requests.iter().enumerate() {
        validate_references(conn, index, line)?;
        if let std::collections::btree_map::Entry::Vacant(entry) = balances.entry(line.item_id) {
            entry.insert(load_balance(conn, line.item_id)?);
        }
        let balance = balances
            .get_mut(&line.item_id)
            .ok_or_else(|| ApiError::internal_message("waste balance missing"))?;
        if balance.lots.is_empty() {
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
                    .lots
                    .iter()
                    .position(|lot| &lot.lot_id == lot_id)
                    .ok_or_else(|| ApiError::internal_message("validated waste lot missing"))
            })
            .transpose()?;
        let lot_book_qty = lot_index.map(|index| balance.lots[index].remaining);
        let book = LineBook {
            line: index,
            item_id: line.item_id,
            lot_id: line.lot_id.clone(),
            qty,
            item_book_qty: balance.book,
            lot_book_qty,
        };
        let needs_confirmation = qty > balance.book || lot_book_qty.is_some_and(|book| qty > book);
        // Check the net book after EVERY line, including the final line. SQL's
        // SUM and addition can overflow or become REAL even if each component fits.
        balance.book = balance
            .book
            .checked_sub(qty)
            .ok_or_else(ApiError::validation)?;
        let alloc = allocate(balance, lot_index, qty)?;
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

fn allocate(
    balance: &mut ItemBalance,
    specified: Option<usize>,
    qty: i64,
) -> Result<Vec<Allocation>, ApiError> {
    if let Some(index) = specified {
        let lot = &mut balance.lots[index];
        lot.remaining = lot
            .remaining
            .checked_sub(qty)
            .ok_or_else(ApiError::validation)?;
        return Ok(vec![Allocation {
            lot_id: Some(lot.lot_id.clone()),
            qty,
            source: AllocationSource::Specified,
        }]);
    }
    let mut remaining = qty;
    let mut alloc = Vec::new();
    for lot in &mut balance.lots {
        if remaining == 0 {
            break;
        }
        if lot.remaining <= 0 {
            continue;
        }
        let qty = remaining.min(lot.remaining);
        lot.remaining = lot
            .remaining
            .checked_sub(qty)
            .ok_or_else(ApiError::validation)?;
        remaining = remaining
            .checked_sub(qty)
            .ok_or_else(ApiError::validation)?;
        alloc.push(Allocation {
            lot_id: Some(lot.lot_id.clone()),
            qty,
            source: AllocationSource::Fifo,
        });
    }
    if remaining > 0 {
        balance.unallocated = balance
            .unallocated
            .checked_sub(remaining)
            .ok_or_else(ApiError::validation)?;
        alloc.push(Allocation {
            lot_id: None,
            qty: remaining,
            source: AllocationSource::Shortfall,
        });
    }
    Ok(alloc)
}

fn load_balance(conn: &Connection, item_id: AggregateId) -> Result<ItemBalance, ApiError> {
    let mut statement = conn.prepare(
        "SELECT lot_id, remaining_qty FROM inventory_lots WHERE item_id = ?1 ORDER BY lot_date, lot_serial",
    ).map_err(|error| StorageError::sqlite("准备查询报损批次", error))?;
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
                remaining: row.get(1)?,
            })
        })
        .map_err(|error| StorageError::sqlite("查询报损批次", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StorageError::sqlite("读取报损批次", error))?;
    let unallocated: i64 = conn
        .query_row(
            "SELECT qty FROM inventory_unallocated WHERE item_id = ?1",
            [item_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| StorageError::sqlite("查询报损账外缺口", error))?
        .unwrap_or(0);
    let total = lots
        .iter()
        .try_fold(i128::from(unallocated), |total, lot| {
            total
                .checked_add(i128::from(lot.remaining))
                .ok_or_else(ApiError::validation)
        })?;
    let book = i64::try_from(total).map_err(|_| ApiError::validation())?;
    Ok(ItemBalance {
        lots,
        unallocated,
        book,
    })
}

fn validate_references(
    conn: &Connection,
    index: usize,
    line: &WasteRequestLine,
) -> Result<(), ApiError> {
    super::item_units::validate_item(conn, index, line.item_id, &line.input)?;
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
