//! 锁定测试：备份的触发、排队、文件名与保留、失败报告，以及恢复演练。
//! 规则见 AGENTS.md「备份」与「HTTP 约定」/health，接口见 docs/interfaces.md「boh-app：带后台任务的测试节点」。
//! 只经 test_node 的 Router、备份目录中的文件、对数据库文件的只读 SQL 和 boh_storage::testing 访问系统。

mod spec_support;

use std::fs;
use std::path::PathBuf;
use std::pin::pin;
use std::time::Duration;

use boh_app::TestNode;
use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use boh_storage::rusqlite::types::Value as SqlValue;
use boh_storage::rusqlite::{self, Connection};
use serde_json::{Value, json};
use spec_support::{
    MANAGER, STAFF, TEST_STORE_ID, assert_success, backup_numbers, backups, file_names, settle,
    wait_for_health, wait_until,
};
use tempfile::TempDir;

/// 2026-10-06 00:00 +08:00（Asia/Shanghai，UTC 偏移是整小时，UTC 整点即当地整点）。
const SH_DAY: i64 = 1_791_216_000_000;
const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;
const SH: &str = "Asia/Shanghai";
const OTHER_STORE: &str = "01890a5d-ac96-774b-bcce-b302099a9050";

/// 2026-10-06 当地 h:m（h 可以是 24，表示次日 00:00）。
fn local(h: i64, m: i64) -> UnixMillis {
    UnixMillis(SH_DAY + h * HOUR + m * MINUTE)
}

fn just_before(time: UnixMillis) -> UnixMillis {
    UnixMillis(time.0 - 1)
}

fn uuid(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209ab{n:04x}")
}

fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209ac{n:04x}")
}

struct Fixture {
    dir: TempDir,
    clock: ManualClock,
}

#[allow(clippy::unwrap_used)] // 测试夹具：建目录、读目录或启动节点失败时后续断言没有意义，直接终止测试。
impl Fixture {
    fn new(start: UnixMillis) -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            clock: ManualClock::new(start),
        }
    }

    fn db(&self) -> PathBuf {
        self.dir.path().join("boh.db")
    }

    fn backups(&self) -> PathBuf {
        self.dir.path().join("backups")
    }

    async fn start(&self, keep: u32, timezone: &str, closing: &str) -> TestNode {
        let config = spec_support::node_config(&self.backups(), keep, timezone, closing);
        spec_support::node(&self.db(), self.clock.clock(), config)
            .await
            .unwrap()
    }

    fn place(&self, name: &str) {
        fs::create_dir_all(self.backups()).unwrap();
        fs::write(self.backups().join(name), b"not a database").unwrap();
    }

    fn numbers(&self) -> Vec<u64> {
        backup_numbers(&self.backups()).unwrap()
    }

    async fn wait_numbers(&self, expected: &[u64]) {
        let dir = self.backups();
        wait_until(&format!("backup numbers {expected:?}"), || {
            backup_numbers(&dir).unwrap() == expected
        })
        .await;
    }

    fn times(&self) -> Vec<String> {
        backups(&self.backups())
            .unwrap()
            .into_iter()
            .map(|file| file.time)
            .collect()
    }

    fn temporary_files(&self) -> Vec<String> {
        file_names(&self.backups())
            .unwrap()
            .into_iter()
            .filter(|name| name.starts_with("tmp-"))
            .collect()
    }
}

fn own_final(time: &str, number: &str, uuid: &str) -> String {
    format!("boh-{TEST_STORE_ID}-{time}-{number}-{uuid}.db")
}

