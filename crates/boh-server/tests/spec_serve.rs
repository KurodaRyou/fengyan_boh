//! 锁定测试：服务进程的启动核对、停止信号与优雅关闭。
//! 规则见 AGENTS.md「Rust」优雅关闭顺序、docs/domain.md「时间」门店节点校时；接口见 docs/interfaces.md「boh-server：服务进程」。
//! 以子进程运行 `boh-server <配置文件>`，信号经系统 `kill` 命令发送。备份或自检进行中收到信号时执行完再退出，
//! 由开发顺序第 6 步的进程级黑盒验收提供证据。

mod spec_support;

use std::ffi::OsStr;
use std::time::{Duration, Instant};

use spec_support::ntp::{FakeNtp, Reply};
use spec_support::{
    MANAGER_HEADERS, Paths, http, paths, reader, run, signal, spawn_server, wait_exit, wait_ready,
};

fn init(paths: &Paths) {
    assert!(run(&[OsStr::new("init"), paths.config.as_os_str()]).success());
}

/// 新建一台设备（主数据写命令），返回状态码。
fn create_equipment(paths: &Paths, n: u16) -> u16 {
    let body = format!(
        r#"{{"command_id":"01890a5d-ac96-774b-bcce-b30209f0{n:04x}","code":"F{n}","name":"Walk-in","equipment_type":"FRIDGE","active":true}}"#
    );
    http(
        paths.listen,
        "POST",
        "/api/v1/equipment",
        &MANAGER_HEADERS,
        Some(&body),
    )
    .map_or(0, |(status, _)| status)
}

