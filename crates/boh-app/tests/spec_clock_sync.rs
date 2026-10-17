//! 锁定测试：门店节点校时——状态文件、NTP 核对、核对节奏、跨重启。
//! 规则见 docs/domain.md「时间」门店节点校时；接口见 docs/interfaces.md「带后台任务的测试节点」「时钟」。
//! NTP 服务是 spec_support::ntp 中的假服务（本机 UDP），服务器时间取 `ManualClock` 的墙钟加设定的偏差。

mod spec_support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use boh_app::TestNode;
use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use serde_json::{Value, json};
use spec_support::ntp::{FakeNtp, Reply};
use spec_support::{
    MANAGER, assert_success, clock_sync_file, read_clock_sync_file, settle, write_clock_sync_file,
};

const MS: i64 = 1;
const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;
const NOW: i64 = 1_791_248_400_000; // 2026-10-06 09:00 +08:00

fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b3020c5f{n:04x}")
}

fn ms(millis: i64) -> Duration {
    Duration::from_millis(u64::try_from(millis).unwrap_or_default())
}

struct Fixture {
    dir: tempfile::TempDir,
    db_path: PathBuf,
    clock: ManualClock,
}

impl Fixture {
    #[allow(clippy::unwrap_used)] // 测试夹具：临时目录创建失败时测试无法开始。
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("boh.db");
        Self {
            dir,
            db_path,
            clock: ManualClock::new(UnixMillis(NOW)),
        }
    }

    /// 以给定的 NTP 服务启动节点，不改动状态文件。
    #[allow(clippy::unwrap_used)] // 测试夹具：节点启动失败时后续断言没有意义。
    async fn start(&self, servers: &[&FakeNtp]) -> TestNode {
        let config = spec_support::node_config_with_ntp(
            &self.dir.path().join("backups"),
            3,
            "Asia/Shanghai",
            "23:30",
            servers.iter().map(|server| server.server()).collect(),
        );
        spec_support::bare_node(&self.db_path, self.clock.clock(), config)
            .await
            .unwrap()
    }

    fn advance(&self, millis: i64) {
        self.clock.advance(ms(millis));
    }

    fn set(&self, now: i64) {
        self.clock.set(UnixMillis(now));
    }

    fn file(&self) -> Option<String> {
        read_clock_sync_file(&self.db_path)
    }

    fn write_file(&self, synced_at: i64) {
        write_clock_sync_file(&self.db_path, synced_at);
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：写不进临时目录时测试无法进行。
    fn write_raw(&self, content: &str) {
        std::fs::write(clock_sync_file(&self.db_path), content).unwrap();
    }
}

/// 等到节点完成第 `count` 次核对；10 秒（真实时间）内没有完成则 panic。
async fn checks(node: &TestNode, count: u64) {
    if tokio::time::timeout(Duration::from_secs(10), node.wait_clock_checks(count))
        .await
        .is_err()
    {
        panic!("clock check {count} did not complete");
    }
}

/// 新建一台设备（主数据写命令），返回 HTTP 状态码；成功或被拒都按信封核对。
#[allow(clippy::unwrap_used)] // 测试辅助：响应体不是 JSON 已违反信封约定，直接终止测试。
async fn write(router: &Router, n: u16) -> u16 {
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
    match reply.status.as_u16() {
        200 => {
            assert_success(&reply);
        }
        503 => {
            assert_eq!(
                spec_support::assert_error(&reply, 503, "CLOCK_NOT_SYNCED"),
                &json!({})
            );
        }
        other => panic!("unexpected {other}: {}", reply.body),
    }
    reply.status.as_u16()
}

/// /health 的 (clock_synced, clock_synced_at)。`status` 还受备份结果影响（推进时钟会触发整点备份），这里不比较；
/// clock_synced 与 status 的关系见 spec_clock.rs。
async fn signal(router: &Router) -> (Value, Value) {
    let data = spec_support::health(router).await;
    (
        data["clock_synced"].clone(),
        data["clock_synced_at"].clone(),
    )
}

fn valid(synced_at: i64) -> (Value, Value) {
    (json!(true), json!(synced_at))
}

fn invalid(synced_at: Option<i64>) -> (Value, Value) {
    (json!(false), synced_at.map_or(Value::Null, |at| json!(at)))
}

#[allow(clippy::unwrap_used)] // 测试辅助：健康检查失败本身就是断言失败。
async fn clock_status(router: &Router) -> u16 {
    spec_support::get(router, "/api/v1/clock", None)
        .await
        .unwrap()
        .status
        .as_u16()
}

#[allow(clippy::unwrap_used)] // 测试辅助：关闭失败或超时本身就是断言失败。
async fn shutdown(node: TestNode) {
    tokio::time::timeout(Duration::from_secs(5), node.shutdown())
        .await
        .unwrap()
        .unwrap();
}

// ---------------------------------------------------------------------------
// 启动与状态文件
// ---------------------------------------------------------------------------

