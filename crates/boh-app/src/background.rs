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
        let rolled_back = now < self.observed;
        let hourly_due = !rolled_back && now >= self.hourly;
        let closing_due = !rolled_back && now >= self.closing;
        // Calculate both deadlines before changing state, so a failed calculation
        // cannot leave a future deadline behind after the clock returns to range.
        let hourly = if rolled_back || hourly_due {
            next_hour(now)?
        } else {
            self.hourly
        };
        let closing = if rolled_back || closing_due {
            next_closing_backup(now, &self.timezone, self.closing_time).map_err(schedule_error)?
        } else {
            self.closing
        };
        self.hourly = hourly;
        self.closing = closing;
        self.observed = now;
        if hourly_due {
            enqueue(queue, Trigger::Hourly);
        }
        if closing_due {
            enqueue(queue, Trigger::Closing);
        }
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
        let task = tokio::spawn(run(
            backup,
            clock,
            health,
            schedule,
            changes,
            stopping,
            VecDeque::new(),
        ));
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
    mut queue: VecDeque<Trigger>,
) {
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
        let deadline = if stopping {
            None
        } else {
            match schedule.observe(clock.now(), &mut queue) {
                Ok(()) => Some(schedule.deadline()),
                Err(error) => {
                    tracing::error!(%error, "cannot calculate backup trigger");
                    // Keep draining accepted work, but do not spin on a stale deadline.
                    None
                }
            }
        };
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
            _ = changes.changed(deadline), if !stopping => {}
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
    async fn scheduling_error_drains_accepted_backups_and_recovers_with_the_clock() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("boh.db");
        let directory = dir.path().join("backups");
        let storage = boh_storage::open(&db, NonZeroUsize::MIN).unwrap();
        let initial = UnixMillis(1_791_261_000_000); // 12:30 Shanghai
        let schedule = Schedule::new(
            initial,
            parse_timezone("Asia/Shanghai").unwrap(),
            parse_closing_backup_time("23:30").unwrap(),
        )
        .unwrap();
        let clock = ManualClock::new(UnixMillis(253_402_400_000_000)); // Beyond jiff's range.
        let backup = Backup::prepare(
            &db,
            &directory,
            StoreId::parse("01890a5d-ac96-774b-bcce-b302099a8050").unwrap(),
            NonZeroU32::new(3).unwrap(),
        )
        .unwrap();
        let hold = backup.hold();
        let health = BackupHealth::default();
        let (stop, stopping) = watch::channel(false);
        let task = tokio::spawn(run(
            backup,
            clock.clock(),
            health.clone(),
            schedule,
            clock.clock().changes(),
            stopping,
            VecDeque::from([Trigger::Hourly, Trigger::Closing]),
        ));
        tokio::time::timeout(Duration::from_secs(5), hold.started())
            .await
            .expect("a scheduling failure must not prevent accepted work from starting");
        drop(hold);
        for count in [2, 3] {
            if count == 3 {
                clock.set(UnixMillis(initial.0 + 30 * 60 * 1000));
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let published = std::fs::read_dir(&directory)
                        .unwrap()
                        .map(|entry| entry.unwrap().file_name())
                        .filter(|name| name.to_string_lossy().starts_with("boh-"))
                        .count();
                    if published == count && health.snapshot().last_seq == Some(0) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
        }
        stop.send_replace(true);
        task.await.unwrap();
        storage.writer_handle.shutdown().await.unwrap();
    }

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