#[allow(clippy::unwrap_used)] // 测试辅助：只读查询失败说明写入没有持久化。
fn equipment_events(paths: &Paths) -> i64 {
    reader(&paths.db)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM store_events WHERE aggregate_type = 'EQUIPMENT'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

// AGENTS「Rust」优雅关闭：启动 → 写入一条记录 → 发 SIGTERM（或 SIGINT）→ 退出码 0；退出后 WAL 已截断
// （<db>-wal 不存在或大小为 0），写入的记录已持久化。
// domain「时间」：启动时读取有效的状态文件即接受写入；「启动时核对一次」——NTP 收到请求，成功后状态文件改写为更新的时间。
#[tokio::test]
async fn stop_signals_shut_down_gracefully() {
    for name in ["TERM", "INT"] {
        let ntp = FakeNtp::system(Reply::Offset(0));
        let paths = paths(&[&ntp]);
        let previous = paths.write_clock_sync(3_600_000);
        init(&paths);
        assert_eq!(ntp.requests(), 0, "init with a valid state file");

        let mut child = spawn_server(&paths.config);
        wait_ready(&mut child, paths.listen).await;
        ntp.wait_requests(1).await;
        assert_eq!(create_equipment(&paths, 1), 200, "{name}");

        // 启动核对成功后状态文件改写为收到回复时的时间（晚于原来的值）。
        let mut updated = None;
        for _ in 0..500 {
            if let Some(content) = paths.clock_sync_file()
                && content != format!("{previous}\n")
            {
                updated = Some(content);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let updated = updated.unwrap_or_else(|| panic!("{name}: clock sync file not updated"));
        let value: i64 = updated
            .trim_end_matches('\n')
            .parse()
            .unwrap_or_else(|_| panic!("{updated:?}"));
        assert!(value > previous, "{name}");

        signal(&child, name);
        let status = wait_exit(&mut child, Duration::from_secs(20)).await;
        assert_eq!(status.code(), Some(0), "{name}");
        assert!(
            matches!(paths.wal_size(), None | Some(0)),
            "{name}: {:?}",
            paths.wal_size()
        );
        assert_eq!(equipment_events(&paths), 1, "{name}");
    }
}

// AGENTS「Rust」优雅关闭：「正在等待的 NTP 查询直接取消」。NTP 服务不回复：收到启动核对的请求后立即发 SIGTERM，
// 进程在 3 秒内以 0 退出，不等满 5 秒的查询超时。信号有效时写入照常（核对未完成不影响写入）。
#[tokio::test]
async fn sigterm_cancels_a_pending_ntp_query() {
    let ntp = FakeNtp::system(Reply::Silent);
    let paths = paths(&[&ntp]);
    paths.write_clock_sync(60_000);
    init(&paths);

    let mut child = spawn_server(&paths.config);
    wait_ready(&mut child, paths.listen).await;
    ntp.wait_requests(1).await;
    assert_eq!(create_equipment(&paths, 1), 200);

    let sent = Instant::now();
    signal(&child, "TERM");
    let status = wait_exit(&mut child, Duration::from_secs(20)).await;
    assert_eq!(status.code(), Some(0));
    assert!(
        sent.elapsed() < Duration::from_secs(3),
        "exit took {:?}",
        sent.elapsed()
    );
    assert_eq!(equipment_events(&paths), 1);
}

// domain「时间」：没有状态文件时服务照常启动（不拒绝启动），写命令 503 CLOCK_NOT_SYNCED，/health 的 clock_synced 为 false；
// 启动核对成功（偏差 0）后自动恢复写入，状态文件写入。init 之后删除状态文件来构造这种情况。
#[tokio::test]
async fn the_server_starts_without_a_signal_and_recovers_after_a_successful_check() {
    let ntp = FakeNtp::system(Reply::Silent);
    let paths = paths(&[&ntp]);
    paths.write_clock_sync(60_000);
    init(&paths);
    std::fs::remove_file(paths.clock_sync()).ok();

    let mut child = spawn_server(&paths.config);
    wait_ready(&mut child, paths.listen).await;
    ntp.wait_requests(1).await;
    assert_eq!(create_equipment(&paths, 1), 503);
    let health = http(paths.listen, "GET", "/health", &[], None).map(|(_, body)| body);
    assert!(
        health
            .as_deref()
            .is_some_and(|body| body.contains(r#""clock_synced":false"#)),
        "{health:?}"
    );

    // 查询超时（5 秒）后，信号无效时 1 分钟后才再核对；这里换成新的进程更快：停掉，改为正常回复，再启动。
    signal(&child, "TERM");
    assert_eq!(
        wait_exit(&mut child, Duration::from_secs(20)).await.code(),
        Some(0)
    );
    ntp.set_default(Reply::Offset(0));
    let mut child = spawn_server(&paths.config);
    wait_ready(&mut child, paths.listen).await;
    let mut status = 0;
    for _ in 0..500 {
        status = create_equipment(&paths, 2);
        if status == 200 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(status, 200);
    assert!(paths.clock_sync_file().is_some());
    signal(&child, "TERM");
    assert_eq!(
        wait_exit(&mut child, Duration::from_secs(20)).await.code(),
        Some(0)
    );
    assert_eq!(equipment_events(&paths), 1);
}

// domain「时间」门店节点校时：「ntp_servers 缺失、为空或某项无法解析时拒绝启动」——服务进程以非 0 退出，不监听端口。
#[tokio::test]
#[allow(clippy::unwrap_used)] // 测试夹具：改写配置失败时测试无法进行。
async fn the_server_refuses_to_start_with_invalid_ntp_servers() {
    let ntp = FakeNtp::system(Reply::Offset(0));
    let paths = paths(&[&ntp]);
    paths.write_clock_sync(60_000);
    init(&paths);
    let original = std::fs::read_to_string(&paths.config).unwrap();
    let without: Vec<&str> = original
        .lines()
        .filter(|line| !line.starts_with("ntp_servers"))
        .collect();
    for replacement in [
        None,
        Some("ntp_servers = []"),
        Some(r#"ntp_servers = ["::1"]"#),
    ] {
        let mut lines = without.clone();
        if let Some(line) = replacement {
            lines.push(line);
        }
        std::fs::write(&paths.config, lines.join("\n") + "\n").unwrap();
        let mut child = spawn_server(&paths.config);
        let status = wait_exit(&mut child, Duration::from_secs(20)).await;
        assert!(!status.success(), "{replacement:?}");
        assert!(
            http(paths.listen, "GET", "/health", &[], None).is_none(),
            "{replacement:?}"
        );
    }
    assert_eq!(ntp.requests(), 0);
}