// 「启动时读取状态文件：不存在时没有信号，不拒绝启动」；「启动时核对一次，不等待它完成就开始服务」：
// NTP 服务不回复、时钟不推进时，test_node 已返回，查询已发出；写命令 503，/api/v1/clock 503，/health degraded。
// 「正在等待的 NTP 查询直接取消」：此时关闭节点不等满 5 秒超时（真实时间 5 秒内返回，ManualClock 不推进）。
#[tokio::test]
async fn without_a_state_file_the_node_serves_but_rejects_writes() {
    let f = Fixture::new();
    let ntp = FakeNtp::manual(&f.clock, Reply::Silent);
    let node = f.start(&[&ntp]).await;
    ntp.wait_requests(1).await;

    assert_eq!(signal(&node.router).await, invalid(None));
    assert_eq!(
        spec_support::health(&node.router).await["status"],
        json!("degraded")
    );
    assert_eq!(write(&node.router, 1).await, 503);
    assert_eq!(clock_status(&node.router).await, 503);
    assert_eq!(f.file(), None);
    shutdown(node).await;
}

// 「重启不使信号失效：节点启动时读取持久化的上次校时时间，仍满足有效条件即接受写入，不等待本次联网校时成功」。
// 「没有回复（如断网）不使仍有效的信号失效」：查询 5 秒超时后核对完成，信号和状态文件都不变。
#[tokio::test]
async fn a_persisted_signal_is_used_without_waiting_for_ntp() {
    let f = Fixture::new();
    f.write_file(NOW - HOUR);
    let ntp = FakeNtp::manual(&f.clock, Reply::Silent);
    let node = f.start(&[&ntp]).await;
    ntp.wait_requests(1).await;

    assert_eq!(signal(&node.router).await, valid(NOW - HOUR));
    assert_eq!(
        spec_support::health(&node.router).await["status"],
        json!("ok")
    );
    assert_eq!(write(&node.router, 1).await, 200);
    assert_eq!(clock_status(&node.router).await, 200);

    f.advance(5 * SECOND);
    checks(&node, 1).await;
    assert_eq!(signal(&node.router).await, valid(NOW - HOUR));
    assert_eq!(f.file(), Some(format!("{}\n", NOW - HOUR)));
    assert_eq!(write(&node.router, 2).await, 200);
    shutdown(node).await;
}

// 「校时信号」在启动时读到的值上同样适用：上次校时 ≤ 现在 ≤ 上次校时 + 7 天。
// 恰好 7 天之前、恰好等于现在都有效；7 天零 1 ms 之前、晚于现在 1 ms（时钟复位到过去）都无效，后者不拒绝启动。
#[tokio::test]
async fn persisted_signal_validity_boundaries() {
    for (synced_at, expected) in [
        (NOW - WEEK, valid(NOW - WEEK)),
        (NOW, valid(NOW)),
        (NOW - WEEK - MS, invalid(Some(NOW - WEEK - MS))),
        (NOW + MS, invalid(Some(NOW + MS))),
    ] {
        let f = Fixture::new();
        f.write_file(synced_at);
        let ntp = FakeNtp::manual(&f.clock, Reply::Silent);
        let node = f.start(&[&ntp]).await;
        assert_eq!(signal(&node.router).await, expected, "{synced_at}");
        let status = if expected.0 == json!(true) { 200 } else { 503 };
        assert_eq!(write(&node.router, 1).await, status, "{synced_at}");
        shutdown(node).await;
    }
}

// 「内容无法解析（为空、非整数、负数）或读取出错（如路径是目录）时没有信号，下次校时成功后覆盖；不拒绝启动」。
// 覆盖：之后一次核对成功，状态文件改写为规定格式（收到回复时的本机时间加换行），信号恢复。
// 路径是非空目录时，重命名覆盖不了它：信号保持无效（domain「持久化」任一步失败时保留原有信号）。
#[tokio::test]
async fn an_unreadable_state_file_means_no_signal_until_the_next_successful_check() {
    for content in ["", "abc\n", "-1\n", "12.5\n"] {
        let f = Fixture::new();
        f.write_raw(content);
        let ntp = FakeNtp::manual(&f.clock, Reply::Silent);
        let node = f.start(&[&ntp]).await;
        ntp.wait_requests(1).await;
        assert_eq!(signal(&node.router).await, invalid(None), "{content:?}");
        assert_eq!(write(&node.router, 1).await, 503, "{content:?}");

        f.advance(5 * SECOND);
        checks(&node, 1).await;
        ntp.set_default(Reply::Offset(0));
        f.advance(MINUTE);
        checks(&node, 2).await;
        let synced_at = NOW + 5 * SECOND + MINUTE;
        assert_eq!(f.file(), Some(format!("{synced_at}\n")), "{content:?}");
        assert_eq!(signal(&node.router).await, valid(synced_at), "{content:?}");
        assert_eq!(write(&node.router, 2).await, 200, "{content:?}");
        shutdown(node).await;
    }

    let f = Fixture::new();
    let path = clock_sync_file(&f.db_path);
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep"), "x").unwrap();
    let ntp = FakeNtp::manual(&f.clock, Reply::Offset(0));
    let node = f.start(&[&ntp]).await;
    checks(&node, 1).await;
    assert_eq!(signal(&node.router).await, invalid(None));
    assert_eq!(write(&node.router, 1).await, 503);
    assert!(path.is_dir());
    shutdown(node).await;
}

