//! 门店边缘节点入口：加载配置 → 打开数据库并迁移 → 启动 HTTP →
//! 收到 SIGTERM / SIGINT 后优雅关闭。

mod config;

use std::path::PathBuf;

use anyhow::Context;
use boh_app::{AppState, http};
use boh_storage::clock::Clock;
use config::parse_config;
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

const DEFAULT_CONFIG_PATH: &str = "/etc/boh/config.toml";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config_path = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH), PathBuf::from);
    let text = std::fs::read_to_string(&config_path)
        .with_context(|| format!("read config {}", config_path.display()))?;
    let config =
        parse_config(&text).with_context(|| format!("parse config {}", config_path.display()))?;
    tracing::info!(
        store_id = %config.store_id,
        db = %config.db_path.display(),
        timezone = ?config.timezone,
        business_day_cutoff = ?config.business_day_cutoff,
        "starting"
    );

    let storage =
        boh_storage::open(&config.db_path, config.reader_pool_size).context("open storage")?;

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
    let shutdown = async move {
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
        tracing::info!("shutdown signal received");
    };

    let app = http::router(AppState {
        writer: storage.writer,
        readers: storage.readers,
        clock: Clock::system(),
    });
    let listener = tokio::net::TcpListener::bind(config.listen_addr)
        .await
        .with_context(|| format!("bind {}", config.listen_addr))?;
    tracing::info!(addr = %config.listen_addr, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .context("http server")?;

    tracing::info!("http stopped, draining writer");
    storage.writer_handle.shutdown().await?;
    tracing::info!("shutdown complete");
    Ok(())
}