#[allow(clippy::unwrap_used)] // 测试夹具：前置步骤失败时后续断言没有意义，直接终止测试。
async fn create_equipment(router: &axum::Router, command_id: &str, code: &str) -> String {
    let reply = spec_support::post(
        router,
        "/api/v1/equipment",
        Some(&MANAGER),
        &json!({
            "command_id": command_id, "code": code, "name": "Walk-in",
            "equipment_type": "FRIDGE", "active": true,
        }),
    )
    .await
    .unwrap();
    assert_success(&reply)["equipment"]["equipment_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn ms(value: &Value) -> Option<i64> {
    value.as_i64()
}

// ---------------------------------------------------------------- 启动与目录

// 「启动时目录不存在则创建（含上级目录）；创建失败或路径不是目录，拒绝启动」；时区或闭店时刻非法同样拒绝。
#[tokio::test]
async fn backup_dir_is_created_and_must_be_a_directory() {
    let fixture = Fixture::new(local(12, 30));
    let nested = fixture.dir.path().join("a").join("b").join("backups");
    let config = spec_support::node_config(&nested, 3, SH, "23:30");
    let node = spec_support::node(&fixture.db(), fixture.clock.clock(), config)
        .await
        .unwrap();
    assert!(nested.is_dir());
    node.shutdown().await.unwrap();

    let not_a_dir = fixture.dir.path().join("plain-file");
    fs::write(&not_a_dir, b"x").unwrap();
    for (n, config) in [
        spec_support::node_config(&not_a_dir, 3, SH, "23:30"),
        spec_support::node_config(&nested, 3, "Asia/Shangai", "23:30"),
        spec_support::node_config(&nested, 3, SH, "24:00"),
    ]
    .into_iter()
    .enumerate()
    {
        let db = fixture.dir.path().join(format!("rejected-{n}.db"));
        assert!(
            spec_support::node(&db, fixture.clock.clock(), config)
                .await
                .is_err(),
            "config {n}"
        );
    }
}

// ---------------------------------------------------------------- 触发

// 「启动时不立即备份」（即使恰好在整点启动）；「定时备份在每个 UTC 整点一次」：整点前 1ms 不触发，到点触发。
// 文件名的时间是备份开始时的 now()（UTC），序号从 1 开始；/health 报告完成时间与快照的 n。
#[tokio::test]
async fn hourly_backup_fires_on_the_hour_but_not_at_startup() {
    let fixture = Fixture::new(local(13, 0));
    let node = fixture.start(3, SH, "23:30").await;
    create_equipment(&node.router, &cmd(1), "F1").await;
    settle().await;
    assert!(file_names(&fixture.backups()).unwrap().is_empty());

    fixture.clock.set(just_before(local(14, 0)));
    settle().await;
    assert!(fixture.numbers().is_empty());

    fixture.clock.set(local(14, 0));
    fixture.wait_numbers(&[1]).await;
    let files = backups(&fixture.backups()).unwrap();
    assert_eq!(files[0].time, "20261006T060000Z");
    assert_eq!(
        file_names(&fixture.backups()).unwrap(),
        [files[0].name.clone()]
    );
    let data = wait_for_health(&node.router, "first backup", |d| {
        !d["last_backup_ok_at"].is_null()
    })
    .await;
    assert_eq!(ms(&data["last_backup_ok_at"]), Some(local(14, 0).0));
    assert_eq!(data["last_backup_seq"], json!(1));
    assert_eq!(data["last_backup_failed_at"], Value::Null);
    assert_eq!(data["status"], json!("ok"));
    node.shutdown().await.unwrap();
}

// 「按 UTC 而不是当地钟点」：Asia/Kolkata 偏移 +05:30，当地整点（UTC :30）不触发，UTC 整点触发。
#[tokio::test]
async fn hourly_backup_follows_utc_hours_in_a_half_hour_offset_zone() {
    let utc_12_10 = 1_791_288_600_000; // 2026-10-06 12:10Z = 17:40 +05:30
    let fixture = Fixture::new(UnixMillis(utc_12_10));
    let node = fixture.start(3, "Asia/Kolkata", "23:30").await;
    fixture.clock.set(UnixMillis(utc_12_10 + 20 * MINUTE)); // 当地 18:00
    settle().await;
    assert!(fixture.numbers().is_empty());
    fixture.clock.set(UnixMillis(utc_12_10 + 50 * MINUTE - 1));
    settle().await;
    assert!(fixture.numbers().is_empty());
    fixture.clock.set(UnixMillis(utc_12_10 + 50 * MINUTE)); // 13:00Z
    fixture.wait_numbers(&[1]).await;
    assert_eq!(fixture.times(), ["20261006T130000Z"]);
    node.shutdown().await.unwrap();
}

// 「时钟一次向前跨过多个触发时刻，每种触发只触发一次，不补跑」；之后照常等下一个整点。
#[tokio::test]
async fn jumping_over_several_hours_backs_up_once() {
    let fixture = Fixture::new(local(13, 30));
    let node = fixture.start(5, SH, "23:30").await;
    fixture.clock.set(local(17, 30));
    fixture.wait_numbers(&[1]).await;
    settle().await;
    assert_eq!(fixture.numbers(), [1]);
    assert_eq!(fixture.times(), ["20261006T093000Z"]);

    fixture.clock.set(just_before(local(18, 0)));
    settle().await;
    assert_eq!(fixture.numbers(), [1]);
    fixture.clock.set(local(18, 0));
    fixture.wait_numbers(&[1, 2]).await;
    node.shutdown().await.unwrap();
}

// 「闭店备份在每天门店当地时间 closing_backup_time 一次」：前 1ms 不触发，到点触发；之后下一个整点照常。
#[tokio::test]
async fn closing_backup_fires_at_the_local_closing_time() {
    let fixture = Fixture::new(local(23, 10));
    let node = fixture.start(5, SH, "23:30").await;
    fixture.clock.set(just_before(local(23, 30)));
    settle().await;
    assert!(fixture.numbers().is_empty());
    fixture.clock.set(local(23, 30));
    fixture.wait_numbers(&[1]).await;
    assert_eq!(fixture.times(), ["20261006T153000Z"]);

    fixture.clock.set(just_before(local(24, 0)));
    settle().await;
    assert_eq!(fixture.numbers(), [1]);
    fixture.clock.set(local(24, 0));
    fixture.wait_numbers(&[1, 2]).await;
    assert_eq!(fixture.times(), ["20261006T153000Z", "20261006T160000Z"]);
    node.shutdown().await.unwrap();
}

// 「两种触发同时到期时各执行一次」：闭店时刻恰是整点。
#[tokio::test]
async fn hourly_and_closing_due_together_back_up_twice() {
    let fixture = Fixture::new(local(14, 30));
    let node = fixture.start(5, SH, "15:00").await;
    fixture.clock.set(local(15, 0));
    fixture.wait_numbers(&[1, 2]).await;
    settle().await;
    assert_eq!(fixture.numbers(), [1, 2]);
    assert_eq!(fixture.times(), ["20261006T070000Z", "20261006T070000Z"]);
    node.shutdown().await.unwrap();
}

// 闭店时刻按门店时区与夏令时换算：America/New_York 2026-03-08 跳过 02:00–03:00，02:30 顺延为 03:30 EDT（07:30Z）；
// 定时备份仍在 UTC 整点（07:00Z = 03:00 EDT）。
#[tokio::test]
async fn closing_backup_follows_daylight_saving_time_in_the_store_timezone() {
    let utc_06_00 = 1_772_949_600_000; // 2026-03-08 06:00Z = 01:00 EST
    let fixture = Fixture::new(UnixMillis(utc_06_00));
    let node = fixture.start(5, "America/New_York", "02:30").await;
    fixture.clock.set(UnixMillis(utc_06_00 + HOUR));
    fixture.wait_numbers(&[1]).await;
    fixture
        .clock
        .set(UnixMillis(utc_06_00 + HOUR + 30 * MINUTE - 1));
    settle().await;
    assert_eq!(fixture.numbers(), [1]);
    fixture
        .clock
        .set(UnixMillis(utc_06_00 + HOUR + 30 * MINUTE));
    fixture.wait_numbers(&[1, 2]).await;
    assert_eq!(fixture.times(), ["20260308T070000Z", "20260308T073000Z"]);
    node.shutdown().await.unwrap();
}

// 「时钟回拨后，按回拨后的时间重新计算等待的触发时刻」：在同一个小时内小幅回拨不改变下一个整点；
// 回拨到更早的小时后，在新的下一个整点触发，而不是等到回拨前算出的时刻。
#[tokio::test]
async fn clock_rewind_recomputes_the_next_trigger() {
    let fixture = Fixture::new(local(12, 30));
    let node = fixture.start(5, SH, "23:30").await;
    fixture.clock.set(local(12, 10));
    settle().await;
    fixture.clock.set(just_before(local(13, 0)));
    settle().await;
    assert!(fixture.numbers().is_empty());
    fixture.clock.set(local(13, 0));
    fixture.wait_numbers(&[1]).await;

    fixture.clock.set(local(10, 30));
    settle().await;
    fixture.clock.set(just_before(local(11, 0)));
    settle().await;
    assert_eq!(fixture.numbers(), [1]);
    fixture.clock.set(local(11, 0));
    fixture.wait_numbers(&[1, 2]).await;
    assert_eq!(fixture.times(), ["20261006T050000Z", "20261006T030000Z"]);
    node.shutdown().await.unwrap();
}

// 「两种触发共用串行的备份任务入口……同一种触发最多排队一次」：备份停在执行中时，整点到期两次、闭店到期一次，
// 释放后只再执行两次（整点一次、闭店一次），共 3 份，序号连续。
#[tokio::test]
async fn queued_triggers_run_one_at_a_time_and_each_kind_queues_once() {
    let fixture = Fixture::new(local(12, 30));
    let node = fixture.start(5, SH, "15:30").await;
    let hold = node.hold_backups();
    fixture.clock.set(local(13, 0));
    tokio::time::timeout(Duration::from_secs(10), hold.started())
        .await
        .unwrap();
    for time in [local(14, 0), local(15, 0), local(15, 30)] {
        fixture.clock.set(time);
        settle().await;
    }
    assert!(fixture.numbers().is_empty());

    drop(hold);
    fixture.wait_numbers(&[1, 2, 3]).await;
    settle().await;
    assert_eq!(fixture.numbers(), [1, 2, 3]);
    assert_eq!(
        fixture.times(),
        ["20261006T050000Z", "20261006T073000Z", "20261006T073000Z"]
    );
    let data = wait_for_health(&node.router, "third backup", |d| {
        ms(&d["last_backup_ok_at"]) == Some(local(15, 30).0)
    })
    .await;
    assert_eq!(data["status"], json!("ok"));
    node.shutdown().await.unwrap();
}

// 「关闭：不再接受新的触发；先执行完正在进行和已排队的备份，再关写线程」：shutdown 等到暂停释放、队列执行完才返回；
// 关闭开始后到期的整点不再排队；返回后没有残留的临时文件。
#[tokio::test]
async fn shutdown_runs_the_running_and_queued_backups_first() {
    let fixture = Fixture::new(local(12, 30));
    let node = fixture.start(5, SH, "14:30").await;
    let hold = node.hold_backups();
    fixture.clock.set(local(13, 0));
    tokio::time::timeout(Duration::from_secs(10), hold.started())
        .await
        .unwrap();
    fixture.clock.set(local(14, 0));
    settle().await;
    fixture.clock.set(local(14, 30));
    settle().await;

    let mut shutdown = pin!(node.shutdown());
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut shutdown)
            .await
            .is_err()
    );
    fixture.clock.set(local(15, 0));
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut shutdown)
            .await
            .is_err()
    );
    assert!(fixture.numbers().is_empty());

    drop(hold);
    tokio::time::timeout(Duration::from_secs(10), shutdown)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fixture.numbers(), [1, 2, 3]);
    assert!(fixture.temporary_files().is_empty());
    settle().await;
    assert_eq!(fixture.numbers(), [1, 2, 3]);
}

