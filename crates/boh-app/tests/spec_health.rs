//! 锁定测试：/health 的 data 契约。
//! 规则见 AGENTS.md「HTTP 约定」/health 与 docs/domain.md「时间」时钟异常；备份字段随备份结果变化的用例见 spec_backup.rs。

mod spec_support;

use std::fs;
use std::path::Path;

use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use serde_json::{Value, json};
use spec_support::{MANAGER, assert_success, health};

const NOW: UnixMillis = UnixMillis(1_791_248_400_000); // 2026-10-06 09:00 +08:00

fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209ad{n:04x}")
}

#[allow(clippy::unwrap_used)] // 测试夹具：前置步骤失败时后续断言没有意义，直接终止测试。
fn wal_size(db_path: &Path) -> u64 {
    let mut wal = db_path.as_os_str().to_owned();
    wal.push("-wal");
    fs::metadata(wal).unwrap().len()
}

#[allow(clippy::unwrap_used)] // 测试夹具：前置步骤失败时后续断言没有意义，直接终止测试。
async fn create_equipment(router: &axum::Router, n: u16) {
    let reply = spec_support::post(
        router,
        "/api/v1/equipment",
        Some(&MANAGER),
        &json!({
            "command_id": cmd(n), "code": format!("F{n}"), "name": "Walk-in",
            "equipment_type": "FRIDGE", "active": true,
        }),
    )
    .await
    .unwrap();
    assert_success(&reply);
}

// 「data 的字段」恰好是这七个；新节点：账本为空、本进程还没有备份结果，状态 ok。不需要身份。
#[tokio::test]
async fn new_node_reports_ok_and_unknown_backup_state() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let config =
        spec_support::node_config(&dir.path().join("backups"), 3, "Asia/Shanghai", "23:30");
    let node = spec_support::node(&db_path, ManualClock::new(NOW).clock(), config)
        .await
        .unwrap();

    let data = health(&node.router).await;

    let mut keys: Vec<&str> = data
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "clock_regression_ms",
            "last_backup_failed_at",
            "last_backup_ok_at",
            "last_backup_seq",
            "schema_version",
            "status",
            "wal_size_bytes",
        ]
    );
    assert_eq!(data["status"], json!("ok"));
    assert_eq!(
        data["schema_version"],
        json!(boh_storage::LATEST_SCHEMA_VERSION)
    );
    assert_eq!(data["clock_regression_ms"], json!(0));
    assert_eq!(data["last_backup_ok_at"], Value::Null);
    assert_eq!(data["last_backup_seq"], Value::Null);
    assert_eq!(data["last_backup_failed_at"], Value::Null);
    assert_eq!(data["wal_size_bytes"], json!(wal_size(&db_path)));
    node.shutdown().await.unwrap();
}

// 「clock_regression_ms = max(0, max(store_events.recorded_at) − now)，账本为空时为 0；超过 300000 为 degraded」：
// 恰好 300000 仍为 ok。degraded 不阻止写入；回拨后写入的记录不降低 max(recorded_at)，指标保持到时钟追上最晚的记录。
#[tokio::test]
async fn clock_regression_measures_how_far_the_latest_record_is_ahead() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(NOW);
    let router = spec_support::router(&dir.path().join("boh.db"), clock.clock()).unwrap();

    clock.set(UnixMillis(NOW.0 - 3_600_000));
    let data = health(&router).await;
    assert_eq!(data["clock_regression_ms"], json!(0), "empty ledger");

    clock.set(NOW);
    create_equipment(&router, 1).await;
    for (now, regression, status) in [
        (NOW.0 + 5_000, 0, "ok"),
        (NOW.0, 0, "ok"),
        (NOW.0 - 300_000, 300_000, "ok"),
        (NOW.0 - 300_001, 300_001, "degraded"),
    ] {
        clock.set(UnixMillis(now));
        let data = health(&router).await;
        assert_eq!(data["clock_regression_ms"], json!(regression), "{now}");
        assert_eq!(data["status"], json!(status), "{now}");
    }

    create_equipment(&router, 2).await;
    let data = health(&router).await;
    assert_eq!(data["clock_regression_ms"], json!(300_001));
    assert_eq!(data["status"], json!("degraded"));

    clock.set(NOW);
    let data = health(&router).await;
    assert_eq!(data["clock_regression_ms"], json!(0));
    assert_eq!(data["status"], json!("ok"));
}

// 「wal_size_bytes：<数据库文件>-wal 的大小」：现场读取，写入后与文件大小一致。test_router 不启动后台任务，备份字段为 null。
#[tokio::test]
async fn wal_size_is_read_from_the_wal_file() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let router = spec_support::router(&db_path, ManualClock::new(NOW).clock()).unwrap();
    for n in 1..=3 {
        create_equipment(&router, n).await;
    }

    let data = health(&router).await;

    let size = wal_size(&db_path);
    assert!(size > 0);
    assert_eq!(data["wal_size_bytes"], json!(size));
    assert_eq!(data["last_backup_ok_at"], Value::Null);
    assert_eq!(data["last_backup_failed_at"], Value::Null);
    assert_eq!(data["status"], json!("ok"));
}
