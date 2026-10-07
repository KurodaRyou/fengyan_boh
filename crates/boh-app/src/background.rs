//! The two backup triggers share one worker and a bounded, deduplicated queue.

use std::collections::VecDeque;

use boh_domain::UnixMillis;
use boh_domain::time::{ClosingBackupTime, StoreTimeZone, next_closing_backup};
use boh_storage::StorageError;
use boh_storage::backup::{Backup, BackupHealth};
use boh_storage::clock::Clock;
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub struct BackupTasks {
    stop: BackupStop,
    task: JoinHandle<()>,
}

#[derive(Clone)]
pub struct BackupStop(watch::Sender<bool>);

impl BackupStop {
    /// Stop accepting triggers immediately; the task still drains accepted backups.
    pub fn stop(&self) {
        self.0.send_replace(true);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Hourly,
    Closing,
}

struct Schedule {
    timezone: StoreTimeZone,
    closing_time: ClosingBackupTime,
    hourly: UnixMillis,
    closing: UnixMillis,
    observed: UnixMillis,
}

impl Schedule {
    fn new(
        now: UnixMillis,
        timezone: StoreTimeZone,
        closing_time: ClosingBackupTime,
    ) -> Result<Self, StorageError> {
        Ok(Self {
            hourly: next_hour(now)?,
            closing: next_closing_backup(now, &timezone, closing_time).map_err(schedule_error)?,
            observed: now,
            timezone,
            closing_time,
        })
    }

    fn observe(
        &mut self,
        now: UnixMillis,
        queue: &mut VecDeque<Trigger>,
    ) -> Result<(), StorageError> {
        if now < self.observed {
            self.hourly = next_hour(now)?;
            self.closing = next_closing_backup(now, &self.timezone, self.closing_time)
                .map_err(schedule_error)?;
        } else {
            if now >= self.hourly {
                // Advance first: a jump over multiple hours produces only one trigger.
                self.hourly = next_hour(now)?;
                enqueue(queue, Trigger::Hourly);
            }
            if now >= self.closing {
                self.closing = next_closing_backup(now, &self.timezone, self.closing_time)
                    .map_err(schedule_error)?;
                enqueue(queue, Trigger::Closing);
            }
        }
        self.observed = now;
        Ok(())
    }

    fn deadline(&self) -> UnixMillis {
        self.hourly.min(self.closing)
    }
}

fn enqueue(queue: &mut VecDeque<Trigger>, trigger: Trigger) {
    if !queue.contains(&trigger) {
        queue.push_back(trigger);
    }
}

fn next_hour(after: UnixMillis) -> Result<UnixMillis, StorageError> {
    after
        .0
        .div_euclid(3_600_000)
        .checked_add(1)
        .and_then(|h| h.checked_mul(3_600_000))
        .map(UnixMillis)
        .ok_or_else(|| StorageError::Backup("hourly trigger is out of range".into()))
}

fn schedule_error(error: boh_domain::time::TimeError) -> StorageError {
    StorageError::Backup(error.to_string())
}

impl BackupTasks {
    /// Compute initial deadlines before returning, without firing a startup backup.
    pub fn start(
        backup: Backup,
        clock: Clock,
        health: BackupHealth,
        timezone: StoreTimeZone,
        closing_time: ClosingBackupTime,
    ) -> Result<Self, StorageError> {
        let changes = clock.changes();
        let schedule = Schedule::new(clock.now(), timezone, closing_time)?;
        let (stop, stopping) = watch::channel(false);
        let task = tokio::spawn(run(backup, clock, health, schedule, changes, stopping));
        Ok(Self {
            stop: BackupStop(stop),
            task,
        })
    }

    pub fn stop_handle(&self) -> BackupStop {
        self.stop.clone()
    }

    pub async fn shutdown(self) -> Result<(), StorageError> {
        self.stop.stop();
        self.task
            .await
            .map_err(|error| StorageError::Join(error.to_string()))
    }
}

async fn run(
    backup: Backup,
    clock: Clock,
    health: BackupHealth,
    mut schedule: Schedule,
    mut changes: boh_storage::clock::ClockChanges,
    mut stop: watch::Receiver<bool>,
) {
    let mut queue = VecDeque::new();
    let mut active: Option<JoinHandle<()>> = None;
    let mut stopping = false;
    loop {
        if !stopping && (*stop.borrow() || stop.has_changed().is_err()) {
            stopping = true;
            tracing::info!(
                remaining = queue.len() + usize::from(active.is_some()),
                "draining backups"
            );
        }
        if !stopping && let Err(error) = schedule.observe(clock.now(), &mut queue) {
            tracing::error!(%error, "cannot calculate backup trigger");
            // Await clock changes rather than spin on a deadline that cannot be advanced.
            tokio::select! {
                biased;
                _ = stop.changed() => {},
                _ = changes.changed() => {}
            }
            continue;
        }
        if active.is_none() && queue.pop_front().is_some() {
            let backup = backup.clone();
            let clock = clock.clone();
            let health = health.clone();
            active = Some(tokio::task::spawn_blocking(move || {
                let result = backup.run(&clock);
                health.record(&result, clock.now());
            }));
        }
        if stopping && active.is_none() && queue.is_empty() {
            return;
        }
        tokio::select! {
            biased;
            _ = stop.changed(), if !stopping => {}
            result = async {
                match active.as_mut() {
                    Some(task) => task.await,
                    None => std::future::pending().await,
                }
            } => {
                active = None;
                if let Err(error) = result {
                    health.record(&Err(StorageError::Join(error.to_string())), clock.now());
                }
            }
            _ = changes.changed(), if !stopping => {}
            _ = clock.sleep_until(schedule.deadline()), if !stopping => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boh_domain::StoreId;
    use boh_domain::time::{parse_closing_backup_time, parse_timezone};
    use boh_storage::clock::ManualClock;
    use std::num::{NonZeroU32, NonZeroUsize};
    use std::time::Duration;

    #[tokio::test]
    async fn stop_signal_rejects_triggers_while_a_backup_is_still_running() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("boh.db");
        let directory = dir.path().join("backups");
        let storage = boh_storage::open(&db, NonZeroUsize::MIN).unwrap();
        let clock = ManualClock::new(UnixMillis(1_791_261_000_000)); // 12:30 Shanghai
        let backup = Backup::prepare(
            &db,
            &directory,
            StoreId::parse("01890a5d-ac96-774b-bcce-b302099a8050").unwrap(),
            NonZeroU32::MIN,
        )
        .unwrap();
        let hold = backup.hold();
        let tasks = BackupTasks::start(
            backup,
            clock.clock(),
            BackupHealth::default(),
            parse_timezone("Asia/Shanghai").unwrap(),
            parse_closing_backup_time("23:30").unwrap(),
        )
        .unwrap();
        clock.advance(Duration::from_secs(30 * 60));
        tokio::time::timeout(Duration::from_secs(5), hold.started())
            .await
            .unwrap();
        tasks.stop_handle().stop();
        // Advance before the scheduler handles the stop signal. No new trigger may enter.
        clock.advance(Duration::from_secs(60 * 60));
        drop(hold);
        tasks.shutdown().await.unwrap();
        let files = std::fs::read_dir(&directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path().extension().unwrap(), "db");
        storage.writer_handle.shutdown().await.unwrap();
    }
}