// ---------------------------------------------------------------- 文件名与保留

// 「保留策略」：按序号从大到小保留 N 份，第 N+1 份成功后删除序号最小的一份。
// 「识别」：只处理本店、完全符合格式的最终文件和临时文件；本店残留的临时文件在下一次备份开始时删除；
// 其他门店的、格式相近但不匹配的文件不计入序号、不删除。
#[tokio::test]
async fn retention_keeps_the_newest_numbers_and_leaves_other_files_alone() {
    let fixture = Fixture::new(local(12, 30));
    let untouched = [
        "notes.txt".to_owned(),
        format!("boh-{OTHER_STORE}-20260101T000000Z-99-{}.db", uuid(1)),
        format!("tmp-{OTHER_STORE}-{}.db", uuid(2)),
        own_final("20260101T000000Z", "07", &uuid(3)), // 序号有前导零
        own_final("20260101T000000Z", "0", &uuid(4)),
        own_final("20260101T000000Z", "18446744073709551616", &uuid(5)), // 超出 u64
        own_final("20260101T000000Z", "50", &uuid(6).to_uppercase()),
        own_final(
            "20260101T000000Z",
            "51",
            "01890a5d-ac96-474b-bcce-b30209ab0007",
        ), // v4
        format!("boh-{TEST_STORE_ID}-2026-01-01-52-{}.db", uuid(8)),
        format!("{}.bak", own_final("20260101T000000Z", "53", &uuid(9))),
    ];
    for name in &untouched {
        fixture.place(name);
    }
    let residual = format!("tmp-{TEST_STORE_ID}-{}.db", uuid(10));
    fixture.place(&residual);

    let node = fixture.start(3, SH, "23:30").await;
    for (hour, expected) in [
        (13, vec![1]),
        (14, vec![1, 2]),
        (15, vec![1, 2, 3]),
        (16, vec![2, 3, 4]),
    ] {
        fixture.clock.set(local(hour, 0));
        fixture.wait_numbers(&expected).await;
    }
    settle().await;
    node.shutdown().await.unwrap();

    let mut expected: Vec<String> = untouched.to_vec();
    expected.extend(
        backups(&fixture.backups())
            .unwrap()
            .into_iter()
            .map(|file| file.name),
    );
    expected.sort();
    assert_eq!(file_names(&fixture.backups()).unwrap(), expected);
    assert_eq!(fixture.numbers(), [2, 3, 4]);
}

