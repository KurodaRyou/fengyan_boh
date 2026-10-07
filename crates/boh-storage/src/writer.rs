use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rusqlite::{Connection, Transaction, TransactionBehavior};
use tokio::sync::{mpsc, oneshot};

use crate::StorageError;
use crate::connection::checkpoint_truncate;

/// 写队列容量。队列满时 `Writer::call` 会等待，形成背压。
const QUEUE_CAPACITY: usize = 256;

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

enum Msg {
    Run(Job),
    Shutdown,
}

/// 唯一写连接的句柄，可廉价克隆。所有写操作都必须通过它。
#[derive(Clone)]
pub struct Writer {
    tx: mpsc::Sender<Msg>,
}

/// 写入线程的所有权句柄，只用于关闭。
pub struct WriterHandle {
    tx: mpsc::Sender<Msg>,
    thread: JoinHandle<Result<(), StorageError>>,
}

/// 启动专用写入线程，由它独占 `conn`。
pub(crate) fn spawn_writer(conn: Connection) -> Result<(Writer, WriterHandle), StorageError> {
    let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
    let thread = std::thread::Builder::new()
        .name("sqlite-writer".into())
        .spawn(move || run(conn, rx))
        .map_err(StorageError::Spawn)?;
    Ok((Writer { tx: tx.clone() }, WriterHandle { tx, thread }))
}

fn run(mut conn: Connection, mut rx: mpsc::Receiver<Msg>) -> Result<(), StorageError> {
    while let Some(msg) = rx.blocking_recv() {
        match msg {
            Msg::Run(job) => {
                let started = Instant::now();
                job(&mut conn);
                let elapsed = started.elapsed();
                if elapsed > Duration::from_millis(50) {
                    tracing::warn!(elapsed_ms = %elapsed.as_millis(), "sqlite writer task exceeded 50ms");
                }
            }
            // 关闭入口后继续排空队列，包括标记后已经入队的任务。
            Msg::Shutdown => rx.close(),
        }
    }
    let checkpoint = checkpoint_truncate(&conn);
    let closed = conn.close().map_err(|(_, err)| StorageError::from(err));
    match checkpoint {
        Err(StorageError::CheckpointBusy) => {
            tracing::warn!("WAL checkpoint busy at shutdown; committed data remains in WAL")
        }
        result => result?,
    }
    closed
}

impl Writer {
    pub async fn rebuild_projections(&self) -> Result<u64, StorageError> {
        self.call(crate::projections::rebuild).await
    }

    /// 在写入线程上用 `BEGIN IMMEDIATE` 事务执行 `f`。
    /// `f` 返回 `Ok` 时提交，返回 `Err` 时回滚。
    ///
    /// `f` 运行在唯一的写线程上：禁止在其中做网络 I/O、sleep 或其他耗时操作。
    pub async fn call<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<StorageError> + Send + 'static,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        let job: Job = Box::new(move |conn| {
            // 调用方已放弃等待时，结果无处可送，丢弃即可（事务已经提交或回滚）。
            let _ = reply_tx.send(run_in_transaction(conn, f));
        });
        self.tx
            .send(Msg::Run(job))
            .await
            .map_err(|_| E::from(StorageError::WriterClosed))?;
        reply_rx
            .await
            .map_err(|_| E::from(StorageError::WriterClosed))?
    }
}

fn run_in_transaction<T, E, F>(conn: &mut Connection, f: F) -> Result<T, E>
where
    F: FnOnce(&Transaction<'_>) -> Result<T, E>,
    E: From<StorageError>,
{
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StorageError::from)?;
    let out = f(&tx)?;
    tx.commit().map_err(StorageError::from)?;
    Ok(out)
}

impl WriterHandle {
    /// 排空已入队任务，在写线程上截断 WAL 并关闭写连接，再等待线程退出。
    /// 之后再提交的任务会收到 [`StorageError::WriterClosed`]。
    pub async fn shutdown(self) -> Result<(), StorageError> {
        // 线程若已退出，send 会失败；结果以下面的 join 为准。
        let _ = self.tx.send(Msg::Shutdown).await;
        let thread = self.thread;
        tokio::task::spawn_blocking(move || thread.join())
            .await
            .map_err(|e| StorageError::Join(e.to_string()))?
            .map_err(|_| StorageError::WriterPanicked)?
    }
}