// 「读取时只接受这种形式：不带符号、空白和前导零的十进制整数（0 除外），后面至多一个换行；其他内容都按无法解析处理」。
// 能解析的值即使已过期也是上次校时，/health 报告它（clock_synced_at 不为 null）；无法解析时 clock_synced_at 为 null。
// 非法写法用接近 NOW 的值构造：误收时信号会显示为有效。
#[tokio::test]
async fn the_state_file_accepts_only_the_canonical_form() {
    let v = NOW - HOUR;
    let mut cases: Vec<(String, (Value, Value))> = vec![
        (format!("{v}\n"), valid(v)),
        (format!("{v}"), valid(v)),
        ("0\n".to_owned(), invalid(Some(0))),
        ("0".to_owned(), invalid(Some(0))),
    ];
    for bad in [
        format!("+{v}\n"),
        format!(" {v}\n"),
        format!("{v} \n"),
        format!("\t{v}\n"),
        format!("0{v}\n"),
        "00\n".to_owned(),
        format!("{v}\n\n"),
        format!("{v}\n{v}\n"),
        format!("{v}\r\n"),
        format!("{v}\r"),
        format!("\n{v}"),
        format!("{v}x\n"),
        format!("{v}.0\n"),
        "-0\n".to_owned(),
        "99999999999999999999\n".to_owned(),
        "\n".to_owned(),
    ] {
        cases.push((bad, invalid(None)));
    }
    for (content, expected) in cases {
        let f = Fixture::new();
        f.write_raw(&content);
        let ntp = FakeNtp::manual(&f.clock, Reply::Silent);
        let node = f.start(&[&ntp]).await;
        assert_eq!(signal(&node.router).await, expected, "{content:?}");
        shutdown(node).await;
    }
}

// ---------------------------------------------------------------------------
// 一次核对的结果
// ---------------------------------------------------------------------------

// 「偏差绝对值不超过 1 分钟（含）：校时成功，当时的时间取本机收到回复时的时间」；持久化格式为一行十进制整数加换行；
// 「校时成功后自动恢复写入」。偏差恰好 ±60 000 ms 都成功。重启后读到持久化的值，不等待核对。
#[tokio::test]
async fn a_check_within_one_minute_persists_and_publishes_the_signal() {
    for offset in [0, MINUTE, -MINUTE] {
        let f = Fixture::new();
        let ntp = FakeNtp::manual(&f.clock, Reply::Offset(offset));
        let node = f.start(&[&ntp]).await;
        checks(&node, 1).await;
        assert_eq!(f.file(), Some(format!("{NOW}\n")), "{offset}");
        assert_eq!(signal(&node.router).await, valid(NOW), "{offset}");
        assert_eq!(write(&node.router, 1).await, 200, "{offset}");
        assert_eq!(clock_status(&node.router).await, 200);
        shutdown(node).await;

        f.advance(HOUR);
        let silent = FakeNtp::manual(&f.clock, Reply::Silent);
        let node = f.start(&[&silent]).await;
        assert_eq!(signal(&node.router).await, valid(NOW), "{offset} restart");
        assert_eq!(write(&node.router, 2).await, 200, "{offset} restart");
        shutdown(node).await;
    }
}

// 「偏差绝对值超过 1 分钟：信号立即失效，写入停止」：先在内存中使信号失效并清除上次校时时间（/health 的 clock_synced_at
// 为 null），再删除状态文件。偏差 ±60 001 ms 都失效，即使原信号仍在有效期内。重启后没有信号。
#[tokio::test]
async fn an_offset_over_one_minute_invalidates_the_signal_immediately() {
    for offset in [MINUTE + MS, -MINUTE - MS] {
        let f = Fixture::new();
        f.write_file(NOW - HOUR);
        let ntp = FakeNtp::manual(&f.clock, Reply::Silent);
        let node = f.start(&[&ntp]).await;
        assert_eq!(write(&node.router, 1).await, 200, "{offset}");
        f.advance(5 * SECOND);
        checks(&node, 1).await;

        ntp.set_default(Reply::Offset(offset));
        f.advance(HOUR);
        checks(&node, 2).await;
        assert_eq!(signal(&node.router).await, invalid(None), "{offset}");
        assert_eq!(f.file(), None, "{offset}");
        assert_eq!(write(&node.router, 2).await, 503, "{offset}");
        assert_eq!(clock_status(&node.router).await, 503, "{offset}");
        shutdown(node).await;

        let silent = FakeNtp::manual(&f.clock, Reply::Silent);
        let node = f.start(&[&silent]).await;
        assert_eq!(
            signal(&node.router).await,
            invalid(None),
            "{offset} restart"
        );
        shutdown(node).await;
    }
}

// 「回复的校验由 sntpc 完成；不通过的按没有回复处理」「KoD RATE 及其他代码本次按没有回复处理」：
// 只有一个服务器时，这些回复都让本次核对没有结果，原信号和状态文件不变（不论原信号是否有效）。
#[tokio::test]
async fn invalid_or_rate_limited_replies_leave_the_signal_unchanged() {
    for reply in [Reply::Unsynchronized, Reply::KissOfDeath("RATE")] {
        let f = Fixture::new();
        f.write_file(NOW - HOUR);
        let ntp = FakeNtp::manual(&f.clock, reply.clone());
        let node = f.start(&[&ntp]).await;
        checks(&node, 1).await;
        assert_eq!(signal(&node.router).await, valid(NOW - HOUR), "{reply:?}");
        assert_eq!(f.file(), Some(format!("{}\n", NOW - HOUR)), "{reply:?}");
        shutdown(node).await;

        let f = Fixture::new();
        let ntp = FakeNtp::manual(&f.clock, reply.clone());
        let node = f.start(&[&ntp]).await;
        checks(&node, 1).await;
        assert_eq!(signal(&node.router).await, invalid(None), "{reply:?}");
        assert_eq!(f.file(), None, "{reply:?}");
        shutdown(node).await;
    }
}

