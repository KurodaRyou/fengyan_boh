use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::Connection;
use tokio::sync::Semaphore;

use crate::{StorageError, open_reader};

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
    pub fn open(path: &Path, size: usize) -> Result<Self, StorageError> {
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

    /// 借出一个只读连接执行 `f`。
    pub async fn call<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<StorageError> + Send + 'static,
    {
        let permit = Arc::clone(&self.inner.permits)
            .acquire_owned()
            .await
            .map_err(|_| E::from(StorageError::ReadersClosed))?;
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || -> Result<T, E> {
            let _permit = permit;
            let conn = lock(&inner.idle).pop().ok_or(StorageError::ReadersClosed)?;
            let result = f(&conn);
            // release 构建为 panic = "abort"，f 不会 unwind，因此无需在 panic 时归还连接。
            lock(&inner.idle).push(conn);
            result
        })
        .await
        .map_err(|e| E::from(StorageError::Join(e.to_string())))?
    }
}

fn lock(idle: &Mutex<Vec<Connection>>) -> MutexGuard<'_, Vec<Connection>> {
    idle.lock().unwrap_or_else(PoisonError::into_inner)
}
