//! 写入线程与读连接池的行为测试。

use std::num::NonZeroUsize;
use std::sync::mpsc;
use std::time::Duration;

use boh_storage::rusqlite::{Connection, params};
use boh_storage::{Readers, StorageError, Writer, WriterHandle};
use tempfile::TempDir;

const CMD: &str = "01890a5d-ac96-774b-bcce-b302099a8057";

fn setup() -> Result<(TempDir, Writer, WriterHandle, Readers), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("boh.db");
    let storage = boh_storage::open(&path, NonZeroUsize::new(2).ok_or("invalid pool size")?)?;
    Ok((dir, storage.writer, storage.writer_handle, storage.readers))
}

fn insert_command(conn: &Connection, command_id: &str) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO processed_commands (command_id, command_type, request, response, recorded_at)
         VALUES (?1, 'test', '{}', '{}', 0)",
        params![command_id],
    )
    .map_err(|error| StorageError::sqlite("插入幂等响应", error))?;
    Ok(())
}

async fn count_commands(readers: &Readers) -> Result<i64, StorageError> {
    readers
        .call(|conn| -> Result<i64, StorageError> {
            conn.query_row("SELECT COUNT(*) FROM processed_commands", [], |r| r.get(0))
                .map_err(|error| StorageError::sqlite("查询幂等响应", error))
        })
        .await
}

#[derive(Debug)]
#[allow(dead_code)] // 字段只用于 Debug 输出
enum TestError {
    Storage(StorageError),
    Rejected,
}

impl From<StorageError> for TestError {
    fn from(err: StorageError) -> Self {
        TestError::Storage(err)
    }
}

