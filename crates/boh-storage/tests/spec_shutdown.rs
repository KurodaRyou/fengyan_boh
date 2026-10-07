//! 锁定测试：关闭时 WAL 截断因读事务未结束而失败，只记警告、正常返回，已提交的数据不丢。
//! 规则见 AGENTS.md「Rust」优雅关闭顺序，接口见 docs/interfaces.md（`WriterHandle::shutdown`）。

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use boh_storage::rusqlite::params;
use boh_storage::{StorageError, open};

const STORE: &str = "01890a5d-ac96-774b-bcce-b302099a8050";
const CREATED_AT: i64 = 1_791_248_400_000;

// 「CheckpointBusy 只记 warn，正常退出」：外部连接持有读事务，wal_checkpoint(TRUNCATE) 等满 busy_timeout（5 秒）
// 仍无法截断 WAL；shutdown 返回 Ok。读事务结束后重新 open，已提交的行仍在。
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
async fn checkpoint_busy_at_shutdown_still_succeeds_and_keeps_committed_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("boh.db");
    let storage = open(&path, NonZeroUsize::MIN).unwrap();
    storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute(
                "INSERT INTO store_meta (id, store_id, created_at) VALUES (1, ?1, ?2)",
                params![STORE, CREATED_AT],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let reader = boh_storage::testing::open_reader(&path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let rows: i64 = reader
        .query_row("SELECT count(*) FROM store_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);

    let started = Instant::now();
    storage.writer_handle.shutdown().await.unwrap();
    // 返回 Ok 是因为等满了 busy_timeout 仍然 busy，而不是根本没有执行 checkpoint。
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );

    reader.execute_batch("COMMIT").unwrap();
    drop(reader);
    drop(storage.readers);
    drop(storage.writer);

    let reopened = open(&path, NonZeroUsize::MIN).unwrap();
    let stored: (String, i64) = reopened
        .readers
        .call(|conn| -> Result<_, StorageError> {
            Ok(
                conn.query_row("SELECT store_id, created_at FROM store_meta", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(stored, (STORE.to_owned(), CREATED_AT));
    reopened.writer_handle.shutdown().await.unwrap();
}
