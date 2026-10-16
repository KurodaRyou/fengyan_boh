//! Store identity checks and initialization inside storage transactions.
use boh_domain::{StoreId, UnixMillis};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::StorageError;

pub fn verify(tx: &Transaction<'_>, store_id: StoreId) -> Result<(), StorageError> {
    match current(tx)? {
        None => Err(StorageError::StoreNotInitialized),
        Some(id) if id == store_id.to_string() => Ok(()),
        Some(_) => Err(StorageError::StoreMismatch),
    }
}

pub fn initialize(
    tx: &Transaction<'_>,
    store_id: StoreId,
    created_at: UnixMillis,
) -> Result<(), StorageError> {
    ensure_uninitialized(tx)?;
    tx.execute(
        "INSERT INTO store_meta (id, store_id, created_at) VALUES (1, ?1, ?2)",
        params![store_id.to_string(), created_at.0],
    )
    .map_err(|error| StorageError::sqlite("插入门店身份", error))?;
    Ok(())
}

pub fn ensure_uninitialized(conn: &Connection) -> Result<(), StorageError> {
    if current(conn)?.is_some() {
        Err(StorageError::StoreAlreadyInitialized)
    } else {
        Ok(())
    }
}

/// Fixed test nodes reuse their original creation timestamp.
pub fn initialize_if_empty(
    tx: &Transaction<'_>,
    store_id: StoreId,
    created_at: UnixMillis,
) -> Result<(), StorageError> {
    if current(tx)?.is_none() {
        initialize(tx, store_id, created_at)
    } else {
        verify(tx, store_id)
    }
}

fn current(conn: &Connection) -> Result<Option<String>, StorageError> {
    conn.query_row("SELECT store_id FROM store_meta WHERE id = 1", [], |r| {
        r.get(0)
    })
    .optional()
    .map_err(|error| StorageError::sqlite("查询门店身份", error))
}
