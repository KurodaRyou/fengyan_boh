//! Inventory checks and process-local reports. A report is published only in full.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use boh_domain::UnixMillis;
use boh_storage::rusqlite::Connection;
use boh_storage::{Readers, StorageError, clock::Clock};
use serde::Serialize;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Default, Serialize)]
pub struct InvariantCheckStatus {
    last_attempt: Option<Attempt>,
    last_report: Option<Report>,
}

impl InvariantCheckStatus {
    pub(crate) fn degraded(&self) -> bool {
        self.last_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.result != Outcome::Passed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Passed,
    Violated,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
struct Attempt {
    finished_at: UnixMillis,
    result: Outcome,
}

#[derive(Debug, Clone, Serialize)]
struct Report {
    finished_at: UnixMillis,
    max_seq: i64,
    checks: Vec<Check>,
    violation_count: u64,
    samples: Vec<Sample>,
    samples_truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
struct Check {
    check: &'static str,
    violations: u64,
}

#[derive(Debug, Clone, Serialize)]
struct Sample {
    check: &'static str,
    subject: Subject,
    actual: i64,
    expected: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum Subject {
    Lot { item_id: String, lot_id: String },
    Item { item_id: String },
}

#[derive(Clone, Default)]
pub struct InvariantCheckHealth(Arc<Mutex<InvariantCheckStatus>>);

impl InvariantCheckHealth {
    pub fn snapshot(&self) -> InvariantCheckStatus {
        lock(&self.0).clone()
    }

    fn record(&self, result: Result<Report, StorageError>, finished_at: UnixMillis) {
        let mut status = lock(&self.0);
        let outcome = match result {
            Ok(mut report) => {
                report.finished_at = finished_at;
                let outcome = if report.violation_count == 0 {
                    Outcome::Passed
                } else {
                    tracing::error!(checks = ?report.checks, samples = ?report.samples, "inventory invariants violated");
                    Outcome::Violated
                };
                status.last_report = Some(report);
                outcome
            }
            Err(error) => {
                tracing::error!(error = %boh_storage::Diagnostic(&error), "invariant check failed");
                Outcome::Failed
            }
        };
        status.last_attempt = Some(Attempt {
            finished_at,
            result: outcome,
        });
    }
}

pub(crate) struct InvariantChecks {
    handle: CheckHandle,
    task: JoinHandle<()>,
}

#[derive(Clone)]
pub(crate) struct CheckHandle {
    queue: Arc<Mutex<Queue>>,
    gate: Arc<Gate>,
}

struct Queue {
    accepting: bool,
    sender: Option<mpsc::UnboundedSender<oneshot::Sender<()>>>,
}

impl InvariantChecks {
    pub(crate) fn start(readers: Readers, clock: Clock, health: InvariantCheckHealth) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<oneshot::Sender<()>>();
        let handle = CheckHandle {
            queue: Arc::new(Mutex::new(Queue {
                accepting: true,
                sender: Some(sender),
            })),
            gate: Arc::new(Gate::default()),
        };
        // Enqueue exactly one startup check before returning, without waiting for it.
        let _startup = handle.submit(false);
        let gate = handle.gate.clone();
        let task = tokio::spawn(async move {
            while let Some(completed) = receiver.recv().await {
                // The single worker owns the whole execution, including publication.
                gate.wait().await;
                let result = readers.call(check_snapshot).await;
                health.record(result, clock.now());
                let _ = completed.send(());
            }
        });
        Self { handle, task }
    }

    pub(crate) fn handle(&self) -> CheckHandle {
        self.handle.clone()
    }

    pub(crate) async fn shutdown(self) -> Result<(), StorageError> {
        {
            let mut queue = lock(&self.handle.queue);
            queue.accepting = false;
            queue.sender = None;
        }
        // Closing the sender drains receiver's accepted messages before it ends.
        self.task
            .await
            .map_err(|error| StorageError::join("等待不变量自检入口关闭", error))
    }
}

impl CheckHandle {
    fn submit(&self, external: bool) -> Option<oneshot::Receiver<()>> {
        let queue = lock(&self.queue);
        if external && !queue.accepting {
            return None;
        }
        let (completed, result) = oneshot::channel();
        queue.sender.as_ref()?.send(completed).ok()?;
        Some(result)
    }

    pub(crate) fn stop(&self) {
        lock(&self.queue).accepting = false;
    }

    pub(crate) async fn run(&self) {
        if let Some(result) = self.submit(true) {
            let _ = result.await;
        }
    }

    pub(crate) async fn closing(&self) {
        // Accepted closing flows must still submit their check while draining.
        if let Some(result) = self.submit(false) {
            let _ = result.await;
        }
    }

    pub(crate) fn hold(&self) -> InvariantCheckHold {
        lock(&self.gate.state).holds += 1;
        InvariantCheckHold(self.gate.clone())
    }
}

#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    released: Notify,
    started: Notify,
}

#[derive(Default)]
struct GateState {
    holds: usize,
    waiting: bool,
}

impl Gate {
    async fn wait(&self) {
        loop {
            let released = self.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            {
                let mut state = lock(&self.state);
                if state.holds == 0 {
                    state.waiting = false;
                    return;
                }
                state.waiting = true;
                self.started.notify_waiters();
            }
            released.await;
        }
    }
}

#[doc(hidden)]
pub struct InvariantCheckHold(Arc<Gate>);

impl InvariantCheckHold {
    pub async fn started(&self) {
        loop {
            let started = self.0.started.notified();
            tokio::pin!(started);
            started.as_mut().enable();
            if lock(&self.0.state).waiting {
                return;
            }
            started.await;
        }
    }
}

impl Drop for InvariantCheckHold {
    fn drop(&mut self) {
        let mut state = lock(&self.0.state);
        state.holds -= 1;
        if state.holds == 0 {
            self.0.released.notify_waiters();
        }
    }
}

fn check_snapshot(conn: &Connection) -> Result<Report, StorageError> {
    let max_seq = conn
        .query_row(
            "SELECT coalesce(max(seq), 0) FROM store_events",
            [],
            |row| row.get(0),
        )
        .map_err(|error| StorageError::sqlite("读取自检快照 max_seq", error))?;
    let mut report = Report {
        finished_at: UnixMillis(0),
        max_seq,
        checks: Vec::new(),
        violation_count: 0,
        samples: Vec::new(),
        samples_truncated: false,
    };
    let queries = [
        (
            "INVENTORY_LOT_REMAINING",
            "SELECT l.item_id, l.lot_id, l.remaining_qty,
                    coalesce((SELECT sum(m.qty_delta) FROM inventory_movements m WHERE m.lot_id = l.lot_id), 0), 0
             FROM inventory_lots l ORDER BY l.item_id COLLATE BINARY, l.lot_id COLLATE BINARY",
            "检查批次余量与库存流水",
            true,
        ),
        (
            "INVENTORY_UNALLOCATED",
            "SELECT s.item_id, NULL, coalesce(u.qty, 0),
                    coalesce((SELECT sum(m.qty_delta) FROM inventory_movements m WHERE m.item_id = s.item_id AND m.lot_id IS NULL), 0), 0
             FROM (SELECT item_id FROM inventory_unallocated UNION SELECT item_id FROM inventory_movements WHERE lot_id IS NULL) s
             LEFT JOIN inventory_unallocated u ON u.item_id = s.item_id ORDER BY s.item_id COLLATE BINARY",
            "检查账外缺口与库存流水",
            false,
        ),
        (
            "INVENTORY_ON_HAND",
            "SELECT h.item_id, NULL, h.qty,
                    coalesce((SELECT sum(l.remaining_qty) FROM inventory_lots l WHERE l.item_id = h.item_id), 0),
                    coalesce((SELECT u.qty FROM inventory_unallocated u WHERE u.item_id = h.item_id), 0)
             FROM inventory_on_hand h ORDER BY h.item_id COLLATE BINARY",
            "检查账面库存与批次及账外缺口",
            false,
        ),
    ];
    for (check, sql, operation, lot) in queries {
        let mut statement = conn
            .prepare(sql)
            .map_err(|error| StorageError::sqlite(operation, error))?;
        let mut rows = statement
            .query([])
            .map_err(|error| StorageError::sqlite(operation, error))?;
        let mut violations = 0_u64;
        while let Some(row) = rows
            .next()
            .map_err(|error| StorageError::sqlite(operation, error))?
        {
            let values = (|| -> Result<_, boh_storage::rusqlite::Error> {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })()
            .map_err(|error| StorageError::sqlite(operation, error))?;
            let (item_id, lot_id, actual, sum, unallocated) = values;
            let expected = sum
                .checked_add(unallocated)
                .ok_or_else(|| StorageError::external(operation, QuantityOverflow))?;
            if actual != expected {
                violations = violations
                    .checked_add(1)
                    .ok_or_else(|| StorageError::external(operation, QuantityOverflow))?;
                if report.samples.len() < 20 {
                    let subject = if lot {
                        Subject::Lot {
                            item_id,
                            lot_id: lot_id
                                .ok_or(StorageError::Message("missing invariant lot subject"))?,
                        }
                    } else {
                        Subject::Item { item_id }
                    };
                    report.samples.push(Sample {
                        check,
                        subject,
                        actual,
                        expected,
                    });
                }
            }
        }
        report.violation_count = report
            .violation_count
            .checked_add(violations)
            .ok_or_else(|| StorageError::external(operation, QuantityOverflow))?;
        report.checks.push(Check { check, violations });
    }
    report.samples_truncated = report.violation_count > report.samples.len() as u64;
    Ok(report)
}

#[derive(Debug)]
struct QuantityOverflow;

impl std::fmt::Display for QuantityOverflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invariant aggregate exceeds integer range")
    }
}

