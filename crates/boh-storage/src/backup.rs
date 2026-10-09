//! Read-only VACUUM snapshots, verification, durable publication and store-local retention.

use std::fs::{self, File};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use boh_domain::{BackupId, StoreId, UnixMillis};
use jiff::Timestamp;
use rusqlite::{Connection, OpenFlags};
use tokio::sync::Notify;

use crate::{BackupStage, Diagnostic, StorageError, clock::Clock, connection::apply_pragmas};

#[derive(Debug, Clone, Copy, Default)]
pub struct BackupStatus {
    pub last_ok_at: Option<UnixMillis>,
    pub last_seq: Option<i64>,
    pub last_failed_at: Option<UnixMillis>,
}

#[derive(Clone, Default)]
pub struct BackupHealth(Arc<Mutex<BackupStatus>>);

impl BackupHealth {
    pub fn snapshot(&self) -> BackupStatus {
        *lock(&self.0)
    }

    pub fn record(&self, result: &Result<i64, StorageError>, now: UnixMillis) {
        let mut status = lock(&self.0);
        match result {
            Ok(seq) => {
                status.last_ok_at = Some(now);
                status.last_seq = Some(*seq);
                status.last_failed_at = None;
            }
            Err(error) => {
                let (stage, number) = match error {
                    StorageError::Backup { stage, number, .. } => (Some(*stage), *number),
                    _ => (None, None),
                };
                tracing::error!(error = %Diagnostic(error), backup_stage = stage.map(tracing::field::debug), backup_number = number, "backup failed");
                status.last_failed_at = Some(now);
            }
        }
    }
}

#[derive(Clone)]
pub struct Backup {
    db_path: PathBuf,
    directory: PathBuf,
    store_id: StoreId,
    keep: NonZeroU32,
    gate: BackupGate,
}

impl Backup {
    pub fn prepare(
        db_path: &Path,
        directory: &Path,
        store_id: StoreId,
        keep: NonZeroU32,
    ) -> Result<Self, StorageError> {
        fs::create_dir_all(directory).map_err(|error| StorageError::io("创建备份目录", error))?;
        Ok(Self {
            db_path: db_path.to_owned(),
            directory: directory.to_owned(),
            store_id,
            keep,
            gate: BackupGate::default(),
        })
    }

    #[doc(hidden)]
    pub fn hold(&self) -> BackupHold {
        let mut state = lock(&self.gate.0.state);
        state.holds += 1;
        BackupHold(self.gate.clone())
    }

    /// Called only on spawn_blocking. The source never opens a write connection.
    pub fn run(&self, clock: &Clock) -> Result<i64, StorageError> {
        self.run_with_directory_sync(clock, sync_directory)
    }

    fn run_with_directory_sync(
        &self,
        clock: &Clock,
        sync: impl FnMut(&Path) -> Result<(), StorageError>,
    ) -> Result<i64, StorageError> {
        self.run_with_publication(clock, sync_file, rename_snapshot, sync)
    }

