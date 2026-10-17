//! 锁定测试：`boh-server rebuild-projections <配置文件>` 子命令。
//! 规则见 docs/domain.md「投影表」（按 seq 完整重建）与「时间」（rebuild-projections 不受节点信号限制），
//! 接口见 docs/interfaces.md「boh-server：rebuild-projections 子命令」。

mod spec_support;

use std::ffi::OsStr;
use std::path::Path;

use boh_storage::rusqlite::types::Value as SqlValue;
use spec_support::ntp::{FakeNtp, Reply};
use spec_support::{OTHER_STORE_ID, Paths, paths, reader, run, write_config, writer};

type Rows = Vec<Vec<SqlValue>>;

/// `waste_reasons` 的整表，按 code。
#[allow(clippy::unwrap_used)] // 测试辅助：只读查询失败说明夹具有误。
fn reasons(db: &Path) -> Rows {
    let conn = reader(db).unwrap();
    let mut statement = conn
        .prepare("SELECT * FROM waste_reasons ORDER BY code")
        .unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns).map(|i| row.get::<_, SqlValue>(i)).collect()
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// 账本、命令记录与 store_meta 的整表。
#[allow(clippy::unwrap_used)] // 同上。
fn ledger(db: &Path) -> Vec<Rows> {
    let conn = reader(db).unwrap();
    ["store_meta", "processed_commands", "store_events"]
        .iter()
        .map(|table| {
            let mut statement = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1"))
                .unwrap();
            let columns = statement.column_count();
            statement
                .query_map([], |row| {
                    (0..columns).map(|i| row.get::<_, SqlValue>(i)).collect()
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        })
        .collect()
}

#[allow(clippy::unwrap_used)] // 测试夹具：篡改投影失败时测试无法进行。
fn tamper(db: &Path) {
    let conn = writer(db).unwrap();
    conn.execute_batch(
        "UPDATE waste_reasons SET name = 'tampered', active = 0 WHERE code = 'EXPIRED';
         DELETE FROM waste_reasons WHERE code = 'TASTING';",
    )
    .unwrap();
}

/// 已初始化的节点（init 用有效的状态文件，不查询 NTP），返回它和初始化后的 waste_reasons。
fn initialized(ntp: &FakeNtp) -> (Paths, Rows) {
    let paths = paths(&[ntp]);
    paths.write_clock_sync(60_000);
    assert!(run(&[OsStr::new("init"), paths.config.as_os_str()]).success());
    let online = reasons(&paths.db);
    assert_eq!(online.len(), 5);
    (paths, online)
}

fn rebuild(config: &Path) -> bool {
    run(&[OsStr::new("rebuild-projections"), config.as_os_str()]).success()
}

// 「经 boh_storage::open() 打开 db_path，核对 store_meta，再调用 Writer::rebuild_projections」：篡改的投影（改行、删行）
// 恢复成与在线写入一致，账本不变，退出码 0；可以重复执行。
// 「不受节点信号限制：不读状态文件，不查询 NTP」：状态文件已删除、NTP 服务正常，重建照常成功，NTP 没有收到请求，
// 状态文件仍不存在。
#[test]
fn rebuild_restores_projections_without_a_clock_signal() {
    let ntp = FakeNtp::system(Reply::Offset(0));
    let (paths, online) = initialized(&ntp);
    let ledger_before = ledger(&paths.db);
    std::fs::remove_file(paths.clock_sync()).ok();
    tamper(&paths.db);
    assert_ne!(reasons(&paths.db), online);

    for _ in 0..2 {
        assert!(rebuild(&paths.config));
        assert_eq!(reasons(&paths.db), online);
        assert_eq!(ledger(&paths.db), ledger_before);
    }
    assert_eq!(ntp.requests(), 0);
    assert_eq!(paths.clock_sync_file(), None);
    assert!(!paths.backups.exists());
}

// 「重建失败（如某个事件的 payload 无法解析）时以非 0 退出，投影表保持运行前的内容」：在账本末尾追加一条 payload 缺字段的
// MASTER_DATA_CHANGED（经 open_writer，不写 seq 列），篡改投影后重建：非 0 退出，篡改后的投影原样保留（整体回滚）。
#[test]
#[allow(clippy::unwrap_used)] // 测试夹具：写入坏事件失败时测试无法进行。
fn a_failed_rebuild_exits_non_zero_and_keeps_the_projections() {
    let ntp = FakeNtp::system(Reply::Offset(0));
    let (paths, _) = initialized(&ntp);
    let conn = writer(&paths.db).unwrap();
    conn.execute_batch(
        "INSERT INTO processed_commands (command_id, command_type, request, response, recorded_at)
         VALUES ('01890a5d-ac96-774b-bcce-b30209e00001', 'waste_reason.create', '{}', '{}', 1791248400000);
         INSERT INTO store_events (id, event_type, schema_version, aggregate_type, aggregate_id,
             aggregate_version, command_id, actor_id, device_id, business_date, occurred_at, recorded_at, payload)
         VALUES ('01890a5d-ac96-774b-bcce-b30209e00002', 'MASTER_DATA_CHANGED', 1, 'WASTE_REASON',
             '01890a5d-ac96-774b-bcce-b30209e00003', 1, '01890a5d-ac96-774b-bcce-b30209e00001',
             '00000000-0000-7000-8000-000000000000', '00000000-0000-7000-8000-000000000001',
             '2026-10-06', 1791248400000, 1791248400000, '{\"entity\":\"WASTE_REASON\"}');",
    )
    .unwrap();
    drop(conn);
    tamper(&paths.db);
    let tampered = reasons(&paths.db);
    let ledger_before = ledger(&paths.db);

    assert!(!rebuild(&paths.config));

    assert_eq!(reasons(&paths.db), tampered);
    assert_eq!(ledger(&paths.db), ledger_before);
}

// 「store_meta 为空或 store_id 与配置不同时以非 0 退出，不重建」。
#[test]
fn rebuild_refuses_an_uninitialized_or_foreign_database() {
    let ntp = FakeNtp::system(Reply::Offset(0));
    let (paths, _) = initialized(&ntp);
    tamper(&paths.db);
    let tampered = reasons(&paths.db);
    write_config(
        &paths.config,
        OTHER_STORE_ID,
        &paths.db,
        &paths.backups,
        paths.listen,
        &[&ntp],
    );
    assert!(!rebuild(&paths.config));
    assert_eq!(reasons(&paths.db), tampered);

    let fresh = spec_support::paths(&[&ntp]);
    assert!(!rebuild(&fresh.config));
    if fresh.db.exists() {
        let conn = reader(&fresh.db).unwrap_or_else(|e| panic!("{e}"));
        let meta: i64 = conn
            .query_row("SELECT count(*) FROM store_meta", [], |r| r.get(0))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(meta, 0);
    }
}

// 「参数只接受这一种形式：子命令名拼错、配置文件之外多带参数时以非 0 退出，不打开数据库；配置文件不存在或无法解析时以非 0 退出」。
// 每次都先篡改投影，非 0 退出后篡改仍在（没有重建）。
#[test]
#[allow(clippy::unwrap_used)] // 测试夹具：写配置失败时测试无法进行。
fn rebuild_rejects_malformed_arguments_and_configs() {
    let ntp = FakeNtp::system(Reply::Offset(0));
    let (paths, _) = initialized(&ntp);
    tamper(&paths.db);
    let tampered = reasons(&paths.db);
    let missing = paths.dir.path().join("missing.toml");
    let broken = paths.dir.path().join("broken.toml");
    // 缺少必填的 ntp_servers。
    std::fs::write(
        &broken,
        std::fs::read_to_string(&paths.config)
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("ntp_servers"))
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();

    let config = paths.config.as_os_str();
    for args in [
        vec![
            OsStr::new("rebuild-projections"),
            config,
            OsStr::new("extra"),
        ],
        vec![OsStr::new("rebuild-projection"), config],
        vec![OsStr::new("rebuild"), config],
        vec![OsStr::new("rebuild-projections"), missing.as_os_str()],
        vec![OsStr::new("rebuild-projections"), broken.as_os_str()],
    ] {
        assert!(!run(&args).success(), "{args:?}");
        assert_eq!(reasons(&paths.db), tampered, "{args:?}");
    }
    assert_eq!(ntp.requests(), 0);
}
