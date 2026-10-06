//! 应用层：HTTP 接口与 service 命令编排；sync 上行同步 worker 待实现。

pub mod actor;
pub mod http;
mod service;
#[cfg(test)]
mod tests;

use std::num::NonZeroUsize;
use std::path::Path;

use axum::Router;
use boh_domain::StoreId;
use boh_domain::time::{
    BusinessDayCutoff, StoreTimeZone, parse_business_day_cutoff, parse_timezone,
};
use boh_storage::clock::Clock;
use boh_storage::{Readers, StorageError, Writer};

/// 所有 handler 共享的状态。
#[derive(Clone)]
pub struct AppState {
    pub writer: Writer,
    pub readers: Readers,
    pub clock: Clock,
    pub timezone: StoreTimeZone,
    pub business_day_cutoff: BusinessDayCutoff,
    pub dev_actor_stub: bool,
}

/// 构造与生产入口共用路由的 HTTP 黑盒测试入口。
#[doc(hidden)]
pub fn test_router(db_path: &Path, clock: Clock) -> Result<Router, StorageError> {
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
        timezone,
        business_day_cutoff,
        dev_actor_stub: true,
    }))
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