// 「本机收到回复的时间早于发出时间，或往返超过 5 秒：本次结果丢弃，按没有回复处理（查询途中本机时钟跳变）。两者都按本机的
// 发出、收到时间计算，不用 sntpc 的往返时间」。假服务在回复之前改动墙钟（只用 set，单调时间不动，超时不会触发），
// 并声明一个服务器处理耗时 p，使 sntpc 的往返时间（本机往返 − p）与本机往返不同：
// - 回拨 1 ms，p = 0：本机收到早于发出，丢弃（sntpc 的往返时间不为负，按它判断会误收）；
// - 前跳 5 001 ms，p = 0：本机往返 5 001 ms，丢弃；
// - 前跳 5 001 ms，p = 5 001：sntpc 的往返为 0，本机往返仍是 5 001 ms，丢弃；
// - 前跳 4 999 ms，p = 0：本机往返不超过 5 秒，成功；
// - 前跳 4 999 ms，p = −1 000：sntpc 的往返 5 999 ms，本机往返 4 999 ms，成功。
// 这些回复算出的偏差都在 1 分钟以内。丢弃时状态文件和信号不变；成功时上次校时取收到回复时的本机时间 NOW + 4 999。
#[tokio::test]
async fn a_clock_jump_during_the_query_is_judged_by_local_send_and_receive_times() {
    for (jump, processing, accepted) in [
        (-MS, 0, false),
        (5_001, 0, false),
        (5_001, 5_001, false),
        (4_999, 0, true),
        (4_999, -SECOND, true),
    ] {
        let f = Fixture::new();
        f.write_file(NOW - HOUR);
        let ntp = FakeNtp::manual(
            &f.clock,
            Reply::JumpThenOffset {
                jump_ms: jump,
                offset_ms: 0,
                processing_ms: processing,
            },
        );
        let node = f.start(&[&ntp]).await;
        checks(&node, 1).await;
        let expected = if accepted { NOW + jump } else { NOW - HOUR };
        let case = format!("jump {jump} processing {processing}");
        assert_eq!(f.file(), Some(format!("{expected}\n")), "{case}");
        assert_eq!(signal(&node.router).await, valid(expected), "{case}");
        shutdown(node).await;
    }
}

// ---------------------------------------------------------------------------
// 多个服务器
// ---------------------------------------------------------------------------

// 「按 ntp_servers 的顺序逐个查询」「每个服务器的查询超时 5 秒」：第一个不回复时，满 5 秒（Clock::sleep）才查询第二个；
// 4 999 ms 时第二个还没有收到请求。第二个的合法回复决定结果，上次校时是收到回复时的本机时间 NOW + 5 秒。
#[tokio::test]
async fn servers_are_queried_in_order_with_a_five_second_timeout() {
    let f = Fixture::new();
    let first = FakeNtp::manual(&f.clock, Reply::Silent);
    let second = FakeNtp::manual(&f.clock, Reply::Offset(0));
    let node = f.start(&[&first, &second]).await;
    first.wait_requests(1).await;
    settle().await;
    assert_eq!(second.requests(), 0);

    f.advance(4_999);
    settle().await;
    assert_eq!(second.requests(), 0);
    f.advance(1);
    checks(&node, 1).await;
    assert_eq!(second.requests(), 1);
    assert_eq!(f.file(), Some(format!("{}\n", NOW + 5 * SECOND)));
    assert_eq!(signal(&node.router).await, valid(NOW + 5 * SECOND));
    shutdown(node).await;
}

// 「第一个给出合法回复的服务器决定本次结果」：第一个合法回复偏差超过 1 分钟时，信号失效，不再查询第二个；
// 第一个合法回复成功时同样不查询第二个。
#[tokio::test]
async fn the_first_valid_reply_decides() {
    let f = Fixture::new();
    f.write_file(NOW - HOUR);
    let first = FakeNtp::manual(&f.clock, Reply::Offset(2 * MINUTE));
    let second = FakeNtp::manual(&f.clock, Reply::Offset(0));
    let node = f.start(&[&first, &second]).await;
    checks(&node, 1).await;
    assert_eq!(signal(&node.router).await, invalid(None));
    assert_eq!(second.requests(), 0);

    first.set_default(Reply::Offset(0));
    f.advance(MINUTE);
    checks(&node, 2).await;
    assert_eq!(signal(&node.router).await, valid(NOW + MINUTE));
    assert_eq!(second.requests(), 0);
    shutdown(node).await;
}

// 「不通过的按没有回复处理，立即查询下一个服务器，不等满 5 秒」（时钟不推进）；该服务器之后照常查询。
#[tokio::test]
async fn an_invalid_reply_moves_on_to_the_next_server_immediately() {
    let f = Fixture::new();
    let first = FakeNtp::manual(&f.clock, Reply::Unsynchronized);
    let second = FakeNtp::manual(&f.clock, Reply::Offset(0));
    let node = f.start(&[&first, &second]).await;
    checks(&node, 1).await;
    assert_eq!((first.requests(), second.requests()), (1, 1));
    assert_eq!(signal(&node.router).await, valid(NOW));

    f.advance(HOUR);
    checks(&node, 2).await;
    assert_eq!((first.requests(), second.requests()), (2, 2));
    shutdown(node).await;
}

