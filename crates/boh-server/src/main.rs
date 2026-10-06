//! 门店边缘节点入口：加载配置 → 打开数据库并迁移 → 启动 HTTP →
//! 收到 SIGTERM / SIGINT 后优雅关闭。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use boh_app::{AppState, http};
use boh_storage::{Readers, checkpoint_truncate, migrate, open_writer, spawn_writer};
use serde::Deserialize;
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

const DEFAULT_CONFIG_PATH: &str = "/etc/boh/config.toml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    store_id: String,
    db_path: PathBuf,
    listen_addr: SocketAddr,
    #[serde(default = "default_reader_pool_size")]
    reader_pool_size: usize,
}

fn default_reader_pool_size() -> usize {
    4
}

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
    let config = load_config(&config_path)?;
    tracing::info!(
        store_id = %config.store_id,
        db = %config.db_path.display(),
        "starting"
    );

    let mut writer_conn = open_writer(&config.db_path).context("open writer connection")?;
    migrate(&mut writer_conn).context("run migrations")?;
    let readers =
        Readers::open(&config.db_path, config.reader_pool_size).context("open reader pool")?;
    let (writer, writer_handle) = spawn_writer(writer_conn)?;

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
    let shutdown = async move {
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
        tracing::info!("shutdown signal received");
    };

    let app = http::router(AppState { writer, readers });
    let listener = tokio::net::TcpListener::bind(config.listen_addr)
        .await
        .with_context(|| format!("bind {}", config.listen_addr))?;
    tracing::info!(addr = %config.listen_addr, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .context("http server")?;

    tracing::info!("http stopped, draining writer");
    let conn = writer_handle.shutdown().await?;
    checkpoint_truncate(&conn)?;
    tracing::info!("shutdown complete");
    Ok(())
}

fn load_config(path: &Path) -> anyhow::Result<Config> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("parse config {}", path.display()))?;
    anyhow::ensure!(
        config.reader_pool_size >= 1,
        "reader_pool_size must be >= 1"
    );
    Ok(config)
}
