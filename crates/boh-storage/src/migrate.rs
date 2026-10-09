use rusqlite::{Connection, TransactionBehavior};

use crate::StorageError;

/// 按顺序排列的迁移，下标 + 1 即 schema 版本（`PRAGMA user_version`）。
/// 已发布的迁移禁止修改，只能在末尾追加。
const MIGRATIONS: &[&str] = &[
    include_str!("../../../migrations/001_initial_schema.sql"),
    include_str!("../../../migrations/002_equipment.sql"),
    include_str!("../../../migrations/003_temperature_readings.sql"),
    include_str!("../../../migrations/004_store_events_recorded_at.sql"),
    include_str!("../../../migrations/005_master_data_inventory.sql"),
    include_str!("../../../migrations/006_inventory_lots_manufacturer_lot_no.sql"),
    include_str!("../../../migrations/007_lot_numbers.sql"),
];

pub const LATEST_SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// 把数据库升级到 [`LATEST_SCHEMA_VERSION`]。每个迁移在独立的事务中执行。
/// 数据库版本高于本程序支持的版本时返回错误，调用方必须拒绝启动。
pub(crate) fn migrate(conn: &mut Connection) -> Result<(), StorageError> {
    let current = schema_version(conn)?;
    if !(0..=LATEST_SCHEMA_VERSION).contains(&current) {
        return Err(StorageError::UnsupportedSchemaVersion {
            found: current,
            supported: LATEST_SCHEMA_VERSION,
        });
    }

    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = index as i64 + 1;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| StorageError::sqlite("开始迁移事务", error))?;
        tx.execute_batch(sql)
            .map_err(|error| StorageError::sqlite("执行数据库迁移", error))?;
        tx.pragma_update(None, "user_version", version)
            .map_err(|error| StorageError::sqlite("保存迁移版本", error))?;
        tx.commit()
            .map_err(|error| StorageError::sqlite("提交数据库迁移", error))?;
    }
    Ok(())
}

pub fn schema_version(conn: &Connection) -> Result<i64, StorageError> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| StorageError::sqlite("读取数据库版本", error))
}
