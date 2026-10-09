//! 门店边缘节点入口：加载配置 → 打开数据库并迁移 → 启动 HTTP →
//! 收到 SIGTERM / SIGINT 后优雅关闭。

use std::path::PathBuf;

use anyhow::Context;
use boh_app::background::BackupTasks;
use boh_app::{AppState, http};
use boh_server::config::{Config, parse_config};
use boh_storage::backup::{Backup, BackupHealth};
use boh_storage::clock::Clock;
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

    let args: Vec<String> = std::env::args().skip(1).collect();
    let (operation, config_path) = cli(&args)?;
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

    let storage = boh_storage::open(&config.db_path, config.reader_pool_size)
        .context("open storage and migrate database")?;
    let result = run(operation, config, storage.writer, storage.readers).await;
    let closed = storage
        .writer_handle
        .shutdown()
        .await
        .context("close storage");
    match &closed {
        Ok(()) => tracing::info!("storage closed"),
        Err(error) if result.is_err() => {
            tracing::error!(error = %boh_storage::Diagnostic(error.as_ref()), "storage shutdown failed")
        }
        Err(_) => {}
    }
    // Preserve the original startup/serve error even if shutdown also fails.
    result?;
    closed?;
    Ok(())
}

async fn run(
    operation: Operation,
    config: Config,
    writer: boh_storage::Writer,
    readers: boh_storage::Readers,
) -> anyhow::Result<()> {
    let clock = Clock::system();
    let store_id = config.store_id;
    if operation == Operation::Init {
        boh_app::initialize_store(
            writer,
            store_id,
            clock,
            config.timezone,
            config.business_day_cutoff,
        )
        .await
        .context("initialize store")?;
        tracing::info!(store_id = %store_id, "store initialized");
        return Ok(());
    }
    writer
        .call(move |tx| boh_storage::store::verify(tx, store_id))
        .await
        .context("verify store identity; run init for a new node")?;
    if operation == Operation::Rebuild {
        tracing::info!(
            replayed = writer
                .rebuild_projections()
                .await
                .context("rebuild projections")?,
            "projections rebuilt"
        );
        return Ok(());
    }

    let mut sigterm = signal(SignalKind::terminate()).context("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).context("install SIGINT handler")?;
    let backup = Backup::prepare(
        &config.db_path,
        &config.backup_dir,
        store_id,
        config.backup_keep_count,
    )
    .context("prepare backup directory")?;
    let backup_health = BackupHealth::default();
    let listener = tokio::net::TcpListener::bind(config.listen_addr)
        .await
        .with_context(|| format!("bind {}", config.listen_addr))?;
    let tasks = BackupTasks::start(
        backup,
        clock.clone(),
        backup_health.clone(),
        config.timezone.clone(),
        config.closing_backup_time,
    )
    .context("start backup scheduler")?;
    let stop_backups = tasks.stop_handle();
    let shutdown = async move {
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
        stop_backups.stop();
        tracing::info!("shutdown signal received");
    };
    let app = http::router(AppState {
        writer,
        readers,
        clock,
        timezone: config.timezone,
        business_day_cutoff: config.business_day_cutoff,
        dev_actor_stub: config.dev_actor_stub,
        db_path: config.db_path,
        backup_health,
    });
    tracing::info!(addr = %config.listen_addr, "listening");
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .context("http server");

    tracing::info!("http stopped, draining backups");
    let stopped = tasks
        .shutdown()
        .await
        .context("drain and stop backup tasks");
    tracing::info!("backups stopped, draining writer");
    served?;
    stopped?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Serve,
    Init,
    Rebuild,
}

fn cli(args: &[String]) -> anyhow::Result<(Operation, PathBuf)> {
    let (operation, path) = match args {
        [] => (Operation::Serve, DEFAULT_CONFIG_PATH),
        [command] if command == "init" => (Operation::Init, DEFAULT_CONFIG_PATH),
        [command] if command == "rebuild-projections" => (Operation::Rebuild, DEFAULT_CONFIG_PATH),
        [path] => (Operation::Serve, path.as_str()),
        [command, path] if command == "init" => (Operation::Init, path.as_str()),
        [command, path] if command == "rebuild-projections" => (Operation::Rebuild, path.as_str()),
        _ => anyhow::bail!("usage: boh-server [init | rebuild-projections] [config file]"),
    };
    Ok((operation, PathBuf::from(path)))
}