// 「扫描本店最终文件，取最大备份序号 checked_add(1)」：符合格式的文件都计入序号和保留额度，不检查内容；
// 重启后序号接着递增；时钟回拨后文件名里的时间变早，保留仍按序号。
#[tokio::test]
async fn numbering_continues_across_existing_files_restart_and_clock_rewind() {
    let fixture = Fixture::new(local(12, 30));
    fixture.place(&own_final("20260101T000000Z", "41", &uuid(1)));
    let node = fixture.start(3, SH, "23:30").await;
    fixture.clock.set(local(13, 0));
    fixture.wait_numbers(&[41, 42]).await;
    fixture.clock.set(local(14, 0));
    fixture.wait_numbers(&[41, 42, 43]).await;
    fixture.clock.set(local(15, 0));
    fixture.wait_numbers(&[42, 43, 44]).await;
    node.shutdown().await.unwrap();

    fixture.clock.set(local(15, 30));
    let node = fixture.start(3, SH, "23:30").await;
    fixture.clock.set(local(16, 0));
    fixture.wait_numbers(&[43, 44, 45]).await;

    let yesterday = SH_DAY - 24 * HOUR;
    fixture
        .clock
        .set(UnixMillis(yesterday + 10 * HOUR + 30 * MINUTE));
    settle().await;
    fixture.clock.set(UnixMillis(yesterday + 11 * HOUR));
    fixture.wait_numbers(&[44, 45, 46]).await;
    assert_eq!(
        fixture.times(),
        ["20261006T070000Z", "20261006T080000Z", "20261005T030000Z"]
    );
    node.shutdown().await.unwrap();
}

