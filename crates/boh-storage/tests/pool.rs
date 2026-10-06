//! 写入线程与读连接池的行为测试。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use boh_storage::rusqlite::{Connection, params};
use boh_storage::{
    Readers, StorageError, Writer, WriterHandle, checkpoint_truncate, migrate, open_writer,
    spawn_writer,
};
use tempfile::TempDir;

const CMD: &str = "01890a5d-ac96-774b-bcce-b302099a8057";

fn setup() -> (TempDir, Writer, WriterHandle, Readers) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("boh.db");
    let mut conn = open_writer(&path).unwrap();
    migrate(&mut conn).unwrap();
    let readers = Readers::open(&path, 2).unwrap();
    let (writer, handle) = spawn_writer(conn).unwrap();
    (dir, writer, handle, readers)
}

fn insert_command(conn: &Connection, command_id: &str) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO processed_commands (command_id, command_type, request, response, recorded_at)
         VALUES (?1, 'test', '{}', '{}', 0)",
        params![command_id],
    )?;
    Ok(())
}

async fn count_commands(readers: &Readers) -> i64 {
    readers
        .call(|conn| -> Result<i64, StorageError> {
            Ok(conn.query_row("SELECT COUNT(*) FROM processed_commands", [], |r| r.get(0))?)
        })
        .await
        .unwrap()
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
    let (_dir, writer, handle, readers) = setup();

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

    assert_eq!(count_commands(&readers).await, 50);

    let conn = handle.shutdown().await.unwrap();
    checkpoint_truncate(&conn).unwrap();
}

#[tokio::test]
async fn error_in_closure_rolls_back() {
    let (_dir, writer, _handle, readers) = setup();

    let result = writer
        .call(|tx| -> Result<(), TestError> {
            insert_command(tx, CMD)?;
            Err(TestError::Rejected)
        })
        .await;

    assert!(matches!(result, Err(TestError::Rejected)));
    assert_eq!(count_commands(&readers).await, 0);
}

#[tokio::test]
async fn calls_after_shutdown_fail_with_writer_closed() {
    let (_dir, writer, handle, _readers) = setup();
    handle.shutdown().await.unwrap();

    let result = writer.call(|tx| insert_command(tx, CMD)).await;
    assert!(matches!(result, Err(StorageError::WriterClosed)));
}

#[tokio::test]
async fn readers_cannot_write() {
    let (_dir, _writer, _handle, readers) = setup();
    let result = readers.call(|conn| insert_command(conn, CMD)).await;
    assert!(matches!(result, Err(StorageError::Sqlite(_))));
    assert_eq!(count_commands(&readers).await, 0);
}