impl std::error::Error for QuantityOverflow {}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boh_storage::clock::ManualClock;
    use std::num::NonZeroUsize;
    use std::time::Duration;

    #[tokio::test]
    async fn shutdown_drains_queued_checks_even_when_the_submitter_stops_waiting() {
        let dir = tempfile::tempdir().unwrap();
        let storage = boh_storage::open(&dir.path().join("boh.db"), NonZeroUsize::MIN).unwrap();
        let clock = ManualClock::new(UnixMillis(10));
        let health = InvariantCheckHealth::default();
        let checks = InvariantChecks::start(storage.readers.clone(), clock.clock(), health.clone());
        let handle = checks.handle();
        let hold = handle.hold();
        // The startup check is queued, but neither result is visible before it ends.
        assert!(health.snapshot().last_attempt.is_none());
        assert!(health.snapshot().last_report.is_none());
        tokio::time::timeout(Duration::from_secs(5), hold.started())
            .await
            .unwrap();
        drop(handle.submit(true).unwrap());
        let first = handle.submit(true).unwrap();
        let second = handle.submit(true).unwrap();
        handle.stop();
        assert!(handle.submit(true).is_none());
        let mut shutdown = std::pin::pin!(checks.shutdown());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut shutdown)
                .await
                .is_err()
        );
        assert!(health.snapshot().last_attempt.is_none());
        clock.set(UnixMillis(9));
        drop(hold);
        tokio::time::timeout(Duration::from_secs(5), shutdown)
            .await
            .unwrap()
            .unwrap();
        first.await.unwrap();
        second.await.unwrap();
        let status = health.snapshot();
        assert_eq!(status.last_attempt.unwrap().result, Outcome::Passed);
        assert_eq!(status.last_report.unwrap().finished_at, UnixMillis(9));
        assert!(handle.submit(true).is_none());
        storage.writer_handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn empty_snapshot_executes_all_checks_and_failed_attempt_keeps_report() {
        let dir = tempfile::tempdir().unwrap();
        let storage = boh_storage::open(&dir.path().join("boh.db"), NonZeroUsize::MIN).unwrap();
        let report = storage.readers.call(check_snapshot).await.unwrap();
        assert_eq!(report.max_seq, 0);
        assert_eq!(report.checks.len(), 3);
        assert_eq!(report.violation_count, 0);
        let health = InvariantCheckHealth::default();
        health.record(Ok(report), UnixMillis(10));
        health.record(
            Err(StorageError::external("测试汇总溢出", QuantityOverflow)),
            UnixMillis(9),
        );
        let status = health.snapshot();
        assert!(status.degraded());
        assert_eq!(status.last_attempt.unwrap().finished_at, UnixMillis(9));
        assert_eq!(status.last_report.unwrap().finished_at, UnixMillis(10));
        storage.writer_handle.shutdown().await.unwrap();
    }
}