// ---------------------------------------------------------------- 失败

// 「备份失败时不清理已有的最终备份」：已有序号 u64::MAX 的文件，checked_add 溢出，本次失败；
// 已有文件（超出额度也）不变，临时文件被删除；/health 报告失败时间并为 degraded，成功字段保持上一次。
// 「最近一次尝试成功时 last_backup_failed_at 为 null」：移走该文件、回拨时钟后再次成功，即使成功时间早于失败时间也清除失败状态。
#[tokio::test]
async fn failed_backup_keeps_existing_files_and_is_reported_until_a_later_success() {
    let fixture = Fixture::new(local(12, 30));
    let node = fixture.start(2, SH, "23:30").await;
    create_equipment(&node.router, &cmd(1), "F1").await;
    fixture.clock.set(local(13, 0));
    fixture.wait_numbers(&[1]).await;
    fixture.clock.set(local(14, 0));
    fixture.wait_numbers(&[1, 2]).await;
    wait_for_health(&node.router, "second backup", |d| {
        ms(&d["last_backup_ok_at"]) == Some(local(14, 0).0)
    })
    .await;

    let max = own_final("20260101T000000Z", "18446744073709551615", &uuid(1));
    fixture.place(&max);
    create_equipment(&node.router, &cmd(2), "F2").await;
    fixture.clock.set(local(15, 0));
    let data = wait_for_health(&node.router, "failure", |d| {
        !d["last_backup_failed_at"].is_null()
    })
    .await;
    assert_eq!(ms(&data["last_backup_failed_at"]), Some(local(15, 0).0));
    assert_eq!(data["status"], json!("degraded"));
    assert_eq!(ms(&data["last_backup_ok_at"]), Some(local(14, 0).0));
    assert_eq!(data["last_backup_seq"], json!(1));
    settle().await;
    assert_eq!(fixture.numbers(), [1, 2, u64::MAX]);
    assert!(fixture.temporary_files().is_empty());

    fs::remove_file(fixture.backups().join(&max)).unwrap();
    fixture.clock.set(local(13, 20));
    settle().await;
    fixture.clock.set(local(14, 0));
    let data = wait_for_health(&node.router, "recovery", |d| {
        d["last_backup_failed_at"].is_null()
    })
    .await;
    assert_eq!(data["status"], json!("ok"));
    assert_eq!(ms(&data["last_backup_ok_at"]), Some(local(14, 0).0));
    assert_eq!(data["last_backup_seq"], json!(2));
    fixture.wait_numbers(&[2, 3]).await;
    node.shutdown().await.unwrap();
}