#[tokio::test]
async fn concurrent_writes_are_serialized_and_committed() {
    let (_dir, writer, handle, readers) = setup().unwrap();

    let tasks: Vec<_> = (0..50)
        .map(|i| {
            let writer = writer.clone();
            tokio::spawn(async move {
                writer
                    .call(move |tx| {
                        insert_command(tx, &format!("01890a5d-ac96-774b-bcce-{i:012x}"))
                    })
                    .await
            })
        })
        .collect();
    for task in tasks {
        task.await.unwrap().unwrap();
    }

    assert_eq!(count_commands(&readers).await.unwrap(), 50);

    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn error_in_closure_rolls_back() {
    let (_dir, writer, _handle, readers) = setup().unwrap();

    let result = writer
        .call(|tx| -> Result<(), TestError> {
            insert_command(tx, CMD)?;
            Err(TestError::Rejected)
        })
        .await;

    assert!(matches!(result, Err(TestError::Rejected)));
    assert_eq!(count_commands(&readers).await.unwrap(), 0);
}

#[tokio::test]
async fn calls_after_shutdown_fail_with_writer_closed() {
    let (_dir, writer, handle, _readers) = setup().unwrap();
    handle.shutdown().await.unwrap();

    let result = writer.call(|tx| insert_command(tx, CMD)).await;
    assert!(matches!(result, Err(StorageError::WriterClosed)));
}

#[tokio::test]
async fn readers_cannot_write() {
    let (_dir, _writer, _handle, readers) = setup().unwrap();
    let result = readers.call(|conn| insert_command(conn, CMD)).await;
    assert!(matches!(result, Err(StorageError::Sqlite { .. })));
    assert_eq!(count_commands(&readers).await.unwrap(), 0);
}

#[tokio::test]
async fn reader_keeps_one_snapshot_across_a_concurrent_commit() {
    let (_dir, writer, handle, readers) = setup().unwrap();
    let (first_read_tx, first_read_rx) = tokio::sync::oneshot::channel();
    let (committed_tx, committed_rx) = mpsc::channel();
    let reading = {
        let readers = readers.clone();
        tokio::spawn(async move {
            readers
                .call(move |conn| -> Result<(i64, i64), StorageError> {
                    assert!(!conn.is_autocommit());
                    let read = || {
                        conn.query_row("SELECT count(*) FROM processed_commands", [], |r| r.get(0))
                    };
                    let first =
                        read().map_err(|error| StorageError::sqlite("读取第一份快照", error))?;
                    first_read_tx.send(()).unwrap();
                    committed_rx.recv().unwrap();
                    let second =
                        read().map_err(|error| StorageError::sqlite("读取第二份快照", error))?;
                    Ok((first, second))
                })
                .await
        })
    };
    first_read_rx.await.unwrap();
    writer.call(|tx| insert_command(tx, CMD)).await.unwrap();
    committed_tx.send(()).unwrap();
    assert_eq!(reading.await.unwrap().unwrap(), (0, 0));
    assert_eq!(count_commands(&readers).await.unwrap(), 1);
    handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn reader_error_rolls_back_before_reusing_the_connection() {
    let dir = tempfile::tempdir().unwrap();
    let storage = boh_storage::open(&dir.path().join("boh.db"), NonZeroUsize::MIN).unwrap();
    let result = storage
        .readers
        .call(|conn| -> Result<(), TestError> {
            assert!(!conn.is_autocommit());
            conn.query_row("SELECT count(*) FROM processed_commands", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(|error| StorageError::sqlite("查询幂等响应", error))?;
            Err(TestError::Rejected)
        })
        .await;
    assert!(matches!(result, Err(TestError::Rejected)));
    storage
        .writer
        .call(|tx| insert_command(tx, CMD))
        .await
        .unwrap();
    // 只有一个连接：再次开始事务成功且看见新提交，证明上一事务已经结束。
    assert_eq!(count_commands(&storage.readers).await.unwrap(), 1);
    storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_truncates_wal_while_readers_remain_open() {
    let (dir, writer, handle, readers) = setup().unwrap();
    writer.call(|tx| insert_command(tx, CMD)).await.unwrap();
    let wal = dir.path().join("boh.db-wal");
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    handle.shutdown().await.unwrap();
    assert_eq!(std::fs::metadata(&wal).unwrap().len(), 0);
    assert_eq!(count_commands(&readers).await.unwrap(), 1);
}

#[tokio::test]
async fn shutdown_drains_jobs_already_in_the_queue() {
    let (_dir, writer, handle, readers) = setup().unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first = {
        let writer = writer.clone();
        tokio::spawn(async move {
            writer
                .call(move |tx| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    insert_command(tx, CMD)
                })
                .await
        })
    };
    entered_rx.await.unwrap();
    let mut queued =
        Box::pin(writer.call(|tx| insert_command(tx, "01890a5d-ac96-774b-bcce-b302099a8058")));
    // call 的第一次 poll 完成入队，然后等待回复。
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(queued.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    let mut stopping = Box::pin(handle.shutdown());
    std::future::poll_fn(|cx| {
        assert!(std::future::Future::poll(stopping.as_mut(), cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    release_tx.send(()).unwrap();
    first.await.unwrap().unwrap();
    queued.await.unwrap();
    stopping.await.unwrap();
    assert_eq!(count_commands(&readers).await.unwrap(), 2);
    assert!(matches!(
        writer.call(|tx| insert_command(tx, CMD)).await,
        Err(StorageError::WriterClosed)
    ));
}

#[tokio::test]
async fn dropping_all_writer_owners_exits_the_thread() {
    let (dir, writer, handle, readers) = setup().unwrap();
    writer.call(|tx| insert_command(tx, CMD)).await.unwrap();
    let wal = dir.path().join("boh.db-wal");
    assert!(std::fs::metadata(&wal).unwrap().len() > 0);
    let clone = writer.clone();
    drop(handle);
    drop(writer);
    clone
        .call(|tx| -> Result<(), StorageError> {
            tx.query_row("SELECT 1", [], |_| Ok(()))
                .map_err(|error| StorageError::sqlite("查询查询结果", error))?;
            Ok(())
        })
        .await
        .unwrap();
    drop(clone);
    tokio::time::timeout(Duration::from_secs(2), async {
        while std::fs::metadata(&wal).unwrap().len() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(count_commands(&readers).await.unwrap(), 1);
}