// 「本机收到回复的时间早于发出时间，或往返超过 5 秒：本次结果丢弃，按没有回复处理」「没有合法回复的服务器跳过」：
// 第一个服务器回复途中墙钟回拨 1 ms 或前跳 5 001 ms，它的结果丢弃，接着查询第二个服务器（单调时间不动，不等 5 秒超时）；
// 第二个的合法回复决定结果，上次校时取收到第二个回复时的本机时间 NOW + jump。启动时没有状态文件。
#[tokio::test]
async fn a_clock_jump_on_one_server_moves_on_to_the_next_server() {
    for jump in [-MS, 5_001] {
        let f = Fixture::new();
        let first = FakeNtp::manual(
            &f.clock,
            Reply::JumpThenOffset {
                jump_ms: jump,
                offset_ms: 0,
                processing_ms: 0,
            },
        );
        let second = FakeNtp::manual(&f.clock, Reply::Offset(0));
        let node = f.start(&[&first, &second]).await;
        checks(&node, 1).await;
        assert_eq!((first.requests(), second.requests()), (1, 1), "jump {jump}");
        assert_eq!(f.file(), Some(format!("{}\n", NOW + jump)), "jump {jump}");
        assert_eq!(signal(&node.router).await, valid(NOW + jump), "jump {jump}");
        assert_eq!(write(&node.router, 1).await, 200, "jump {jump}");
        shutdown(node).await;
    }
}

// 「KoD：DENY、RSTR 时本进程不再查询该服务器；RATE 及其他代码本次按没有回复处理」。
// 本次都转而查询下一个服务器（时钟不推进）；之后的核对中 DENY / RSTR 的服务器不再收到请求，RATE 和其他代码（如 ACST）的照常收到。
// 重启后（新进程）重新查询被停用的服务器。
#[tokio::test]
async fn kiss_of_death_deny_and_rstr_retire_the_server_for_this_process() {
    for (code, retired) in [
        ("DENY", true),
        ("RSTR", true),
        ("RATE", false),
        ("ACST", false),
    ] {
        let f = Fixture::new();
        let first = FakeNtp::manual(&f.clock, Reply::KissOfDeath(code));
        let second = FakeNtp::manual(&f.clock, Reply::Offset(0));
        let node = f.start(&[&first, &second]).await;
        checks(&node, 1).await;
        assert_eq!((first.requests(), second.requests()), (1, 1), "{code}");
        assert_eq!(signal(&node.router).await, valid(NOW), "{code}");

        for n in 2..=3 {
            f.advance(HOUR);
            checks(&node, n).await;
            let expected_first = if retired { 1 } else { n };
            assert_eq!(
                (first.requests(), second.requests()),
                (expected_first, n),
                "{code} check {n}"
            );
        }
        shutdown(node).await;

        let node = f.start(&[&first, &second]).await;
        first.wait_requests(if retired { 2 } else { 4 }).await;
        shutdown(node).await;
    }
}

// ---------------------------------------------------------------------------
// 节奏
// ---------------------------------------------------------------------------

/// 等到节点完成第 `count` 次每分钟判断；10 秒（真实时间）内没有完成则 panic。
async fn ticks(node: &TestNode, count: u64) {
    if tokio::time::timeout(Duration::from_secs(10), node.wait_clock_ticks(count))
        .await
        .is_err()
    {
        panic!("clock tick {count} did not complete");
    }
}

// 「之后每隔 1 分钟判断一次：信号无效时核对，有效时距上次核对满 1 小时才核对」。
// 有效（启动核对成功）：推进 59 分钟，这次判断不核对；再推进 1 分钟，满 1 小时，核对；再 1 分钟不核对。
// 无效（没有回复）：每次判断都核对；1 分钟少 1 ms 时还没有判断。
#[tokio::test]
async fn checks_run_hourly_while_valid_and_every_minute_while_invalid() {
    let f = Fixture::new();
    let ntp = FakeNtp::manual(&f.clock, Reply::Offset(0));
    let node = f.start(&[&ntp]).await;
    checks(&node, 1).await;
    f.advance(59 * MINUTE);
    ticks(&node, 1).await;
    assert_eq!(ntp.requests(), 1);
    f.advance(MINUTE);
    ticks(&node, 2).await;
    assert_eq!(ntp.requests(), 2);
    f.advance(MINUTE);
    ticks(&node, 3).await;
    assert_eq!(ntp.requests(), 2);
    shutdown(node).await;

    let f = Fixture::new();
    let ntp = FakeNtp::manual(&f.clock, Reply::Unsynchronized);
    let node = f.start(&[&ntp]).await;
    checks(&node, 1).await;
    f.advance(MINUTE - MS);
    settle().await;
    assert_eq!(ntp.requests(), 1);
    f.advance(MS);
    ticks(&node, 1).await;
    assert_eq!(ntp.requests(), 2);
    for n in 2..=3 {
        f.advance(MINUTE);
        ticks(&node, n).await;
        assert_eq!(ntp.requests(), n + 1);
    }
    shutdown(node).await;
}

