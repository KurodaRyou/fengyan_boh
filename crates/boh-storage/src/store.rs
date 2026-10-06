//! Store identity operations, always inside Writer::call.
use boh_domain::{StoreId, UnixMillis};
use rusqlite::{OptionalExtension, Transaction, params};

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
    if current(tx)?.is_some() {
        return Err(StorageError::StoreAlreadyInitialized);
    }
    tx.execute(
        "INSERT INTO store_meta (id, store_id, created_at) VALUES (1, ?1, ?2)",
        params![store_id.to_string(), created_at.0],
    )?;
    Ok(())
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

fn current(tx: &Transaction<'_>) -> Result<Option<String>, StorageError> {
    Ok(tx
        .query_row("SELECT store_id FROM store_meta WHERE id = 1", [], |r| {
            r.get(0)
        })
        .optional()?)
}
