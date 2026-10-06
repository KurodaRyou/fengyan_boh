//! 锁定测试专用的原始连接入口；普通代码使用 [`crate::open`]。

use std::path::Path;

use rusqlite::Connection;

use crate::StorageError;

pub fn open_writer(path: &Path) -> Result<Connection, StorageError> {
    crate::connection::open_writer(path)
}

pub fn open_reader(path: &Path) -> Result<Connection, StorageError> {
    crate::connection::open_reader(path)
}

pub fn migrate(conn: &mut Connection) -> Result<(), StorageError> {
    crate::migrate::migrate(conn)
}

pub fn rebuild_projections(conn: &mut Connection) -> Result<u64, StorageError> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let count = crate::projections::rebuild(&tx)?;
    tx.commit()?;
    Ok(count)
}