    fn run_with_publication(
        &self,
        clock: &Clock,
        mut file_sync: impl FnMut(&Path) -> Result<(), StorageError>,
        mut rename: impl FnMut(&Path, &Path) -> Result<(), StorageError>,
        mut sync: impl FnMut(&Path) -> Result<(), StorageError>,
    ) -> Result<i64, StorageError> {
        let started = clock.now();
        let source = open_source(&self.db_path)
            .map_err(|error| StorageError::backup(BackupStage::Generate, None, error))?;
        let entropy: [u8; 10] = source
            .query_row("SELECT randomblob(10)", [], |row| row.get(0))
            .map_err(|error| {
                StorageError::backup(
                    BackupStage::Generate,
                    None,
                    StorageError::sqlite("生成备份 ID 随机字节", error),
                )
            })?;
        let id = BackupId::from_parts(started, entropy)
            .map_err(|error| StorageError::backup(BackupStage::Generate, None, error.into()))?;
        let temporary = self
            .directory
            .join(format!("tmp-{}-{id}.db", self.store_id));
        let mut stage = BackupStage::Generate;
        let mut number = None;
        let result = (|| {
            let mut files = Vec::new();
            for name in self.names()? {
                if self.temporary_name(&name) {
                    remove_logged(&self.directory.join(name));
                } else if let Some(number) = final_number(&name, self.store_id) {
                    files.push((number, self.directory.join(name)));
                }
            }
            let before = max_seq(&source)?;
            self.gate.wait();
            let path = temporary
                .to_str()
                .ok_or_else(|| StorageError::Message("snapshot path is not UTF-8"))?;
            source.execute("VACUUM INTO ?1", [path]).map_err(|error| StorageError::sqlite("生成备份快照", error))?;
            stage = BackupStage::Verify;
            let n = verify(&temporary, before)?;
            stage = BackupStage::FileSync;
            file_sync(&temporary)?;
            stage = BackupStage::AllocateNumber;
            let allocated = files
                .iter()
                .map(|(n, _)| *n)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| StorageError::Message("backup number overflow"))?;
            number = Some(allocated);
            stage = BackupStage::Rename;
            let final_path = self.directory.join(format!(
                "boh-{}-{}-{allocated}-{id}.db",
                self.store_id,
                utc_label(started)
            ));
            rename(&temporary, &final_path)?;
            stage = BackupStage::DirectorySync;
            sync(&self.directory)?;
            // Cleanup starts only after the new file and its directory entry are durable.
            files.push((allocated, final_path.clone()));
            files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            for (_, path) in files.into_iter().skip(self.keep.get() as usize) {
                if path != final_path {
                    remove_logged(&path);
                }
            }
            if let Err(error) = sync(&self.directory) {
                tracing::warn!(error = %Diagnostic(&error), backup_stage = %BackupStage::DirectorySync, backup_number = allocated, "could not persist old backup cleanup; new backup is durable");
            }
            Ok(n)
        })().map_err(|error| StorageError::backup(stage, number, error));
        if result.is_err() && temporary.exists() {
            remove_logged(&temporary);
        }
        result
    }

    fn names(&self) -> Result<Vec<String>, StorageError> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.directory)
            .map_err(|error| StorageError::io("扫描备份目录", error))?
        {
            if let Some(name) = entry
                .map_err(|error| StorageError::io("读取备份目录项", error))?
                .file_name()
                .to_str()
            {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    fn temporary_name(&self, name: &str) -> bool {
        name.strip_prefix(&format!("tmp-{}-", self.store_id))
            .and_then(|s| s.strip_suffix(".db"))
            .is_some_and(canonical_v7)
    }
}

#[allow(clippy::disallowed_methods)] // The backup source is a dedicated read-only connection.
fn open_source(path: &Path) -> Result<Connection, StorageError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| StorageError::sqlite("打开备份源库", error))?;
    apply_pragmas(&conn)?;
    Ok(conn)
}

fn max_seq(conn: &Connection) -> Result<i64, StorageError> {
    conn.query_row("SELECT coalesce(max(seq), 0) FROM store_events", [], |r| {
        r.get(0)
    })
    .map_err(|error| StorageError::sqlite("查询事件账本", error))
}

#[allow(clippy::disallowed_methods)] // Verification preserves the snapshot's DELETE journal mode.
fn verify(path: &Path, before: i64) -> Result<i64, StorageError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| StorageError::sqlite("打开备份校验连接", error))?;
    let mut stmt = conn
        .prepare("PRAGMA integrity_check")
        .map_err(|error| StorageError::sqlite("准备快照完整性检查", error))?;
    let results = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|error| StorageError::sqlite("执行快照完整性检查", error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| StorageError::sqlite("读取快照完整性检查结果", error))?;
    if results != ["ok"] {
        return Err(StorageError::Message("snapshot integrity check failed"));
    }
    let (count, n): (i64, i64) = conn
        .query_row(
            "SELECT count(*), coalesce(max(seq), 0) FROM store_events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|error| StorageError::sqlite("查询事件账本", error))?;
    if n < before || count != n {
        return Err(StorageError::Message("snapshot ledger is incomplete"));
    }
    Ok(n)
}

fn rename_snapshot(from: &Path, to: &Path) -> Result<(), StorageError> {
    fs::rename(from, to).map_err(|error| StorageError::io("重命名备份文件", error))
}

fn sync_file(path: &Path) -> Result<(), StorageError> {
    File::open(path)
        .map_err(|error| StorageError::io("打开备份文件以同步", error))?
        .sync_all()
        .map_err(|error| StorageError::io("同步备份文件", error))?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), StorageError> {
    File::open(path)
        .map_err(|error| StorageError::io("打开备份目录以同步", error))?
        .sync_all()
        .map_err(|error| StorageError::io("同步备份目录", error))?;
    Ok(())
}

fn remove_logged(path: &Path) {
    if let Err(error) = fs::remove_file(path) {
        tracing::warn!(path = %path.display(), %error, "could not remove backup file");
    }
}

fn canonical_v7(value: &str) -> bool {
    BackupId::parse(value).is_ok_and(|id| id.to_string() == value)
}

