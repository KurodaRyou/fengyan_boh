use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::StorageError;

const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// 打开唯一的写连接。整个进程只调用一次，返回的连接交给写线程。
pub(crate) fn open_writer(path: &Path) -> Result<Connection, StorageError> {
    // 连接工厂是允许打开 SQLite 连接的边界，并统一应用必需的 PRAGMA。
    #[allow(clippy::disallowed_methods)]
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| StorageError::sqlite("打开写连接", error))?;
    apply_pragmas(&conn)?;
    Ok(conn)
}

/// 打开一个只读连接（`query_only = ON`）。必须在写连接完成迁移之后调用。
pub(crate) fn open_reader(path: &Path) -> Result<Connection, StorageError> {
    // 读连接也必须从此处打开，以统一应用必需的 PRAGMA 和 query_only。
    #[allow(clippy::disallowed_methods)]
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| StorageError::sqlite("打开读连接", error))?;
    apply_pragmas(&conn)?;
    conn.pragma_update(None, "query_only", true)
        .map_err(|error| StorageError::sqlite("启用只读查询", error))?;
    Ok(conn)
}

/// 把 WAL 内容写回主库并截断 WAL 文件。只在关闭前对写连接调用。
pub(crate) fn checkpoint_truncate(conn: &Connection) -> Result<(), StorageError> {
    let busy: i64 = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .map_err(|error| StorageError::sqlite("截断 WAL", error))?;
    if busy != 0 {
        return Err(StorageError::CheckpointBusy);
    }
    Ok(())
}

pub(crate) fn apply_pragmas(conn: &Connection) -> Result<(), StorageError> {
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| StorageError::sqlite("设置忙等待时间", error))?;
    let mode: String = conn
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .map_err(|error| StorageError::sqlite("启用 WAL", error))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StorageError::WalNotEnabled(mode));
    }
    // FULL 而不是 NORMAL：NORMAL 在断电时可能丢失已经回复客户端成功的事务。
    conn.pragma_update(None, "synchronous", "FULL")
        .map_err(|error| StorageError::sqlite("设置 FULL 持久化", error))?;
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(|error| StorageError::sqlite("启用外键约束", error))?;
    // REPLACE 隐式删除旧行时也要触发只追加表的 DELETE 防护。
    conn.pragma_update(None, "recursive_triggers", true)
        .map_err(|error| StorageError::sqlite("启用递归触发器", error))?;
    Ok(())
}