// 失败必须确定能触发，不依赖目录权限：备份目录被换成普通文件时本次失败并报告；恢复目录后下一次成功，清除失败状态，
// 原有备份仍在。失败不影响写入。
#[tokio::test]
async fn io_failure_is_reported_and_cleared_by_the_next_success() {
    let fixture = Fixture::new(local(12, 30));
    let node = fixture.start(3, SH, "23:30").await;
    fixture.clock.set(local(13, 0));
    fixture.wait_numbers(&[1]).await;

    let away = fixture.dir.path().join("backups-away");
    fs::rename(fixture.backups(), &away).unwrap();
    fs::write(fixture.backups(), b"not a directory").unwrap();
    fixture.clock.set(local(14, 0));
    let data = wait_for_health(&node.router, "failure", |d| {
        !d["last_backup_failed_at"].is_null()
    })
    .await;
    assert_eq!(ms(&data["last_backup_failed_at"]), Some(local(14, 0).0));
    assert_eq!(data["status"], json!("degraded"));
    create_equipment(&node.router, &cmd(1), "F1").await;

    fs::remove_file(fixture.backups()).unwrap();
    fs::rename(&away, fixture.backups()).unwrap();
    fixture.clock.set(local(15, 0));
    let data = wait_for_health(&node.router, "recovery", |d| {
        d["last_backup_failed_at"].is_null()
    })
    .await;
    assert_eq!(data["status"], json!("ok"));
    assert_eq!(ms(&data["last_backup_ok_at"]), Some(local(15, 0).0));
    assert_eq!(data["last_backup_seq"], json!(1));
    fixture.wait_numbers(&[1, 2]).await;
    node.shutdown().await.unwrap();
}

// ---------------------------------------------------------------- 恢复演练

type Snapshot = Vec<(String, Vec<Vec<SqlValue>>)>;

fn table_names(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    conn.prepare(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
    )?
    .query_map([], |r| r.get(0))?
    .collect()
}