fn final_number(name: &str, store_id: StoreId) -> Option<u64> {
    let rest = name
        .strip_prefix(&format!("boh-{store_id}-"))?
        .strip_suffix(".db")?;
    let (time, rest) = rest.split_once('-')?;
    let bytes = time.as_bytes();
    if bytes.len() != 16
        || bytes[8] != b'T'
        || bytes[15] != b'Z'
        || !bytes[..8]
            .iter()
            .chain(&bytes[9..15])
            .all(u8::is_ascii_digit)
    {
        return None;
    }
    let (number, id) = rest.split_once('-')?;
    if number.starts_with('0')
        || number.is_empty()
        || !number.bytes().all(|b| b.is_ascii_digit())
        || !canonical_v7(id)
    {
        return None;
    }
    number.parse().ok()
}

fn utc_label(time: UnixMillis) -> String {
    if let Ok(timestamp) = Timestamp::from_millisecond(time.0) {
        let date = timestamp.to_zoned(jiff::tz::TimeZone::UTC);
        if (0..=9999).contains(&date.year()) {
            return format!(
                "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
                date.year(),
                date.month(),
                date.day(),
                date.hour(),
                date.minute(),
                date.second()
            );
        }
    }
    "00000000T000000Z".into()
}

#[derive(Clone, Default)]
struct BackupGate(Arc<Gate>);

#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    released: Condvar,
    started: Notify,
}

#[derive(Default)]
struct GateState {
    holds: usize,
    waiting: bool,
}