// 「等待期间信号过期的，下一次判断时即开始每分钟核对」：上次校时为 NOW − 7 天 + 5 分钟，核对一直没有回复。
// 推进 5 分钟时信号恰好仍有效，这次判断不核对；再推进 1 分钟，信号已过期，这次判断就核对（不等满 1 小时），之后每分钟核对。
#[tokio::test]
async fn a_signal_that_expires_while_waiting_is_checked_at_the_next_minute() {
    let f = Fixture::new();
    f.write_file(NOW - WEEK + 5 * MINUTE);
    let ntp = FakeNtp::manual(&f.clock, Reply::Unsynchronized);
    let node = f.start(&[&ntp]).await;
    checks(&node, 1).await;
    f.advance(5 * MINUTE);
    ticks(&node, 1).await;
    assert_eq!(ntp.requests(), 1);
    assert_eq!(signal(&node.router).await, valid(NOW - WEEK + 5 * MINUTE));
    f.advance(MINUTE);
    ticks(&node, 2).await;
    assert_eq!(ntp.requests(), 2);
    assert_eq!(
        signal(&node.router).await,
        invalid(Some(NOW - WEEK + 5 * MINUTE))
    );
    f.advance(MINUTE);
    ticks(&node, 3).await;
    assert_eq!(ntp.requests(), 3);
    shutdown(node).await;
}

// 「之后每隔 1 分钟判断一次：信号无效时核对，有效时距上次核对满 1 小时才核对。等待期间信号过期的，下一次判断时即开始每分钟核对」：
// 状态文件剩 15 分钟有效期（上次校时 NOW − 7 天 + 15 分钟），启动核对没有回复。逐分钟推进：第 1–15 次判断时信号仍有效
// （第 15 次恰在有效期末端），距上次核对不满 1 小时，都不核对；再推进 1 ms 信号过期，写入 503；第 16 次判断（过期后 1 分钟内）
// 就核对，这次回复正常：上次校时 = NOW + 16 分钟，写入恢复。之后回到「满 1 小时才核对」：第 17 次判断不核对。
#[tokio::test]
async fn a_signal_expiring_during_the_hourly_wait_is_checked_within_a_minute() {
    let f = Fixture::new();
    let synced_at = NOW - WEEK + 15 * MINUTE;
    f.write_file(synced_at);
    let ntp = FakeNtp::manual(&f.clock, Reply::Unsynchronized);
    let node = f.start(&[&ntp]).await;
    checks(&node, 1).await;
    assert_eq!(signal(&node.router).await, valid(synced_at));
    for n in 1..=15 {
        f.advance(MINUTE);
        ticks(&node, n).await;
        assert_eq!(ntp.requests(), 1, "tick {n}");
        assert_eq!(signal(&node.router).await, valid(synced_at), "tick {n}");
    }

    ntp.set_default(Reply::Offset(0));
    f.advance(MS);
    assert_eq!(signal(&node.router).await, invalid(Some(synced_at)));
    assert_eq!(write(&node.router, 1).await, 503);
    f.advance(MINUTE - MS);
    ticks(&node, 16).await;
    assert_eq!(ntp.requests(), 2);
    assert_eq!(f.file(), Some(format!("{}\n", NOW + 16 * MINUTE)));
    assert_eq!(signal(&node.router).await, valid(NOW + 16 * MINUTE));
    assert_eq!(write(&node.router, 2).await, 200);

    f.advance(MINUTE);
    ticks(&node, 17).await;
    assert_eq!(ntp.requests(), 2);
    shutdown(node).await;
}

// 节奏随信号切换：偏差超过 1 分钟使信号失效后，下一次判断（1 分钟后）就核对；成功恢复之后，满 1 小时才再核对。
#[tokio::test]
async fn the_interval_follows_the_signal_after_each_check() {
    let f = Fixture::new();
    f.write_file(NOW - HOUR);
    let ntp = FakeNtp::manual(&f.clock, Reply::Offset(0));
    ntp.push(&[Reply::Offset(-2 * MINUTE)]);
    let node = f.start(&[&ntp]).await;
    checks(&node, 1).await;
    assert_eq!(signal(&node.router).await, invalid(None));
    f.advance(MINUTE);
    ticks(&node, 1).await;
    assert_eq!(ntp.requests(), 2);
    assert_eq!(signal(&node.router).await, valid(NOW + MINUTE));
    f.advance(MINUTE);
    ticks(&node, 2).await;
    assert_eq!(ntp.requests(), 2);
    f.advance(58 * MINUTE);
    ticks(&node, 3).await;
    assert_eq!(ntp.requests(), 2);
    f.advance(MINUTE);
    ticks(&node, 4).await;
    assert_eq!(ntp.requests(), 3);
    shutdown(node).await;
}

// 「间隔按单调时间计，不按墙钟时刻：墙钟回拨不推迟下一次核对」：墙钟向前跳 2 小时不触发判断，也不提前核对；回拨 2 小时后，
// 单调时间满 1 小时仍按时核对（ManualClock 一次推进 1 小时，下一次判断就核对，见 interfaces「时钟」），成功的上次校时
// 取当时的墙钟。
#[tokio::test]
async fn the_interval_uses_monotonic_time() {
    let f = Fixture::new();
    let ntp = FakeNtp::manual(&f.clock, Reply::Offset(0));
    let node = f.start(&[&ntp]).await;
    checks(&node, 1).await;

    f.set(NOW + 2 * HOUR);
    settle().await;
    assert_eq!(ntp.requests(), 1);

    f.set(NOW - 2 * HOUR);
    f.advance(HOUR);
    checks(&node, 2).await;
    assert_eq!(ntp.requests(), 2);
    assert_eq!(f.file(), Some(format!("{}\n", NOW - HOUR)));
    assert_eq!(signal(&node.router).await, valid(NOW - HOUR));
    shutdown(node).await;
}

