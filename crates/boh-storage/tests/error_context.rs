//! Implementation diagnostics, independent of the frozen acceptance suite.
use std::error::Error;
use std::num::{NonZeroU32, NonZeroUsize};

use boh_domain::{StoreId, UnixMillis};
use boh_storage::backup::Backup;
use boh_storage::clock::ManualClock;
use boh_storage::{BackupStage, Diagnostic, StorageError};

const ID: &str = "01890a5d-ac96-774b-bcce-b302099a8057";

#[tokio::test]
async fn sqlite_operations_keep_distinct_context_and_original_cause() {
    let dir = tempfile::tempdir().unwrap();
    let storage = boh_storage::open(&dir.path().join("boh.db"), NonZeroUsize::MIN).unwrap();
    for operation in ["查询待收货批次", "查询报损批次"] {
        let error = storage
            .readers
            .call(move |conn| -> Result<i64, StorageError> {
                conn.query_row("SELECT seq FROM store_events WHERE seq = -1", [], |row| {
                    row.get(0)
                })
                .map_err(|error| StorageError::sqlite(operation, error))
            })
            .await
            .unwrap_err();
        assert!(
            matches!(&error, StorageError::Sqlite { operation: actual, location, .. }
            if *actual == operation && location.file().ends_with("error_context.rs") && location.line() > 0)
        );
        assert!(error.to_string().contains(operation));
        assert!(matches!(
            error
                .source()
                .unwrap()
                .downcast_ref::<boh_storage::rusqlite::Error>(),
            Some(boh_storage::rusqlite::Error::QueryReturnedNoRows)
        ));
        assert!(Diagnostic(&error).to_string().contains("caused by"));
    }
    storage.writer_handle.shutdown().await.unwrap();
}

async fn append_invalid(
    storage: &boh_storage::Storage,
    device: &'static str,
    event_type: &'static str,
    seq: Option<i64>,
) -> Result<(), StorageError> {
    storage.writer.call(move |tx| -> Result<(), StorageError> {
        tx.execute("INSERT INTO processed_commands (command_id, command_type, request, response, recorded_at)
            VALUES (?1, 'test', '{}', '{}', 0)", [ID])
            .map_err(|error| StorageError::sqlite("保存测试命令", error))?;
        if seq.is_some() {
            tx.execute_batch("DROP TRIGGER store_events_seq_contiguous")
                .map_err(|error| StorageError::sqlite("注入账本空洞", error))?;
        }
        tx.execute("INSERT INTO store_events (seq, id, event_type, schema_version, aggregate_type,
            aggregate_id, aggregate_version, command_id, actor_id, device_id, business_date,
            occurred_at, recorded_at, payload)
            VALUES (?1, ?2, ?3, 3, 'EQUIPMENT', ?2, 1, ?2, ?2, ?4, '2026-10-08', 0, 0, '{}')",
            boh_storage::rusqlite::params![seq, ID, event_type, device])
            .map_err(|error| StorageError::sqlite("插入测试事件", error))?;
        Ok(())
    }).await
}

#[tokio::test]
async fn rebuild_decode_and_apply_failures_identify_the_event() {
    for device in ["invalid-device", ID] {
        let dir = tempfile::tempdir().unwrap();
        let storage = boh_storage::open(&dir.path().join("boh.db"), NonZeroUsize::MIN).unwrap();
        append_invalid(&storage, device, "FUTURE_EVENT", None)
            .await
            .unwrap();
        let error = storage.writer.rebuild_projections().await.unwrap_err();
        assert!(
            matches!(&error, StorageError::Rebuild { seq: 1, event_type, schema_version: 3, .. }
            if event_type == "FUTURE_EVENT")
        );
        let cause = error.source().unwrap();
        if device == ID {
            assert!(matches!(
                cause.downcast_ref::<StorageError>(),
                Some(StorageError::InvalidEvent(_))
            ));
        } else {
            assert!(matches!(
                cause.downcast_ref::<StorageError>(),
                Some(StorageError::Sqlite { .. })
            ));
            assert!(Diagnostic(&error).to_string().contains("invalid UUIDv7"));
        }
        storage.writer_handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn snapshot_validation_and_number_overflow_report_their_stage() {
    for gap in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("boh.db");
        let storage = boh_storage::open(&db, NonZeroUsize::MIN).unwrap();
        let directory = dir.path().join("backups");
        let store = StoreId::parse(ID).unwrap();
        let backup = Backup::prepare(&db, &directory, store, NonZeroU32::MIN).unwrap();
        if gap {
            append_invalid(&storage, ID, "FUTURE_EVENT", Some(2))
                .await
                .unwrap();
        } else {
            std::fs::write(
                directory.join(format!("boh-{store}-20261008T000000Z-{}-{ID}.db", u64::MAX)),
                [],
            )
            .unwrap();
        }
        let error = backup
            .run(&ManualClock::new(UnixMillis(1_791_261_000_000)).clock())
            .unwrap_err();
        let expected = if gap {
            BackupStage::Verify
        } else {
            BackupStage::AllocateNumber
        };
        assert!(
            matches!(&error, StorageError::Backup { stage, number: None, .. } if *stage == expected)
        );
        assert!(error.source().is_some());
        // Failed publication removes temporary files and preserves existing final files.
        assert!(std::fs::read_dir(&directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("tmp-")
        }));
        storage.writer_handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn join_error_keeps_the_cancelled_task_as_its_cause() {
    let task = tokio::spawn(std::future::pending::<()>());
    task.abort();
    let error = StorageError::join("等待查询任务", task.await.unwrap_err());
    assert!(
        error
            .source()
            .unwrap()
            .downcast_ref::<tokio::task::JoinError>()
            .unwrap()
            .is_cancelled()
    );
    assert!(error.location().is_some());
}
