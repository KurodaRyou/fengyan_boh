use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, TransactionBehavior};
use tokio::sync::Semaphore;

use crate::StorageError;
use crate::connection::open_reader;

/// 固定大小的只读连接池。查询在 `spawn_blocking` 中执行，不阻塞异步运行时。
#[derive(Clone)]
pub struct Readers {
    inner: Arc<Inner>,
}

struct Inner {
    idle: Mutex<Vec<Connection>>,
    permits: Arc<Semaphore>,
}

impl Readers {
    /// 打开 `size` 个只读连接（`size` 必须 ≥ 1）。必须在迁移完成之后调用。
    pub(crate) fn open(path: &Path, size: usize) -> Result<Self, StorageError> {
        let conns = (0..size)
            .map(|_| open_reader(path))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            inner: Arc::new(Inner {
                idle: Mutex::new(conns),
                permits: Arc::new(Semaphore::new(size)),
            }),
        })
    }

    /// 借出一个只读连接，在 DEFERRED 事务的同一快照内执行 `f`。
    pub async fn call<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<StorageError> + Send + 'static,
    {
        let permit = Arc::clone(&self.inner.permits)
            .acquire_owned()
            .await
            .map_err(|_| E::from(StorageError::ReaderPoolInvariant("semaphore closed")))?;
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || -> Result<T, E> {
            let _permit = permit;
            let mut conn = lock(&inner.idle)
                .pop()
                .ok_or(StorageError::ReaderPoolInvariant(
                    "permit without connection",
                ))?;
            let result = (|| {
                let tx = conn
                    .transaction_with_behavior(TransactionBehavior::Deferred)
                    .map_err(StorageError::from)?;
                match f(&tx) {
                    Ok(out) => {
                        tx.commit().map_err(StorageError::from)?;
                        Ok(out)
                    }
                    Err(err) => {
                        tx.rollback().map_err(StorageError::from)?;
                        Err(err)
                    }
                }
            })();
            ensure_read_finished(&conn);
            // release 构建为 panic = "abort"，f 不会 unwind，因此无需在 panic 时归还连接。
            lock(&inner.idle).push(conn);
            result
        })
        .await
        .map_err(|e| E::from(StorageError::Join(e.to_string())))?
    }
}

fn ensure_read_finished(conn: &Connection) {
    if !conn.is_autocommit() {
        tracing::error!("reader transaction survived commit/rollback; aborting node");
        std::process::abort();
    }
}

fn lock(idle: &Mutex<Vec<Connection>>) -> MutexGuard<'_, Vec<Connection>> {
    idle.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroUsize;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn unfinished_transaction_aborts() {
        const CHILD_PATH: &str = "BOH_READER_ABORT_TEST_DB";
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let conn = crate::connection::open_reader(Path::new(&path)).unwrap();
            conn.execute_batch("BEGIN DEFERRED").unwrap();
            ensure_read_finished(&conn);
            panic!("unfinished transaction did not abort");
        }
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("boh.db");
        drop(crate::connection::open_writer(&db).unwrap());
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "readers::tests::unfinished_transaction_aborts",
                "--nocapture",
            ])
            .env(CHILD_PATH, &db)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert_eq!(output.status.signal(), Some(6), "{output:?}");
    }

    #[tokio::test]
    async fn query_and_cleanup_errors_with_no_open_transaction_do_not_abort() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::open(&dir.path().join("boh.db"), NonZeroUsize::MIN).unwrap();
        // Query failure is rolled back, and the same pooled connection remains usable.
        let result = storage
            .readers
            .call(|conn| -> Result<(), StorageError> {
                conn.execute_batch("SELECT * FROM nonexistent_table")?;
                Ok(())
            })
            .await;
        assert!(matches!(result, Err(StorageError::Sqlite(_))));
        // Force commit/rollback errors by ending the transaction inside the callback.
        // Autocommit has already been restored: these errors must not abort the node.
        for sql in ["COMMIT", "ROLLBACK"] {
            let result = storage
                .readers
                .call(move |conn| -> Result<(), StorageError> {
                    conn.execute_batch(sql)?;
                    if sql == "ROLLBACK" {
                        Err(StorageError::InvalidEvent("query failed".into()))
                    } else {
                        Ok(())
                    }
                })
                .await;
            assert!(matches!(result, Err(StorageError::Sqlite(_))));
            let value = storage
                .readers
                .call(|conn| -> Result<i64, StorageError> {
                    assert!(!conn.is_autocommit());
                    Ok(conn.query_row("SELECT 1", [], |r| r.get(0))?)
                })
                .await
                .unwrap();
            assert_eq!(value, 1);
        }
        storage.writer_handle.shutdown().await.unwrap();
    }
}