/// schema 文本、user_version 和每张表按全部列排序的内容。
fn whole_database(conn: &Connection) -> rusqlite::Result<Snapshot> {
    let mut snapshot = vec![
        (
            "sqlite_master".to_owned(),
            spec_support::rows(
                conn,
                "SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name",
            )?,
        ),
        (
            "user_version".to_owned(),
            spec_support::rows(conn, "PRAGMA user_version")?,
        ),
    ];
    for name in table_names(conn)? {
        let columns = conn
            .prepare(&format!("SELECT * FROM \"{name}\""))?
            .column_count();
        let order: Vec<String> = (1..=columns).map(|i| i.to_string()).collect();
        let rows = spec_support::rows(
            conn,
            &format!("SELECT * FROM \"{name}\" ORDER BY {}", order.join(", ")),
        )?;
        snapshot.push((name, rows));
    }
    Ok(snapshot)
}

// AGENTS「备份」恢复演练：经 HTTP 写入设备与温度记录，触发一次备份后不再写入；备份文件是回滚日志模式、完整性检查通过，
// count(*) = max(seq) = /health 的 last_backup_seq。把它复制成新库打开，整库（schema、user_version、每张表）与原库一致；
// 清空投影并按 seq 重建后仍一致。
#[tokio::test]
async fn restore_drill_matches_the_snapshot_and_rebuilds_the_same_projections() {
    let fixture = Fixture::new(local(9, 0));
    let node = fixture.start(3, SH, "23:30").await;
    let router = node.router.clone();
    let a = create_equipment(&router, &cmd(1), "F1").await;
    let b = create_equipment(&router, &cmd(2), "F2").await;
    let reply = spec_support::put(
        &router,
        &format!("/api/v1/equipment/{a}"),
        Some(&MANAGER),
        &json!({
            "command_id": cmd(3), "base_revision": 1, "name": "Walk-in A",
            "equipment_type": "FREEZER", "active": true,
        }),
    )
    .await
    .unwrap();
    assert_success(&reply);
    for (n, equipment, note, captured_at, sent_at) in [
        (4, &a, Some("门 半开"), local(8, 50), local(8, 55)),
        (5, &b, None, local(8, 58), local(8, 57)), // 负 lag，带 CAPTURE_TIME_ADJUSTED
    ] {
        let mut body = json!({
            "command_id": cmd(n), "equipment_id": equipment, "celsius_x10": -180,
            "captured_at": captured_at.0, "sent_at": sent_at.0,
        });
        if let Some(note) = note {
            body["note"] = json!(note);
        }
        let reply =
            spec_support::post(&router, "/api/v1/temperature-readings", Some(&STAFF), &body)
                .await
                .unwrap();
        assert_success(&reply);
    }

    fixture.clock.set(local(10, 0));
    fixture.wait_numbers(&[1]).await;
    let data = wait_for_health(&router, "backup", |d| !d["last_backup_seq"].is_null()).await;
    assert_eq!(data["last_backup_seq"], json!(5));
    drop(router);
    node.shutdown().await.unwrap();

    let backup = fixture
        .backups()
        .join(&backups(&fixture.backups()).unwrap()[0].name);
    let header = fs::read(&backup).unwrap();
    assert_eq!(
        (header[18], header[19]),
        (1, 1),
        "rollback-journal file format"
    );

    let original = whole_database(&spec_support::reader(&fixture.db()).unwrap()).unwrap();
    let restored_path = fixture.dir.path().join("restored.db");
    fs::copy(&backup, &restored_path).unwrap();
    let mut restored = spec_support::writer(&restored_path).unwrap();
    let integrity: String = restored
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let (count, max_seq): (i64, i64) = restored
        .query_row(
            "SELECT count(*), coalesce(max(seq), 0) FROM store_events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((count, max_seq), (5, 5));
    assert_eq!(whole_database(&restored).unwrap(), original);

    assert_eq!(spec_support::rebuild_projections(&mut restored).unwrap(), 5);
    assert_eq!(whole_database(&restored).unwrap(), original);
    let readings: i64 = restored
        .query_row("SELECT count(*) FROM temperature_readings", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(readings, 2);
}