// 「有效时距上次核对满 1 小时才核对」「间隔按单调时间计，不按墙钟时刻」：信号全程有效（墙钟始终不早于上次校时 NOW），
// 「满 1 小时」只看单调时间。启动核对后墙钟前跳 3 小时，再推进 30 分钟：墙钟距上次核对 3.5 小时，单调时间 30 分钟，这次判断
// 不核对。墙钟再回到 NOW + 10 分钟，推进 30 分钟：墙钟距上次核对 40 分钟，单调时间满 1 小时，这次判断核对，上次校时取当时的
// 墙钟 NOW + 40 分钟。
#[tokio::test]
async fn the_hourly_check_counts_monotonic_time_while_the_signal_is_valid() {
    let f = Fixture::new();
    let ntp = FakeNtp::manual(&f.clock, Reply::Offset(0));
    let node = f.start(&[&ntp]).await;
    checks(&node, 1).await;

    f.set(NOW + 3 * HOUR);
    f.advance(30 * MINUTE);
    ticks(&node, 1).await;
    assert_eq!(ntp.requests(), 1);
    assert_eq!(signal(&node.router).await, valid(NOW));

    f.set(NOW + 10 * MINUTE);
    f.advance(30 * MINUTE);
    ticks(&node, 2).await;
    assert_eq!(ntp.requests(), 2);
    assert_eq!(f.file(), Some(format!("{}\n", NOW + 40 * MINUTE)));
    assert_eq!(signal(&node.router).await, valid(NOW + 40 * MINUTE));
    shutdown(node).await;
}

// 「校时信号」：核对一直没有回复时，信号到上次校时 + 7 天为止有效，多 1 ms 起写入 503。
#[tokio::test]
async fn the_signal_expires_seven_days_after_the_last_sync() {
    let f = Fixture::new();
    f.write_file(NOW);
    let ntp = FakeNtp::manual(&f.clock, Reply::Unsynchronized);
    let node = f.start(&[&ntp]).await;
    checks(&node, 1).await;
    f.advance(WEEK);
    assert_eq!(signal(&node.router).await, valid(NOW));
    assert_eq!(write(&node.router, 1).await, 200);
    f.advance(MS);
    assert_eq!(signal(&node.router).await, invalid(Some(NOW)));
    assert_eq!(write(&node.router, 2).await, 503);
    shutdown(node).await;
}

// ---------------------------------------------------------------------------
// 持久化失败
// ---------------------------------------------------------------------------

/// 把数据库所在目录设为只读，使状态文件的写入（临时文件、重命名、删除）失败。用例要求以非 root 用户运行
/// （docs/interfaces.md「带后台任务的测试节点」）：root 下目录权限不起作用，构造不了失败，报告环境不满足条件并失败，不跳过。
#[cfg(unix)]
#[allow(clippy::unwrap_used)] // 测试夹具：改不了权限时测试无法进行。
fn set_read_only(dir: &Path, read_only: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if read_only { 0o555 } else { 0o755 };
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
    if read_only {
        let probe = dir.join("probe");
        assert!(
            std::fs::write(&probe, "x").is_err(),
            "test environment not satisfied: directory permissions are not enforced (running as root?)"
        );
    }
}

// 「校时成功后：写临时文件 → fsync → 原子重命名 → fsync 目录；四步全部成功才发布新的信号。任一步失败时内存中保留原有信号
// （仍有效就继续使用），下次核对成功时重新执行全部四步」「重命名之前失败：状态文件保持原内容」。目录只读时建不了临时文件，
// 失败在重命名之前：/health 仍是原来的上次校时，状态文件不变；重启后读到原来的值（下一个用例）。恢复权限后下一次成功的核对
// （信号仍有效，满 1 小时）写入新值。重命名之后 fsync 目录失败无法注入，不在锁定测试中。
#[cfg(unix)]
#[tokio::test]
async fn a_failed_write_keeps_the_previous_signal() {
    let f = Fixture::new();
    f.write_file(NOW - HOUR);
    let ntp = FakeNtp::manual(&f.clock, Reply::Silent);
    let node = f.start(&[&ntp]).await;
    ntp.wait_requests(1).await;
    set_read_only(f.dir.path(), true);
    f.advance(5 * SECOND);
    checks(&node, 1).await;

    ntp.set_default(Reply::Offset(0));
    f.advance(HOUR);
    checks(&node, 2).await;
    assert_eq!(signal(&node.router).await, valid(NOW - HOUR));
    assert_eq!(f.file(), Some(format!("{}\n", NOW - HOUR)));

    set_read_only(f.dir.path(), false);
    f.advance(HOUR);
    checks(&node, 3).await;
    let synced_at = NOW + 5 * SECOND + 2 * HOUR;
    assert_eq!(f.file(), Some(format!("{synced_at}\n")));
    assert_eq!(signal(&node.router).await, valid(synced_at));
    shutdown(node).await;

    let silent = FakeNtp::manual(&f.clock, Reply::Silent);
    let node = f.start(&[&silent]).await;
    assert_eq!(signal(&node.router).await, valid(synced_at));
    shutdown(node).await;
}

