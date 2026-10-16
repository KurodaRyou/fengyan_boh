//! 应用层：HTTP 接口与 service 命令编排；sync 上行同步 worker 待实现。

pub mod actor;
pub mod background;
pub mod clock_sync;
pub mod http;
mod invariant_check;
pub use invariant_check::InvariantCheckHealth;
#[doc(hidden)]
pub use invariant_check::InvariantCheckHold;
mod service;
pub use service::master_data::initialize_store;
#[cfg(test)]
mod tests;

use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};

use axum::Router;
use boh_domain::StoreId;
use boh_domain::time::{
    BusinessDayCutoff, StoreTimeZone, parse_business_day_cutoff, parse_closing_backup_time,
    parse_timezone,
};
#[doc(hidden)]
pub use boh_storage::backup::BackupHold;
use boh_storage::backup::{Backup, BackupHealth};
use boh_storage::clock::Clock;
use boh_storage::{Readers, StorageError, Writer};

/// 所有 handler 共享的状态。
#[derive(Clone)]
pub struct AppState {
    pub writer: Writer,
    pub readers: Readers,
    pub clock: Clock,
    pub clock_signal: clock_sync::ClockSignal,
    pub timezone: StoreTimeZone,
    pub business_day_cutoff: BusinessDayCutoff,
    pub dev_actor_stub: bool,
    pub db_path: PathBuf,
    pub backup_health: BackupHealth,
    pub invariant_health: InvariantCheckHealth,
}

/// 构造与生产入口共用路由的 HTTP 黑盒测试入口。
#[doc(hidden)]
pub fn test_router(
    db_path: &Path,
    clock: Clock,
    clock_synced_at: Option<boh_domain::UnixMillis>,
) -> Result<Router, StorageError> {
    let storage = boh_storage::open(db_path, NonZeroUsize::MIN)?;
    let store_id = StoreId::parse("01890a5d-ac96-774b-bcce-b302099a8050")?;
    let created_at = clock.now();
    // Writer::call's channels do not require a runtime. The synchronous locked-test
    // entry waits for the dedicated writer without nesting a Tokio runtime.
    wait_for(
        storage
            .writer
            .call(move |tx| boh_storage::store::initialize_if_empty(tx, store_id, created_at)),
    )?;
    let timezone = parse_timezone("Asia/Shanghai")?;
    let business_day_cutoff = parse_business_day_cutoff("04:00")?;
    // Router 最后一个 Writer 克隆释放后，队列关闭，写线程自行退出。
    drop(storage.writer_handle);
    Ok(http::router(AppState {
        writer: storage.writer,
        readers: storage.readers,
        clock,
        clock_signal: clock_sync::ClockSignal::new(clock_synced_at),
        timezone,
        business_day_cutoff,
        dev_actor_stub: true,
        db_path: db_path.to_owned(),
        backup_health: BackupHealth::default(),
        invariant_health: InvariantCheckHealth::default(),
    }))
}

#[doc(hidden)]
pub struct TestNodeConfig {
    pub backup_dir: PathBuf,
    pub backup_keep_count: NonZeroU32,
    pub timezone: String,
    pub closing_backup_time: String,
    pub ntp_servers: Vec<String>,
}

#[doc(hidden)]
pub struct TestNode {
    pub router: Router,
    backup: Backup,
    tasks: background::BackupTasks,
    clock_tasks: clock_sync::ClockTasks,
    writer_handle: boh_storage::WriterHandle,
    writer: Writer,
}

impl TestNode {
    #[allow(clippy::disallowed_methods)] // Locked backup tests pause execution through this test-node entry point.
    pub fn hold_backups(&self) -> BackupHold {
        self.backup.hold()
    }

    pub fn writer(&self) -> Writer {
        self.writer.clone()
    }

    pub async fn run_invariant_check(&self) {
        self.tasks.run_invariant_check().await;
    }

    pub fn hold_invariant_checks(&self) -> InvariantCheckHold {
        self.tasks.hold_invariant_checks()
    }

    pub async fn wait_clock_checks(&self, count: u64) {
        self.clock_tasks.wait_checks(count).await;
    }

    pub async fn wait_clock_ticks(&self, count: u64) {
        self.clock_tasks.wait_ticks(count).await;
    }

    pub async fn shutdown(self) -> Result<(), StorageError> {
        drop(self.router);
        self.clock_tasks.stop_handle().stop();
        self.tasks.stop_handle().stop();
        let clock_stopped = self.clock_tasks.shutdown().await;
        let stopped = self.tasks.shutdown().await;
        let closed = self.writer_handle.shutdown().await;
        clock_stopped?;
        stopped?;
        closed
    }
}

#[doc(hidden)]
pub async fn test_node(
    db_path: &Path,
    clock: Clock,
    config: TestNodeConfig,
) -> Result<TestNode, StorageError> {
    let store_id = StoreId::parse("01890a5d-ac96-774b-bcce-b302099a8050")?;
    let timezone = parse_timezone(&config.timezone)?;
    let closing = parse_closing_backup_time(&config.closing_backup_time)?;
    let servers = clock_sync::parse_ntp_servers(&config.ntp_servers)
        .map_err(|error| StorageError::external("解析 NTP 服务器列表", error))?;
    let backup = Backup::prepare(
        db_path,
        &config.backup_dir,
        store_id,
        config.backup_keep_count,
    )?;
    let storage = boh_storage::open(db_path, NonZeroUsize::MIN)?;
    let created_at = clock.now();
    let initialized = storage
        .writer
        .call(move |tx| boh_storage::store::initialize_if_empty(tx, store_id, created_at))
        .await;
    if let Err(error) = initialized {
        storage.writer_handle.shutdown().await?;
        return Err(error);
    }
    let backup_health = BackupHealth::default();
    let invariant_health = InvariantCheckHealth::default();
    let tasks = match background::BackupTasks::start(
        backup.clone(),
        clock.clone(),
        backup_health.clone(),
        timezone.clone(),
        closing,
        storage.readers.clone(),
        invariant_health.clone(),
    ) {
        Ok(tasks) => tasks,
        Err(error) => {
            storage.writer_handle.shutdown().await?;
            return Err(error);
        }
    };
    let clock_signal = clock_sync::ClockSignal::load(db_path).await;
    let clock_tasks =
        clock_sync::ClockTasks::start(db_path, clock.clone(), servers, clock_signal.clone()).await;
    let router = http::router(AppState {
        writer: storage.writer.clone(),
        readers: storage.readers,
        clock,
        clock_signal,
        timezone,
        business_day_cutoff: parse_business_day_cutoff("04:00")?,
        dev_actor_stub: true,
        db_path: db_path.to_owned(),
        backup_health,
        invariant_health,
    });
    Ok(TestNode {
        router,
        backup,
        tasks,
        clock_tasks,
        writer_handle: storage.writer_handle,
        writer: storage.writer,
    })
}

fn wait_for<T>(future: impl std::future::Future<Output = T>) -> T {
    struct WakeThread(std::thread::Thread);
    impl std::task::Wake for WakeThread {
        fn wake(self: std::sync::Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &std::sync::Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(std::sync::Arc::new(WakeThread(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => return value,
            std::task::Poll::Pending => std::thread::park(),
        }
    }
}
