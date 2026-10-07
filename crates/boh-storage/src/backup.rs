//! Read-only VACUUM snapshots, verification, durable publication and store-local retention.

use std::fs::{self, File};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use boh_domain::{BackupId, StoreId, UnixMillis};
use jiff::Timestamp;
use rusqlite::{Connection, OpenFlags};
use tokio::sync::Notify;

use crate::{StorageError, clock::Clock, connection::apply_pragmas};

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
                tracing::error!(%error, "backup failed");
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
        fs::create_dir_all(directory)?;
        if !directory.is_dir() {
            return Err(StorageError::Backup(
                "backup path is not a directory".into(),
            ));
        }
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
        mut sync: impl FnMut(&Path) -> Result<(), StorageError>,
    ) -> Result<i64, StorageError> {
        let started = clock.now();
        self.gate.wait();
        let source = open_source(&self.db_path)?;
        let entropy: [u8; 10] = source.query_row("SELECT randomblob(10)", [], |row| row.get(0))?;
        let id = BackupId::from_parts(started, entropy)?;
        let temporary = self
            .directory
            .join(format!("tmp-{}-{id}.db", self.store_id));
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
            let path = temporary
                .to_str()
                .ok_or_else(|| StorageError::Backup("snapshot path is not UTF-8".into()))?;
            source.execute("VACUUM INTO ?1", [path])?;
            let n = verify(&temporary, before)?;
            File::open(&temporary)?.sync_all()?;
            let number = files
                .iter()
                .map(|(n, _)| *n)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| StorageError::Backup("backup number overflow".into()))?;
            let final_path = self.directory.join(format!(
                "boh-{}-{}-{number}-{id}.db",
                self.store_id,
                utc_label(started)
            ));
            fs::rename(&temporary, &final_path)?;
            sync(&self.directory)?;
            // Cleanup starts only after the new file and its directory entry are durable.
            files.push((number, final_path.clone()));
            files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            for (_, path) in files.into_iter().skip(self.keep.get() as usize) {
                if path != final_path {
                    remove_logged(&path);
                }
            }
            if let Err(error) = sync(&self.directory) {
                tracing::warn!(%error, "could not persist old backup cleanup; new backup is durable");
            }
            Ok(n)
        })();
        if result.is_err() && temporary.exists() {
            remove_logged(&temporary);
        }
        result
    }

    fn names(&self) -> Result<Vec<String>, StorageError> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            if let Some(name) = entry?.file_name().to_str() {
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
    )?;
    apply_pragmas(&conn)?;
    Ok(conn)
}

fn max_seq(conn: &Connection) -> Result<i64, StorageError> {
    Ok(
        conn.query_row("SELECT coalesce(max(seq), 0) FROM store_events", [], |r| {
            r.get(0)
        })?,
    )
}

#[allow(clippy::disallowed_methods)] // Verification preserves the snapshot's DELETE journal mode.
fn verify(path: &Path, before: i64) -> Result<i64, StorageError> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut stmt = conn.prepare("PRAGMA integrity_check")?;
    let results = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if results != ["ok"] {
        return Err(StorageError::Backup(
            "snapshot integrity check failed".into(),
        ));
    }
    let (count, n): (i64, i64) = conn.query_row(
        "SELECT count(*), coalesce(max(seq), 0) FROM store_events",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if n < before || count != n {
        return Err(StorageError::Backup("snapshot ledger is incomplete".into()));
    }
    Ok(n)
}

fn sync_directory(path: &Path) -> Result<(), StorageError> {
    File::open(path)?.sync_all()?;
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
                    Err(std::io::Error::other("injected directory fsync failure").into())
                } else {
                    sync_directory(path)
                }
            });
            health.record(&result, UnixMillis(2));
            let status = health.snapshot();
            if failed_sync == 1 {
                assert!(matches!(result, Err(StorageError::Io(_))));
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
        assert!(matches!(verify(&snapshot, 1), Err(StorageError::Backup(_))));
        let corrupt = dir.path().join("corrupt.db");
        fs::write(&corrupt, b"not a SQLite database").unwrap();
        assert!(verify(&corrupt, 0).is_err());
        storage.writer_handle.shutdown().await.unwrap();
    }
}
