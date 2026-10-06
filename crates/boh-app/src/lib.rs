//! 应用层：HTTP 接口；之后加入 service（命令编排）和 sync（outbox worker）。

pub mod http;

use boh_storage::{Readers, Writer};

/// 所有 handler 共享的状态。
#[derive(Clone)]
pub struct AppState {
    pub writer: Writer,
    pub readers: Readers,
}
