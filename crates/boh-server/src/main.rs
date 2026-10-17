//! 门店边缘节点入口：加载配置 → 打开数据库并迁移 → 启动 HTTP →
//! 收到 SIGTERM / SIGINT 后优雅关闭。

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use boh_app::background::BackupTasks;
use boh_app::clock_sync::{ClockSignal, ClockTasks, ensure_synchronized};
use boh_app::{AppState, InvariantCheckHealth, http};
use boh_server::config::{Config, parse_config};
use boh_storage::backup::{Backup, BackupHealth};
use boh_storage::clock::Clock;
use tokio::signal::unix::{SignalKind, signal};
use tracing_subscriber::EnvFilter;

const DEFAULT_CONFIG_PATH: &str = "/etc/boh/config.toml";

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    run_runtime(main_async())
}

fn run_runtime(
    future: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build runtime")?;
    let result = runtime.block_on(future);
    // HTTP, backup/check tasks, state persistence and the writer checkpoint have
    // already drained, including on init failure. Only abandoned DNS may remain.
    runtime.shutdown_timeout(Duration::from_millis(100));
    result
}

async fn main_async() -> anyhow::Result<()> {
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
        readers
            .call(boh_storage::store::ensure_uninitialized)
            .await
            .context("check store before initialization")?;
        let clock_signal = ensure_synchronized(&config.db_path, clock.clone(), config.ntp_servers)
            .await
            .context("synchronize node clock before initialization")?;
        boh_app::initialize_store(
            writer,
            store_id,
            clock,
            config.timezone,
            config.business_day_cutoff,
            clock_signal,
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
        let replayed = writer
            .rebuild_projections()
            .await
            .context("rebuild projections")?;
        tracing::info!(replayed, "projections rebuilt");
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
    let invariant_health = InvariantCheckHealth::default();
    let listener = tokio::net::TcpListener::bind(config.listen_addr)
        .await
        .with_context(|| format!("bind {}", config.listen_addr))?;
    let tasks = BackupTasks::start(
        backup,
        clock.clone(),
        backup_health.clone(),
        config.timezone.clone(),
        config.closing_backup_time,
        readers.clone(),
        invariant_health.clone(),
    )
    .context("start backup scheduler and submit startup invariant check")?;
    let clock_signal = ClockSignal::load(&config.db_path).await;
    let clock_tasks = ClockTasks::start(
        &config.db_path,
        clock.clone(),
        config.ntp_servers,
        clock_signal.clone(),
    )
    .await;
    let stop_backups = tasks.stop_handle();
    let stop_clock = clock_tasks.stop_handle();
    let signal_stop_backups = stop_backups.clone();
    let signal_stop_clock = stop_clock.clone();
    let shutdown = async move {
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
        signal_stop_backups.stop();
        signal_stop_clock.stop();
        tracing::info!("shutdown signal received");
    };
    let app = http::router(AppState {
        writer,
        readers,
        clock,
        clock_signal,
        timezone: config.timezone,
        business_day_cutoff: config.business_day_cutoff,
        dev_actor_stub: config.dev_actor_stub,
        db_path: config.db_path,
        backup_health,
        invariant_health,
    });
    tracing::info!(addr = %config.listen_addr, "listening");
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .context("http server");

    stop_backups.stop();
    stop_clock.stop();
    tracing::info!("http stopped, draining background tasks");
    let (backups_stopped, clock_stopped) = tokio::join!(tasks.shutdown(), clock_tasks.shutdown());
    tracing::info!("background tasks stopped, draining writer");
    served?;
    backups_stopped.context("drain and stop backups, closing flows and invariant checks")?;
    clock_stopped.context("drain clock state persistence and stop clock checks")?;
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
        [path] if path != "rebuild-projections" => (Operation::Serve, path.as_str()),
        [command, path] if command == "init" => (Operation::Init, path.as_str()),
        [command, path] if command == "rebuild-projections" => (Operation::Rebuild, path.as_str()),
        _ => anyhow::bail!("usage: boh-server [init | rebuild-projections] [config file]"),
    };
    Ok((operation, PathBuf::from(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_drains_awaited_work_but_leaves_abandoned_blocking_tasks() {
        for fail in [false, true] {
            let (abandoned_release, abandoned_wait) = std::sync::mpsc::channel();
            let (durable_release, durable_wait) = std::sync::mpsc::channel();
            let (ready, started) = std::sync::mpsc::channel();
            let (finished, completion) = std::sync::mpsc::channel();
            let thread = std::thread::spawn(move || {
                let result = run_runtime(async move {
                    let (dns_started, dns_ready) = tokio::sync::oneshot::channel();
                    // Models getaddrinfo after its async query has been cancelled.
                    drop(tokio::task::spawn_blocking(move || {
                        dns_started.send(()).unwrap();
                        abandoned_wait.recv().unwrap();
                    }));
                    dns_ready.await.unwrap();
                    tokio::task::spawn_blocking(move || {
                        ready.send(()).unwrap();
                        durable_wait.recv().unwrap();
                    })
                    .await
                    .unwrap();
                    if fail {
                        anyhow::bail!("init failed");
                    }
                    Ok(())
                });
                finished.send(result).unwrap();
            });
            started.recv_timeout(Duration::from_secs(2)).unwrap();
            // A short runtime timeout must not interrupt awaited persistence.
            assert!(matches!(
                completion.recv_timeout(Duration::from_millis(200)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));
            durable_release.send(()).unwrap();
            let result = completion.recv_timeout(Duration::from_secs(2));
            // Release the simulated DNS thread even if the deadline assertion fails.
            abandoned_release.send(()).unwrap();
            thread.join().unwrap();
            let result = result.expect("abandoned DNS must not hold runtime shutdown");
            assert_eq!(result.is_err(), fail);
            if let Err(error) = result {
                assert_eq!(error.to_string(), "init failed");
            }
        }
    }

    #[tokio::test]
    async fn repeated_init_rejects_before_ntp_or_state_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("boh.db");
        let state = dir.path().join("boh.db-clock-sync");
        std::fs::write(&state, "1\n").unwrap();
        let ntp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        ntp.set_nonblocking(true).unwrap();
        let mut config = parse_config(include_str!("../../../deploy/config.example.toml")).unwrap();
        config.db_path = db.clone();
        config.ntp_servers = vec![
            boh_server::config::NtpServer::parse(&ntp.local_addr().unwrap().to_string()).unwrap(),
        ];
        let storage = boh_storage::open(&db, std::num::NonZeroUsize::MIN).unwrap();
        let store_id = config.store_id;
        storage
            .writer
            .call(move |tx| boh_storage::store::initialize(tx, store_id, boh_domain::UnixMillis(1)))
            .await
            .unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run(Operation::Init, config, storage.writer, storage.readers),
        )
        .await;
        storage.writer_handle.shutdown().await.unwrap();
        let error = result
            .expect("already initialized must not wait for NTP")
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<boh_storage::StorageError>(),
            Some(boh_storage::StorageError::StoreAlreadyInitialized)
        ));
        assert_eq!(std::fs::read(&state).unwrap(), b"1\n");
        let mut buf = [0; 48];
        assert_eq!(
            ntp.recv_from(&mut buf).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn rebuild_requires_exactly_one_explicit_config_path() {
        for args in [
            vec!["rebuild-projections"],
            vec!["rebuild-projections", "config.toml", "extra"],
            vec!["rebuild-projection", "config.toml"],
        ] {
            let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
            assert!(cli(&args).is_err(), "{args:?}");
        }
        let args = ["rebuild-projections".to_owned(), "config.toml".to_owned()];
        assert!(matches!(
            cli(&args),
            Ok((Operation::Rebuild, path)) if path == std::path::Path::new("config.toml")
        ));
    }
}