// 同上，失败发生后、恢复之前重启：读到写入失败之前的值。
#[cfg(unix)]
#[tokio::test]
async fn a_restart_after_a_failed_write_reads_the_previous_value() {
    let f = Fixture::new();
    f.write_file(NOW - HOUR);
    let ntp = FakeNtp::manual(&f.clock, Reply::Silent);
    let node = f.start(&[&ntp]).await;
    ntp.wait_requests(1).await;
    set_read_only(f.dir.path(), true);
    ntp.set_default(Reply::Offset(0));
    f.advance(5 * SECOND);
    checks(&node, 1).await;
    f.advance(HOUR);
    checks(&node, 2).await;
    set_read_only(f.dir.path(), false);
    shutdown(node).await;

    let silent = FakeNtp::manual(&f.clock, Reply::Silent);
    let node = f.start(&[&silent]).await;
    assert_eq!(signal(&node.router).await, valid(NOW - HOUR));
    shutdown(node).await;
}

// 「偏差超过 1 分钟：先在内存中使信号失效并清除上次校时时间，再删除状态文件并 fsync 目录」「删除或 fsync 目录失败：
// 记 error，不重试」，「已知限制」状态文件的存储故障：
// - 目录只读、删除失败时，内存中的信号照样立即失效（/health null，写入 503），状态文件仍在；
// - 恢复权限后不重试删除，文件仍在，内存中仍没有信号；
// - 重启后读回已失效的上次校时：仍在有效期内，节点重新有信号。
#[cfg(unix)]
#[tokio::test]
async fn a_failed_deletion_is_not_retried() {
    let f = Fixture::new();
    f.write_file(NOW - HOUR);
    let ntp = FakeNtp::manual(&f.clock, Reply::Silent);
    let node = f.start(&[&ntp]).await;
    ntp.wait_requests(1).await;
    set_read_only(f.dir.path(), true);
    ntp.set_default(Reply::Offset(2 * MINUTE));
    f.advance(5 * SECOND);
    checks(&node, 1).await;
    f.advance(HOUR);
    ticks(&node, 1).await;
    assert_eq!(ntp.requests(), 2);
    assert_eq!(signal(&node.router).await, invalid(None));
    assert_eq!(write(&node.router, 1).await, 503);
    assert_eq!(f.file(), Some(format!("{}\n", NOW - HOUR)));

    set_read_only(f.dir.path(), false);
    ntp.set_default(Reply::Unsynchronized);
    f.advance(MINUTE);
    ticks(&node, 2).await;
    assert_eq!(f.file(), Some(format!("{}\n", NOW - HOUR)));
    assert_eq!(signal(&node.router).await, invalid(None));
    shutdown(node).await;

    let silent = FakeNtp::manual(&f.clock, Reply::Silent);
    let node = f.start(&[&silent]).await;
    assert_eq!(signal(&node.router).await, valid(NOW - HOUR));
    shutdown(node).await;
}

// ---------------------------------------------------------------------------
// 关闭
// ---------------------------------------------------------------------------

// AGENTS「Rust」优雅关闭：「正在等待的 NTP 查询直接取消」，核对间隔的等待同样取消；ManualClock 不推进时关闭在
// 真实时间 5 秒内返回。
#[tokio::test]
async fn shutdown_cancels_pending_queries_and_waits() {
    let f = Fixture::new();
    let ntp = FakeNtp::manual(&f.clock, Reply::Silent);
    let node = f.start(&[&ntp]).await;
    ntp.wait_requests(1).await;
    shutdown(node).await;

    let f = Fixture::new();
    let ntp = FakeNtp::manual(&f.clock, Reply::Offset(0));
    let node = f.start(&[&ntp]).await;
    checks(&node, 1).await;
    shutdown(node).await;
    assert_eq!(f.file(), Some(format!("{NOW}\n")));
}

// domain「时间」门店节点校时：「rebuild-projections、备份、不变量自检不受限制」。没有状态文件、NTP 不回复的节点，
// 启动检查照常完成并报告，整点备份照常执行（NOW 是 01:00Z，推进 1 小时到 02:00Z 触发）。
#[tokio::test]
async fn backups_and_invariant_checks_run_without_a_signal() {
    let f = Fixture::new();
    let ntp = FakeNtp::manual(&f.clock, Reply::Silent);
    let node = f.start(&[&ntp]).await;
    let data = spec_support::wait_for_health(&node.router, "startup check", |d| {
        !d["invariant_check"]["last_attempt"].is_null()
    })
    .await;
    assert_eq!(
        data["invariant_check"]["last_attempt"]["result"],
        json!("passed")
    );
    assert_eq!(data["clock_synced"], json!(false));

    f.advance(HOUR);
    let data = spec_support::wait_for_health(&node.router, "hourly backup", |d| {
        !d["last_backup_ok_at"].is_null()
    })
    .await;
    assert_eq!(data["last_backup_seq"], json!(0));
    assert_eq!(data["last_backup_failed_at"], Value::Null);
    assert_eq!(data["clock_synced"], json!(false));
    shutdown(node).await;
}
