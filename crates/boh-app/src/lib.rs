//! 应用层：HTTP 接口；之后加入 service（命令编排）和 sync（上行同步 worker（待实现））。

pub mod http;

use std::num::NonZeroUsize;
use std::path::Path;

use axum::Router;
use boh_storage::clock::Clock;
use boh_storage::{Readers, StorageError, Writer};

/// 所有 handler 共享的状态。
#[derive(Clone)]
pub struct AppState {
    pub writer: Writer,
    pub readers: Readers,
    pub clock: Clock,
}

/// 构造与生产入口共用路由的 HTTP 黑盒测试入口。
#[doc(hidden)]
pub fn test_router(db_path: &Path, clock: Clock) -> Result<Router, StorageError> {
    let storage = boh_storage::open(db_path, NonZeroUsize::MIN)?;
    // Router 最后一个 Writer 克隆释放后，队列关闭，写线程自行退出。
    drop(storage.writer_handle);
    Ok(http::router(AppState {
        writer: storage.writer,
        readers: storage.readers,
        clock,
    }))
}