impl BackupGate {
    fn wait(&self) {
        let mut state = lock(&self.0.state);
        if state.holds != 0 {
            state.waiting = true;
            self.0.started.notify_waiters();
            while state.holds != 0 {
                state = self
                    .0
                    .released
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            state.waiting = false;
        }
    }
}

#[doc(hidden)]
pub struct BackupHold(BackupGate);

impl BackupHold {
    pub async fn started(&self) {
        loop {
            let notified = self.0.0.started.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if lock(&self.0.0.state).waiting {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for BackupHold {
    fn drop(&mut self) {
        let mut state = lock(&self.0.0.state);
        state.holds -= 1;
        if state.holds == 0 {
            self.0.0.released.notify_all();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use std::num::NonZeroUsize;

    #[tokio::test]
    async fn publication_sync_failure_fails_but_cleanup_sync_failure_keeps_success() {
        for failed_sync in [1, 2] {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("boh.db");
            let directory = dir.path().join("backups");
            let storage = crate::open(&db, NonZeroUsize::MIN).unwrap();
            let clock = ManualClock::new(UnixMillis(1_791_261_000_000)).clock();
            let backup = Backup::prepare(
                &db,
                &directory,
                StoreId::parse("01890a5d-ac96-774b-bcce-b302099a8050").unwrap(),
                NonZeroU32::MIN,
            )
            .unwrap();
            assert_eq!(backup.run(&clock).unwrap(), 0);
            let old = directory.join(backup.names().unwrap().pop().unwrap());
            let health = BackupHealth::default();
            health.record(&Ok(0), UnixMillis(1));
            let mut calls = 0;
            let result = backup.run_with_directory_sync(&clock, |path| {
                calls += 1;
                if calls == failed_sync {
                    Err(StorageError::io(
                        "同步备份目录",
                        std::io::Error::other("injected directory fsync failure"),
                    ))
                } else {
                    sync_directory(path)
                }
            });
            health.record(&result, UnixMillis(2));
            let status = health.snapshot();
            if failed_sync == 1 {
                assert!(matches!(
                    result,
                    Err(StorageError::Backup {
                        stage: BackupStage::DirectorySync,
                        number: Some(2),
                        ..
                    })
                ));
                assert_eq!(calls, 1);
                assert!(
                    old.exists(),
                    "publication failure must not prune old backups"
                );
                assert_eq!(backup.names().unwrap().len(), 2);
                assert_eq!(status.last_ok_at, Some(UnixMillis(1)));
                assert_eq!(status.last_failed_at, Some(UnixMillis(2)));
            } else {
                assert_eq!(result.unwrap(), 0);
                assert_eq!(calls, 2);
                assert!(!old.exists());
                let names = backup.names().unwrap();
                assert_eq!(names.len(), 1);
                assert_eq!(verify(&directory.join(&names[0]), 0).unwrap(), 0);
                assert_eq!(status.last_ok_at, Some(UnixMillis(2)));
                assert_eq!(status.last_seq, Some(0));
                assert_eq!(status.last_failed_at, None);
            }
            storage.writer_handle.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    async fn file_sync_and_rename_failures_keep_the_phase_and_allocated_number() {
        for failed in [BackupStage::FileSync, BackupStage::Rename] {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("boh.db");
            let storage = crate::open(&db, NonZeroUsize::MIN).unwrap();
            let directory = dir.path().join("backups");
            let backup = Backup::prepare(
                &db,
                &directory,
                StoreId::parse("01890a5d-ac96-774b-bcce-b302099a8050").unwrap(),
                NonZeroU32::MIN,
            )
            .unwrap();
            let clock = ManualClock::new(UnixMillis(1_791_261_000_000)).clock();
            let error = backup
                .run_with_publication(
                    &clock,
                    |path| {
                        if failed == BackupStage::FileSync {
                            Err(StorageError::io(
                                "同步备份文件",
                                std::io::Error::other("injected file sync failure"),
                            ))
                        } else {
                            sync_file(path)
                        }
                    },
                    |from, to| {
                        if failed == BackupStage::Rename {
                            Err(StorageError::io(
                                "重命名备份文件",
                                std::io::Error::other("injected rename failure"),
                            ))
                        } else {
                            rename_snapshot(from, to)
                        }
                    },
                    sync_directory,
                )
                .unwrap_err();
            let expected_number = if failed == BackupStage::Rename {
                Some(1)
            } else {
                None
            };
            assert!(matches!(&error, StorageError::Backup { stage, number, .. }
                if *stage == failed && *number == expected_number));
            assert!(std::error::Error::source(&error).is_some());
            assert_eq!(backup.names().unwrap().len(), 0);
            storage.writer_handle.shutdown().await.unwrap();
        }
    }

    #[tokio::test]
    #[allow(clippy::disallowed_methods)] // Backup unit test controls the exact VACUUM boundary.
    async fn vacuum_failure_reports_generation_without_permission_assumptions() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("boh.db");
        let storage = crate::open(&db, NonZeroUsize::MIN).unwrap();
        let directory = dir.path().join("backups");
        let backup = Backup::prepare(
            &db,
            &directory,
            StoreId::parse("01890a5d-ac96-774b-bcce-b302099a8050").unwrap(),
            NonZeroU32::MIN,
        )
        .unwrap();
        let hold = backup.hold();
        let clock = ManualClock::new(UnixMillis(1_791_261_000_000)).clock();
        let task = tokio::task::spawn_blocking(move || backup.run(&clock));
        hold.started().await;
        fs::remove_dir(&directory).unwrap();
        fs::write(&directory, b"a file cannot contain the snapshot").unwrap();
        drop(hold);
        let error = task.await.unwrap().unwrap_err();
        assert!(
            matches!(&error, StorageError::Backup { stage: BackupStage::Generate, number: None, source, .. }
            if matches!(source.downcast_ref::<StorageError>(), Some(StorageError::Sqlite { operation: "生成备份快照", .. })))
        );
        storage.writer_handle.shutdown().await.unwrap();
    }

    #[test]
    fn file_label_handles_year_boundaries_and_unrepresentable_time() {
        for (timestamp, label) in [
            ("0000-01-01T00:00:00Z", "00000101T000000Z"),
            ("9999-12-30T00:00:00Z", "99991230T000000Z"),
            ("-000001-12-31T23:59:59Z", "00000000T000000Z"),
        ] {
            let time: Timestamp = timestamp.parse().unwrap();
            assert_eq!(utc_label(UnixMillis(time.as_millisecond())), label);
        }
        assert_eq!(utc_label(UnixMillis(i64::MAX)), "00000000T000000Z");
    }

    #[tokio::test]
    async fn source_is_read_only_and_verification_enforces_the_snapshot_lower_bound() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("boh.db");
        let storage = crate::open(&db, NonZeroUsize::MIN).unwrap();
        let source = open_source(&db).unwrap();
        let query_only: bool = source
            .pragma_query_value(None, "query_only", |r| r.get(0))
            .unwrap();
        assert!(!query_only, "VACUUM INTO needs query_only disabled");
        assert!(
            source
                .execute_batch("CREATE TABLE forbidden (id INTEGER) STRICT")
                .is_err()
        );
        let snapshot = dir.path().join("snapshot.db");
        source
            .execute("VACUUM INTO ?1", [snapshot.to_str().unwrap()])
            .unwrap();
        assert_eq!(verify(&snapshot, 0).unwrap(), 0);
        assert!(matches!(
            verify(&snapshot, 1),
            Err(StorageError::Message(_))
        ));
        let corrupt = dir.path().join("corrupt.db");
        fs::write(&corrupt, b"not a SQLite database").unwrap();
        assert!(verify(&corrupt, 0).is_err());
        storage.writer_handle.shutdown().await.unwrap();
    }
}
