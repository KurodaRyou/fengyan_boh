//! The sole event insertion path, including idempotency and saved command results.

use boh_domain::{AggregateId, CommandId, EventId, UnixMillis};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::{StorageError, projections};

#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("idempotency conflict")]
    IdempotencyConflict(Vec<String>),
}

pub struct Command<'a> {
    pub id: CommandId,
    pub command_type: &'a str,
    pub request: &'a str,
    pub recorded_at: UnixMillis,
}

/// E is the application error; only successful results enter processed_commands.
pub fn execute<E, F>(tx: &Transaction<'_>, command: Command<'_>, run: F) -> Result<String, E>
where
    E: From<ExecuteError>,
    F: FnOnce(&Ledger<'_>) -> Result<String, E>,
{
    let saved: Option<(String, String, String)> = tx
        .query_row(
            "SELECT command_type, request, response FROM processed_commands WHERE command_id = ?1",
            [command.id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(StorageError::from)
        .map_err(ExecuteError::from)?;
    if let Some((kind, request, response)) = saved {
        if kind != command.command_type {
            return Err(ExecuteError::IdempotencyConflict(vec!["command_type".into()]).into());
        }
        if request != command.request {
            let fields =
                differing_fields(tx, &request, command.request).map_err(ExecuteError::from)?;
            return Err(ExecuteError::IdempotencyConflict(fields).into());
        }
        return Ok(response);
    }
    let response = run(&Ledger { tx })?;
    tx.execute(
        "INSERT INTO processed_commands (command_id, command_type, request, response, recorded_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            command.id.to_string(),
            command.command_type,
            command.request,
            response,
            command.recorded_at.0
        ],
    )
    .map_err(StorageError::from)
    .map_err(ExecuteError::from)?;
    Ok(response)
}

fn differing_fields(tx: &Transaction<'_>, a: &str, b: &str) -> Result<Vec<String>, StorageError> {
    let mut statement = tx.prepare(
        "WITH a AS (SELECT key, value, type FROM json_each(?1)),
              b AS (SELECT key, value, type FROM json_each(?2)),
              keys AS (SELECT key FROM a UNION SELECT key FROM b)
         SELECT keys.key FROM keys LEFT JOIN a USING(key) LEFT JOIN b USING(key)
         WHERE a.type IS NOT b.type OR a.value IS NOT b.value ORDER BY keys.key COLLATE BINARY",
    )?;
    Ok(statement
        .query_map(params![a, b], |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}

pub struct Event {
    pub id: EventId,
    pub event_type: String,
    pub schema_version: i64,
    pub aggregate_type: String,
    pub aggregate_id: AggregateId,
    pub aggregate_version: i64,
    pub command_id: CommandId,
    pub actor_id: AggregateId,
    pub device_id: AggregateId,
    pub business_date: String,
    pub occurred_at: UnixMillis,
    pub recorded_at: UnixMillis,
    pub payload: String,
}

pub struct Ledger<'a> {
    tx: &'a Transaction<'a>,
}

impl Ledger<'_> {
    pub fn append(&self, event: &Event) -> Result<(), StorageError> {
        self.tx.execute(
            "INSERT INTO store_events (id, event_type, schema_version, aggregate_type, aggregate_id,
                 aggregate_version, command_id, actor_id, device_id, business_date, occurred_at,
                 recorded_at, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![event.id.to_string(), event.event_type, event.schema_version, event.aggregate_type,
                event.aggregate_id.to_string(), event.aggregate_version, event.command_id.to_string(),
                event.actor_id.to_string(), event.device_id.to_string(), event.business_date,
                event.occurred_at.0, event.recorded_at.0, event.payload],
        )?;
        projections::apply(self.tx, event)
    }
}

/// SQLite supplies entropy; UUID layout is built by the pure domain function.
pub fn entropy(tx: &Transaction<'_>) -> Result<[u8; 10], StorageError> {
    let bytes: Vec<u8> = tx.query_row("SELECT randomblob(10)", [], |r| r.get(0))?;
    bytes
        .try_into()
        .map_err(|_| StorageError::InvalidEvent("invalid ID entropy length".into()))
}
