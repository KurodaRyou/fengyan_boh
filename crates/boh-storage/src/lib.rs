//! SQLite 存储层：连接初始化、迁移、单写线程、只读连接池。

mod connection;
mod error;
pub mod ledger;
mod master_data;
mod migrate;
mod projections;
mod readers;
pub mod store;
mod waste;
mod writer;

pub mod backup;
pub mod clock;
#[doc(hidden)]
pub mod testing;

use std::num::NonZeroUsize;
use std::path::Path;

pub use error::{BackupStage, Diagnostic, StorageError};
pub use migrate::{LATEST_SCHEMA_VERSION, schema_version};
pub use readers::Readers;
pub use writer::{Writer, WriterHandle};

pub use rusqlite;

pub struct Storage {
    pub writer: Writer,
    pub writer_handle: WriterHandle,
    pub readers: Readers,
}

/// 一个进程对同一数据库只调用一次；迁移和读池成功后才启动写线程。
pub fn open(path: &Path, reader_pool_size: NonZeroUsize) -> Result<Storage, StorageError> {
    let mut conn = connection::open_writer(path)?;
    migrate::migrate(&mut conn)?;
    let readers = Readers::open(path, reader_pool_size.get())?;
    let (writer, writer_handle) = writer::spawn_writer(conn)?;
    Ok(Storage {
        writer,
        writer_handle,
        readers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_rejects_newer_schema_without_migrating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boh.db");
        let conn = connection::open_writer(&path).unwrap();
        let future = LATEST_SCHEMA_VERSION + 1;
        conn.pragma_update(None, "user_version", future).unwrap();
        drop(conn);
        assert!(matches!(
            open(&path, NonZeroUsize::new(1).unwrap()),
            Err(StorageError::UnsupportedSchemaVersion { found, supported })
                if found == future && supported == LATEST_SCHEMA_VERSION
        ));
        let conn = connection::open_reader(&path).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), future);
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = 'table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }
}
