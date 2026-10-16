//! 锁定测试：不变量自检的检查项、报告、触发与关闭。
//! 规则见 docs/invariant-check.md、docs/domain.md「投影表」不变量、AGENTS.md「HTTP 约定」/health，
//! 接口见 docs/interfaces.md「boh-app：带后台任务的测试节点」。
//! 异常只经 boh_storage::testing::open_writer（节点启动前）或 TestNode::writer（节点运行中）修改投影表制造；
//! 不关闭外键，不替换视图定义。
//! 「暂停句柄释放前不得发生」的要求以 300ms 的观察窗口作负断言；窗口只是观察手段，规则不随窗口长短改变。
//! 第 3 项（INVENTORY_ON_HAND）目前由视图定义保证：本文件不构造它的违规，违规分支未经测试；
//! 它确实被执行，由视图求和超出 i64 的 failed 用例证明。
//! 当前测试层未自动覆盖、通过 review 核对：一次检查的全部检查项与 max_seq 共用一次 Readers::call；
//! 启动、闭店、测试入口进入同一个串行队列，串行覆盖整个执行过程；启动检查只提交一次、首次结束前两个字段为 null；
//! 关闭时已提交但尚未开始的检查执行完。

mod spec_support;

use std::fs;
use std::path::PathBuf;
use std::pin::pin;
use std::time::Duration;

use axum::Router;
use boh_app::TestNode;
use boh_domain::UnixMillis;
use boh_storage::StorageError;
use boh_storage::clock::ManualClock;
use boh_storage::rusqlite::Transaction;
use boh_storage::rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use spec_support::{
    MANAGER, STAFF, TEST_STORE_ID, assert_success, backup_numbers, health, settle, wait_for_health,
    wait_until,
};
use tempfile::TempDir;

/// 2026-10-06 00:00 +08:00（Asia/Shanghai）。
const SH_DAY: i64 = 1_791_216_000_000;
const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;
/// 2026-10-06 09:00 +08:00。
const NOW: i64 = SH_DAY + 9 * HOUR;

const LOT: &str = "INVENTORY_LOT_REMAINING";
const UNALLOCATED: &str = "INVENTORY_UNALLOCATED";
const ON_HAND: &str = "INVENTORY_ON_HAND";

/// `stock` 建的面粉批次：收货 A 5 g、B 10 g。
const A: &str = "RAW-FLOUR-20261006-001";
const B: &str = "RAW-FLOUR-20261006-002";

/// 2026-10-06 当地 h:m（h 可以是 24，表示次日 00:00）。
fn local(h: i64, m: i64) -> i64 {
    SH_DAY + h * HOUR + m * MINUTE
}

fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209f1{n:04x}")
}

struct Fixture {
    dir: TempDir,
    clock: ManualClock,
}

#[allow(clippy::unwrap_used)] // 测试夹具：建目录、启动节点或改库失败时后续断言没有意义，直接终止测试。
impl Fixture {
    fn new(start: i64) -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            clock: ManualClock::new(UnixMillis(start)),
        }
    }

    fn db(&self) -> PathBuf {
        self.dir.path().join("boh.db")
    }

    fn backups(&self) -> PathBuf {
        self.dir.path().join("backups")
    }

    fn set(&self, now: i64) {
        self.clock.set(UnixMillis(now));
    }

    /// 启动节点（闭店时刻 23:30），等启动检查的结果交给 /health，返回节点和该次的 `invariant_check`。
    async fn start(&self) -> (TestNode, Value) {
        let config = spec_support::node_config(&self.backups(), 5, "Asia/Shanghai", "23:30");
        let node = spec_support::node(&self.db(), self.clock.clock(), config)
            .await
            .unwrap();
        let data = wait_for_health(&node.router, "startup check", |d| {
            !d["invariant_check"]["last_attempt"].is_null()
        })
        .await;
        (node, data["invariant_check"].clone())
    }

    /// 账本、幂等记录和库存投影的全部行（经只读连接）。
    fn state(&self) -> Vec<Vec<Vec<SqlValue>>> {
        let reader = spec_support::reader(&self.db()).unwrap();
        [
            "SELECT * FROM store_events ORDER BY seq",
            "SELECT * FROM processed_commands ORDER BY command_id",
            "SELECT * FROM inventory_lots ORDER BY lot_id",
            "SELECT * FROM inventory_unallocated ORDER BY item_id",
            "SELECT * FROM inventory_movements ORDER BY event_seq, movement_no",
        ]
        .iter()
        .map(|sql| spec_support::rows(&reader, sql).unwrap())
        .collect()
    }

    /// 节点启动前经原始写连接修改数据库。
    fn corrupt(&self, sql: &str) {
        spec_support::writer(&self.db())
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
}

#[derive(Debug)]
struct SqlFailed(#[allow(dead_code)] String);

impl From<StorageError> for SqlFailed {
    fn from(error: StorageError) -> Self {
        Self(format!("{error:?}"))
    }
}

/// 节点运行中经它唯一的写连接修改数据库。
#[allow(clippy::unwrap_used)] // 测试夹具：改库失败时后续断言没有意义，直接终止测试。
async fn exec(node: &TestNode, sql: String) {
    node.writer()
        .call(move |tx: &Transaction<'_>| {
            tx.execute_batch(&sql)
                .map_err(|error| SqlFailed(error.to_string()))
        })
        .await
        .unwrap();
}

/// 当前 /health 的 `invariant_check`。
async fn invariant(router: &Router) -> Value {
    health(router).await["invariant_check"].clone()
}

/// 经测试入口执行一次检查，返回之后的 `invariant_check`。
async fn run(node: &TestNode) -> Value {
    node.run_invariant_check().await;
    invariant(&node.router).await
}

fn attempt(finished_at: i64, result: &str) -> Value {
    json!({ "finished_at": finished_at, "result": result })
}

fn report(
    finished_at: i64,
    max_seq: i64,
    violations: [u64; 3],
    samples: Vec<Value>,
    truncated: bool,
) -> Value {
    json!({
        "finished_at": finished_at,
        "max_seq": max_seq,
        "checks": [
            { "check": LOT, "violations": violations[0] },
            { "check": UNALLOCATED, "violations": violations[1] },
            { "check": ON_HAND, "violations": violations[2] },
        ],
        "violation_count": violations.iter().sum::<u64>(),
        "samples": samples,
        "samples_truncated": truncated,
    })
}

fn passed(finished_at: i64, max_seq: i64) -> Value {
    json!({
        "last_attempt": attempt(finished_at, "passed"),
        "last_report": report(finished_at, max_seq, [0, 0, 0], vec![], false),
    })
}

/// 第 1 项的一条样本；`actual`、`expected` 是基本单位的小数字符串（存储值 7000 写作 "7"）。
fn lot_sample(item_id: &str, lot_id: &str, actual: &str, expected: &str) -> Value {
    json!({
        "check": LOT,
        "subject": { "item_id": item_id, "lot_id": lot_id },
        "actual": actual,
        "expected": expected,
    })
}

/// 第 2、3 项的一条样本；`actual`、`expected` 同 `lot_sample`。
fn item_sample(check: &str, item_id: &str, actual: &str, expected: &str) -> Value {
    json!({
        "check": check,
        "subject": { "item_id": item_id },
        "actual": actual,
        "expected": expected,
    })
}

/// 同一检查项内按 `subject.item_id` 的字节序排列（物料 ID 由服务端生成，测试不预设先后）。
fn by_item(mut samples: Vec<Value>) -> Vec<Value> {
    samples.sort_by(|a, b| {
        a["subject"]["item_id"]
            .as_str()
            .cmp(&b["subject"]["item_id"].as_str())
    });
    samples
}

#[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态的请求失败时，后续步骤没有意义，直接终止测试。
async fn ok(router: &Router, actor: &spec_support::Actor, uri: &str, body: Value) -> Value {
    let reply = spec_support::post(router, uri, Some(actor), &body)
        .await
        .unwrap();
    assert_success(&reply).clone()
}

#[allow(clippy::unwrap_used)] // 同上。
async fn item(router: &Router, n: u16, code: &str) -> String {
    let data = ok(
        router,
        &MANAGER,
        "/api/v1/items",
        json!({
            "command_id": cmd(n), "code": code, "name": code, "base_unit": "g",
            "category": "RAW", "units": [], "active": true,
        }),
    )
    .await;
    data["item"]["item_id"].as_str().unwrap().to_owned()
}

#[allow(clippy::unwrap_used)] // 同上。
async fn supplier(router: &Router, n: u16) -> String {
    let data = ok(
        router,
        &MANAGER,
        "/api/v1/suppliers",
        json!({ "command_id": cmd(n), "code": "S1", "name": "S1", "active": true }),
    )
    .await;
    data["supplier"]["supplier_id"].as_str().unwrap().to_owned()
}

/// 收货，各行 `qty` g；返回各行的批次号。员工在 `now − lag` 完成录入（节点时钟是 `now`）。
#[allow(clippy::unwrap_used)] // 同上。
#[allow(clippy::too_many_arguments)] // 测试辅助：参数逐一对应请求字段。
async fn receive(
    router: &Router,
    n: u16,
    now: i64,
    lag: i64,
    supplier: &str,
    item: &str,
    qtys: &[i64],
) -> Vec<String> {
    let lines: Vec<Value> = qtys
        .iter()
        .map(|qty| {
            json!({
                "item_id": item,
                "input": { "qty": qty.to_string(), "unit_code": "g", "base_qty_per_unit": "1" },
                "produced_on": "2026-10-01", "expires_on": "2026-10-20", "line_cost_cents": 100,
            })
        })
        .collect();
    let data = ok(
        router,
        &STAFF,
        "/api/v1/receipts",
        json!({
            "command_id": cmd(n), "supplier_id": supplier, "lines": lines,
            "occurred_at": now - lag, "clock_synced_at": now - lag,
        }),
    )
    .await;
    data["receipt"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line["lot_id"].as_str().unwrap().to_owned())
        .collect()
}

/// 报损一行，原因 EXPIRED，带不足确认；`lot` 为指定批次。
async fn waste(router: &Router, n: u16, now: i64, item: &str, lot: Option<&str>, qty: i64) {
    let mut line = json!({
        "item_id": item,
        "input": { "qty": qty.to_string(), "unit_code": "g", "base_qty_per_unit": "1" },
        "reason_code": "EXPIRED",
        "confirm_shortage": true,
    });
    if let Some(lot) = lot {
        line["lot_id"] = json!(lot);
    }
    ok(
        router,
        &STAFF,
        "/api/v1/waste-records",
        json!({ "command_id": cmd(n), "lines": [line], "occurred_at": now, "clock_synced_at": now }),
    )
    .await;
}

struct Stock {
    flour: String,
    sugar: String,
}

/// 在 `NOW` 建一个一致的库存账本，共 7 个事件（seq 1～7）：
/// 物料 FLOUR、SUGAR（SUGAR 没有库存），供应商，报损原因 EXPIRED；FLOUR 收货 A 5、B 10；
/// 指定 B 报损 12（确认）→ B −2；不指定批次报损 6（确认）→ FIFO 扣 A 5 → A 0，余下 1 记入账外缺口。
/// 结果：A 0、B −2，FLOUR 账外缺口 −1，账面 FLOUR −3、SUGAR 0。投影中的数量是千分之一克（B −2000）。
async fn stock(router: &Router) -> Stock {
    let flour = item(router, 1, "FLOUR").await;
    let sugar = item(router, 2, "SUGAR").await;
    let supplier = supplier(router, 3).await;
    ok(
        router,
        &MANAGER,
        "/api/v1/waste-reasons",
        json!({ "command_id": cmd(4), "code": "EXPIRED", "name": "EXPIRED", "active": true }),
    )
    .await;
    assert_eq!(
        receive(router, 5, NOW, 0, &supplier, &flour, &[5, 10]).await,
        [A, B]
    );
    waste(router, 6, NOW, &flour, Some(B), 12).await;
    waste(router, 7, NOW, &flour, None, 6).await;
    Stock { flour, sugar }
}

// ---------------------------------------------------------------- 检查项

// 「批次余量为负是合法状态」「第 3 项照常执行」：空账本与零库存、负批次余量、账外缺口组合的账本都通过。
// 启动检查在空账本上 max_seq 为 0；测试入口的检查在建账之后 max_seq 为 7。两个 finished_at 相同。
#[tokio::test]
async fn consistent_ledger_with_negative_lot_passes() {
    let fixture = Fixture::new(NOW);
    let (node, startup) = fixture.start().await;
    assert_eq!(startup, passed(NOW, 0));
    stock(&node.router).await;

    fixture.set(NOW + HOUR);
    assert_eq!(run(&node).await, passed(NOW + HOUR, 7));
    assert_eq!(health(&node.router).await["status"], json!("ok"));
    node.shutdown().await.unwrap();
}

// 启动检查发现违规：A 的余量被改成 7 g（存 7000；流水合计 0）；FLOUR 的账外缺口行被删除（流水合计 −1，缺行按 0）；
// SUGAR 多出一行账外缺口 −4（没有流水，按 0）。违规按「检查码 + 对象」计数，样本按检查顺序、再按 subject 排列；
// 样本的 actual、expected 是基本单位的小数字符串（docs/invariant-check.md「检查项」）。
// 账面视图由表计算，与表一致，第 3 项无违规。violated 使 status 为 degraded。
// 「检查不修改数据库，不自动修复」：检查前后账本、幂等记录和库存投影逐行相同。
// 「其他降级原因照常生效」的反面：之后的整点备份成功，不清除自检造成的降级。violated 不阻止写入。
#[tokio::test]
async fn startup_check_reports_lot_and_unallocated_mismatches() {
    let fixture = Fixture::new(NOW);
    let (node, _) = fixture.start().await;
    let Stock { flour, sugar } = stock(&node.router).await;
    node.shutdown().await.unwrap();
    fixture.corrupt(&format!(
        "UPDATE inventory_lots SET remaining_qty = 7000 WHERE lot_id = '{A}';
         DELETE FROM inventory_unallocated WHERE item_id = '{flour}';
         INSERT INTO inventory_unallocated (item_id, qty) VALUES ('{sugar}', -4000);"
    ));
    let before = fixture.state();

    fixture.set(NOW + HOUR);
    let (node, startup) = fixture.start().await;
    assert_eq!(fixture.state(), before);
    let mut samples = vec![lot_sample(&flour, A, "7", "0")];
    samples.extend(by_item(vec![
        item_sample(UNALLOCATED, &flour, "0", "-1"),
        item_sample(UNALLOCATED, &sugar, "-4", "0"),
    ]));
    assert_eq!(
        startup,
        json!({
            "last_attempt": attempt(NOW + HOUR, "violated"),
            "last_report": report(NOW + HOUR, 7, [1, 2, 0], samples, false),
        })
    );
    assert_eq!(health(&node.router).await["status"], json!("degraded"));

    fixture.set(NOW + 2 * HOUR);
    let data = wait_for_health(&node.router, "hourly backup", |d| {
        d["last_backup_ok_at"] == json!(NOW + 2 * HOUR)
    })
    .await;
    assert_eq!(data["last_backup_failed_at"], Value::Null);
    assert_eq!(data["invariant_check"], startup);
    assert_eq!(data["status"], json!("degraded"));

    ok(
        &node.router,
        &MANAGER,
        "/api/v1/equipment",
        json!({
            "command_id": cmd(8), "code": "F1", "name": "Walk-in",
            "equipment_type": "FRIDGE", "active": true,
        }),
    )
    .await;
    assert_eq!(invariant(&node.router).await, startup);
    node.shutdown().await.unwrap();
}

// 「物料有 lot_id IS NULL 的流水、合计为 0、没有账外缺口行，也是合法状态」：SUGAR 有一条被吸收的报损流水
// （qty_delta = 0，形状同 domain.md「报损接口」投影），没有账外缺口行；检查通过。
#[tokio::test]
async fn missing_unallocated_row_with_zero_total_is_legal() {
    let fixture = Fixture::new(NOW);
    let (node, _) = fixture.start().await;
    let Stock { sugar, .. } = stock(&node.router).await;
    node.shutdown().await.unwrap();
    fixture.corrupt(&format!(
        "INSERT INTO inventory_movements (event_seq, movement_no, item_id, lot_id, kind, alloc_source,
             nominal_qty, qty_delta, absorbed_by_event_id, physical_at, business_date)
         VALUES (7, 50, '{sugar}', NULL, 'WASTE', 'ABSORBED', -3000, 0,
             (SELECT id FROM store_events WHERE seq = 6), {NOW}, '2026-10-06');"
    ));

    fixture.set(NOW + HOUR);
    let (node, startup) = fixture.start().await;
    assert_eq!(startup, passed(NOW + HOUR, 7));
    node.shutdown().await.unwrap();
}

// 「对象范围」：第 1 项是 inventory_lots 的全部批次（含没有流水的），第 2 项是有账外缺口行或有 lot_id IS NULL
// 流水的全部物料；都不按物料是否停用过滤。SUGAR 先停用，再加两个没有流水的批次：余量 0 的合法，余量 3 的违规；
// 再加一条 lot_id IS NULL、qty_delta = +3 的盘点流水，没有账外缺口行（流水合计为正，按第 2 项报告）。
// FLOUR 的账外缺口行从 −1 改成 −1.5（存 −1500）：两边都有值但不相等，样本写作 "-1.5"。
#[tokio::test]
async fn object_ranges_include_lots_without_movements_and_inactive_items() {
    let fixture = Fixture::new(NOW);
    let (node, _) = fixture.start().await;
    let Stock { flour, sugar } = stock(&node.router).await;
    let reply = spec_support::put(
        &node.router,
        &format!("/api/v1/items/{sugar}"),
        Some(&MANAGER),
        &json!({
            "command_id": cmd(8), "base_revision": 1, "name": "SUGAR",
            "category": "RAW", "units": [], "active": false,
        }),
    )
    .await
    .unwrap();
    assert_success(&reply);
    node.shutdown().await.unwrap();
    fixture.corrupt(&format!(
        "INSERT INTO inventory_lots (lot_id, item_id, lot_date, lot_serial, origin, source_event_seq,
             source_line_no, remaining_qty, expires_at, manufacturer_lot_no)
         VALUES ('RAW-SUGAR-20261006-001', '{sugar}', '2026-10-06', 1, 'RECEIPT', 5, 10, 0, NULL, NULL),
                ('RAW-SUGAR-20261006-002', '{sugar}', '2026-10-06', 2, 'RECEIPT', 5, 11, 3000, NULL, NULL);
         INSERT INTO inventory_movements (event_seq, movement_no, item_id, lot_id, kind, alloc_source,
             nominal_qty, qty_delta, absorbed_by_event_id, physical_at, business_date)
         VALUES (7, 50, '{sugar}', NULL, 'ADJUST', 'COUNT', 3000, 3000, NULL, {NOW}, '2026-10-06');
         UPDATE inventory_unallocated SET qty = -1500 WHERE item_id = '{flour}';"
    ));

    fixture.set(NOW + HOUR);
    let (node, startup) = fixture.start().await;
    let mut samples = vec![lot_sample(&sugar, "RAW-SUGAR-20261006-002", "3", "0")];
    samples.extend(by_item(vec![
        item_sample(UNALLOCATED, &flour, "-1.5", "-1"),
        item_sample(UNALLOCATED, &sugar, "0", "3"),
    ]));
    assert_eq!(
        startup,
        json!({
            "last_attempt": attempt(NOW + HOUR, "violated"),
            "last_report": report(NOW + HOUR, 8, [1, 2, 0], samples, false),
        })
    );
    node.shutdown().await.unwrap();
}

// 「samples：最多 20 条，按检查顺序，同一检查项内按 subject 的字段依次比较」「violations」「violation_count」
// 「samples_truncated」：FLOUR 收货 25 行（批次 001～025，各 10 g），全部改成 11；FLOUR、SUGAR 各多出一行没有流水的
// 账外缺口（−2、−4）。共 27 条：样本是前 20 个批次，截断；样本满了之后仍完整计数。
// 运行中把批次 020～025 改回 10：共 21 条，样本是 19 个批次和排在前面的那个物料的账外缺口，截断。
// 再删掉排在后面的那一行：共 20 条，全部列出，不截断。
#[tokio::test]
async fn samples_are_ordered_and_truncated_at_twenty() {
    let fixture = Fixture::new(NOW);
    let (node, _) = fixture.start().await;
    let flour = item(&node.router, 1, "FLOUR").await;
    let sugar = item(&node.router, 2, "SUGAR").await;
    let supplier = supplier(&node.router, 3).await;
    let mut lots = receive(&node.router, 4, NOW, 0, &supplier, &flour, &[10; 25]).await;
    lots.sort();
    node.shutdown().await.unwrap();
    fixture.corrupt(&format!(
        "UPDATE inventory_lots SET remaining_qty = 11000;
         INSERT INTO inventory_unallocated (item_id, qty) VALUES ('{flour}', -2000), ('{sugar}', -4000);"
    ));
    let lot_samples: Vec<Value> = lots
        .iter()
        .map(|lot| lot_sample(&flour, lot, "11", "10"))
        .collect();
    let unallocated = by_item(vec![
        item_sample(UNALLOCATED, &flour, "-2", "0"),
        item_sample(UNALLOCATED, &sugar, "-4", "0"),
    ]);

    fixture.set(NOW + HOUR);
    let (node, startup) = fixture.start().await;
    assert_eq!(
        startup,
        json!({
            "last_attempt": attempt(NOW + HOUR, "violated"),
            "last_report": report(NOW + HOUR, 4, [25, 2, 0], lot_samples[..20].to_vec(), true),
        })
    );

    exec(
        &node,
        format!(
            "UPDATE inventory_lots SET remaining_qty = 10000 WHERE lot_id >= '{}';",
            lots[19]
        ),
    )
    .await;
    fixture.set(NOW + 2 * HOUR);
    let mut samples = lot_samples[..19].to_vec();
    samples.push(unallocated[0].clone());
    assert_eq!(
        run(&node).await,
        json!({
            "last_attempt": attempt(NOW + 2 * HOUR, "violated"),
            "last_report": report(NOW + 2 * HOUR, 4, [19, 2, 0], samples.clone(), true),
        })
    );

    let later = unallocated[1]["subject"]["item_id"].as_str().unwrap();
    exec(
        &node,
        format!("DELETE FROM inventory_unallocated WHERE item_id = '{later}';"),
    )
    .await;
    fixture.set(NOW + 3 * HOUR);
    assert_eq!(
        run(&node).await,
        json!({
            "last_attempt": attempt(NOW + 3 * HOUR, "violated"),
            "last_report": report(NOW + 3 * HOUR, 4, [19, 1, 0], samples, false),
        })
    );
    node.shutdown().await.unwrap();
}

// 「同一检查项内按 subject 的字段依次比较（字段顺序同上表，按字节序）」：第 1 项先比 item_id，再比 lot_id。
// 服务端生成的 UUIDv7 以 recorded_at 为时间部分：在 NOW 建 SUGAR、在 NOW + 1ms 建 FLOUR，SUGAR 的 item_id 较小；
// 而批次号 RAW-FLOUR-… 小于 RAW-SUGAR-…，两个排序键给出相反的顺序。
// FLOUR 先实时收货（批次日期 2026-10-06），再补交一张 48 小时前录入的收货（批次日期 2026-10-04）：
// 后建的批次号较小，表中的插入顺序与 lot_id 顺序相反。三个批次的余量各加 0.001 g（存储值 + 1），样本写作 "3.001" 等。
#[tokio::test]
async fn samples_sort_by_item_id_then_lot_id() {
    let fixture = Fixture::new(NOW);
    let (node, _) = fixture.start().await;
    let sugar = item(&node.router, 1, "SUGAR").await;
    fixture.set(NOW + 1);
    let flour = item(&node.router, 2, "FLOUR").await;
    assert!(sugar < flour, "{sugar} < {flour}");
    let supplier = supplier(&node.router, 3).await;
    let flour_new = receive(&node.router, 4, NOW + 1, 0, &supplier, &flour, &[5]).await;
    let flour_old = receive(&node.router, 5, NOW + 1, 48 * HOUR, &supplier, &flour, &[7]).await;
    let sugar_lot = receive(&node.router, 6, NOW + 1, 0, &supplier, &sugar, &[3]).await;
    assert_eq!(flour_new, ["RAW-FLOUR-20261006-001"]);
    assert_eq!(flour_old, ["RAW-FLOUR-20261004-001"]);
    assert_eq!(sugar_lot, ["RAW-SUGAR-20261006-001"]);

    exec(
        &node,
        "UPDATE inventory_lots SET remaining_qty = remaining_qty + 1;".to_owned(),
    )
    .await;
    fixture.set(NOW + HOUR);
    let samples = vec![
        lot_sample(&sugar, "RAW-SUGAR-20261006-001", "3.001", "3"),
        lot_sample(&flour, "RAW-FLOUR-20261004-001", "7.001", "7"),
        lot_sample(&flour, "RAW-FLOUR-20261006-001", "5.001", "5"),
    ];
    assert_eq!(
        run(&node).await,
        json!({
            "last_attempt": attempt(NOW + HOUR, "violated"),
            "last_report": report(NOW + HOUR, 6, [3, 0, 0], samples, false),
        })
    );
    node.shutdown().await.unwrap();
}

// ---------------------------------------------------------------- 报告的更新

// 运行中经唯一写连接制造异常：A 改成 7 g → violated，degraded。
// 「任一汇总超出 i64：本次检查失败」：再给 A 加两条 qty_delta = i64::MAX 的流水 → failed，只更新 last_attempt，
// last_report 保持上一份 violated 报告，仍 degraded。
// 修复后把时钟回拨到早于上述两次的时刻再检查：passed 照样覆盖之前的结果（按执行先后，不比较时间戳），status 回到 ok。
#[tokio::test]
async fn failure_keeps_the_last_report_and_a_repair_clears_it() {
    let fixture = Fixture::new(NOW);
    let (node, _) = fixture.start().await;
    let Stock { flour, .. } = stock(&node.router).await;

    exec(
        &node,
        format!("UPDATE inventory_lots SET remaining_qty = 7000 WHERE lot_id = '{A}';"),
    )
    .await;
    fixture.set(NOW + 2 * HOUR);
    let violated = report(
        NOW + 2 * HOUR,
        7,
        [1, 0, 0],
        vec![lot_sample(&flour, A, "7", "0")],
        false,
    );
    assert_eq!(
        run(&node).await,
        json!({ "last_attempt": attempt(NOW + 2 * HOUR, "violated"), "last_report": violated })
    );
    assert_eq!(health(&node.router).await["status"], json!("degraded"));

    let max = i64::MAX;
    exec(
        &node,
        format!(
            "INSERT INTO inventory_movements (event_seq, movement_no, item_id, lot_id, kind, alloc_source,
                 nominal_qty, qty_delta, absorbed_by_event_id, physical_at, business_date)
             VALUES (5, 100, '{flour}', '{A}', 'RECEIPT', 'NEW_LOT', {max}, {max}, NULL, {NOW}, '2026-10-06'),
                    (5, 101, '{flour}', '{A}', 'RECEIPT', 'NEW_LOT', {max}, {max}, NULL, {NOW}, '2026-10-06');"
        ),
    )
    .await;
    fixture.set(NOW + 3 * HOUR);
    assert_eq!(
        run(&node).await,
        json!({ "last_attempt": attempt(NOW + 3 * HOUR, "failed"), "last_report": violated })
    );
    assert_eq!(health(&node.router).await["status"], json!("degraded"));

    exec(
        &node,
        format!(
            "DELETE FROM inventory_movements WHERE event_seq = 5 AND movement_no IN (100, 101);
             UPDATE inventory_lots SET remaining_qty = 0 WHERE lot_id = '{A}';"
        ),
    )
    .await;
    fixture.set(NOW + 90 * MINUTE);
    assert_eq!(run(&node).await, passed(NOW + 90 * MINUTE, 7));
    assert_eq!(health(&node.router).await["status"], json!("ok"));
    node.shutdown().await.unwrap();
}

// 「失败只更新 last_attempt」「两个字段为 null 时……」：启动前给 A 加两条 qty_delta = i64::MAX 的流水，启动检查
// 失败，本进程还没有完整报告，last_report 为 null；status 为 degraded。修复后 passed。
// 再让 FLOUR 的 lot_id IS NULL 流水合计小于 i64::MIN（第 2 项溢出）：failed，保留那份 passed 报告，degraded。
#[tokio::test]
async fn first_failure_degrades_and_a_later_failure_keeps_the_passed_report() {
    let fixture = Fixture::new(NOW);
    let (node, _) = fixture.start().await;
    let Stock { flour, .. } = stock(&node.router).await;
    node.shutdown().await.unwrap();
    let max = i64::MAX;
    fixture.corrupt(&format!(
        "INSERT INTO inventory_movements (event_seq, movement_no, item_id, lot_id, kind, alloc_source,
             nominal_qty, qty_delta, absorbed_by_event_id, physical_at, business_date)
         VALUES (5, 100, '{flour}', '{A}', 'RECEIPT', 'NEW_LOT', {max}, {max}, NULL, {NOW}, '2026-10-06'),
                (5, 101, '{flour}', '{A}', 'RECEIPT', 'NEW_LOT', {max}, {max}, NULL, {NOW}, '2026-10-06');"
    ));

    fixture.set(NOW + HOUR);
    let (node, startup) = fixture.start().await;
    assert_eq!(
        startup,
        json!({ "last_attempt": attempt(NOW + HOUR, "failed"), "last_report": null })
    );
    assert_eq!(health(&node.router).await["status"], json!("degraded"));

    exec(
        &node,
        "DELETE FROM inventory_movements WHERE event_seq = 5 AND movement_no IN (100, 101);"
            .to_owned(),
    )
    .await;
    fixture.set(NOW + 2 * HOUR);
    assert_eq!(run(&node).await, passed(NOW + 2 * HOUR, 7));
    assert_eq!(health(&node.router).await["status"], json!("ok"));

    let min = i64::MIN;
    exec(
        &node,
        format!(
            "INSERT INTO inventory_movements (event_seq, movement_no, item_id, lot_id, kind, alloc_source,
                 nominal_qty, qty_delta, absorbed_by_event_id, physical_at, business_date)
             VALUES (7, 100, '{flour}', NULL, 'WASTE', 'SHORTFALL', {min}, {min}, NULL, {NOW}, '2026-10-06'),
                    (7, 101, '{flour}', NULL, 'WASTE', 'SHORTFALL', {min}, {min}, NULL, {NOW}, '2026-10-06');"
        ),
    )
    .await;
    fixture.set(NOW + 3 * HOUR);
    assert_eq!(
        run(&node).await,
        json!({
            "last_attempt": attempt(NOW + 3 * HOUR, "failed"),
            "last_report": report(NOW + 2 * HOUR, 7, [0, 0, 0], vec![], false),
        })
    );
    assert_eq!(health(&node.router).await["status"], json!("degraded"));
    node.shutdown().await.unwrap();
}

// 「不发布半份报告」「当前每次检查执行表中全部检查项」：A 改成 7 g → violated。再给 FLOUR 加两条 lot_id IS NULL、
// qty_delta = i64::MIN 的流水：第 1 项照常发现 A 的违规，第 2 项的流水之和超出 i64 → failed。前面第 1 项已发现的违规不发布，
// last_report 仍是上一份。删掉这两条流水、修复 A 后 passed。
// （批次余量和账外缺口受库存状态上限的表约束限制，第 3 项的汇总造不出溢出；溢出只经不受范围约束的流水制造。）
#[tokio::test]
async fn a_later_check_failure_publishes_no_partial_report() {
    let fixture = Fixture::new(NOW);
    let (node, _) = fixture.start().await;
    let Stock { flour, .. } = stock(&node.router).await;
    exec(
        &node,
        format!("UPDATE inventory_lots SET remaining_qty = 7000 WHERE lot_id = '{A}';"),
    )
    .await;
    fixture.set(NOW + HOUR);
    let violated = report(
        NOW + HOUR,
        7,
        [1, 0, 0],
        vec![lot_sample(&flour, A, "7", "0")],
        false,
    );
    assert_eq!(
        run(&node).await,
        json!({ "last_attempt": attempt(NOW + HOUR, "violated"), "last_report": violated })
    );

    let min = i64::MIN;
    exec(
        &node,
        format!(
            "INSERT INTO inventory_movements (event_seq, movement_no, item_id, lot_id, kind, alloc_source,
                 nominal_qty, qty_delta, absorbed_by_event_id, physical_at, business_date)
             VALUES (7, 100, '{flour}', NULL, 'WASTE', 'SHORTFALL', {min}, {min}, NULL, {NOW}, '2026-10-06'),
                    (7, 101, '{flour}', NULL, 'WASTE', 'SHORTFALL', {min}, {min}, NULL, {NOW}, '2026-10-06');"
        ),
    )
    .await;
    fixture.set(NOW + 2 * HOUR);
    assert_eq!(
        run(&node).await,
        json!({ "last_attempt": attempt(NOW + 2 * HOUR, "failed"), "last_report": violated })
    );
    assert_eq!(health(&node.router).await["status"], json!("degraded"));

    exec(
        &node,
        format!(
            "DELETE FROM inventory_movements WHERE event_seq = 7 AND movement_no IN (100, 101);
             UPDATE inventory_lots SET remaining_qty = 0 WHERE lot_id = '{A}';"
        ),
    )
    .await;
    fixture.set(NOW + 3 * HOUR);
    assert_eq!(run(&node).await, passed(NOW + 3 * HOUR, 7));
    assert_eq!(health(&node.router).await["status"], json!("ok"));
    node.shutdown().await.unwrap();
}

// 「之后一次 passed 清除自检造成的降级，其他降级原因照常生效」：时钟回拨使 clock_regression_ms 超过 300000，
// 同时自检 violated；修复后自检 passed，clock_regression_ms 仍超过阈值，status 仍为 degraded。
// 时钟回到 NOW 后两个原因都不存在，status 为 ok（/health 不单独列出降级原因）。
#[tokio::test]
async fn a_passing_check_keeps_other_degradation() {
    let fixture = Fixture::new(NOW);
    let (node, _) = fixture.start().await;
    let Stock { flour, .. } = stock(&node.router).await;

    exec(
        &node,
        format!("UPDATE inventory_lots SET remaining_qty = 7000 WHERE lot_id = '{A}';"),
    )
    .await;
    fixture.set(NOW - 300_001);
    assert_eq!(
        run(&node).await,
        json!({
            "last_attempt": attempt(NOW - 300_001, "violated"),
            "last_report": report(NOW - 300_001, 7, [1, 0, 0], vec![lot_sample(&flour, A, "7", "0")], false),
        })
    );
    let data = health(&node.router).await;
    assert_eq!(data["clock_regression_ms"], json!(300_001));
    assert_eq!(data["status"], json!("degraded"));

    exec(
        &node,
        format!("UPDATE inventory_lots SET remaining_qty = 0 WHERE lot_id = '{A}';"),
    )
    .await;
    fixture.set(NOW - 400_000);
    assert_eq!(run(&node).await, passed(NOW - 400_000, 7));
    let data = health(&node.router).await;
    assert_eq!(data["clock_regression_ms"], json!(400_000));
    assert_eq!(data["status"], json!("degraded"));

    fixture.set(NOW);
    let data = health(&node.router).await;
    assert_eq!(data["clock_regression_ms"], json!(0));
    assert_eq!(data["invariant_check"], passed(NOW - 400_000, 7));
    assert_eq!(data["status"], json!("ok"));
    node.shutdown().await.unwrap();
}

// 「读取 /health 不触发检查」：时钟前进后反复读取，结果仍是启动检查的。
#[tokio::test]
async fn reading_health_does_not_run_a_check() {
    let fixture = Fixture::new(NOW);
    let (node, startup) = fixture.start().await;
    fixture.set(NOW + 30 * MINUTE);
    for _ in 0..5 {
        health(&node.router).await;
    }
    settle().await;
    assert_eq!(invariant(&node.router).await, startup);
    assert_eq!(startup, passed(NOW, 0));
    node.shutdown().await.unwrap();
}

// ---------------------------------------------------------------- 闭店与关闭

// 闭店流程「闭店备份 → 备份结果交给 /health → 自检 → 自检结果交给 /health」，在非空库存上执行全部检查：
// 23:10 建账并把 A 改成 7。闭店备份暂停期间，自检不开始；备份释放后自检开始时，备份结果已在 /health 中，
// 自检结果仍是启动检查的。「自检结束后入口才开始下一个排队的备份」：自检暂停期间到了次日 00:00，整点备份不开始。
// 「finished_at 是检查结束时的 now()」：自检暂停期间时钟已到 00:00，释放后两个 finished_at 都是 00:00。
// 之后整点备份执行；「每小时备份不触发自检」：自检结果不变。
#[tokio::test]
async fn closing_backs_up_before_checking() {
    let fixture = Fixture::new(local(23, 10));
    let (node, startup) = fixture.start().await;
    assert_eq!(startup, passed(local(23, 10), 0));
    let Stock { flour, .. } = stock(&node.router).await;
    exec(
        &node,
        format!("UPDATE inventory_lots SET remaining_qty = 7000 WHERE lot_id = '{A}';"),
    )
    .await;

    let backup_hold = node.hold_backups();
    let check_hold = node.hold_invariant_checks();
    fixture.set(local(23, 30));
    tokio::time::timeout(Duration::from_secs(10), backup_hold.started())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(300), check_hold.started())
            .await
            .is_err()
    );

    drop(backup_hold);
    tokio::time::timeout(Duration::from_secs(10), check_hold.started())
        .await
        .unwrap();
    let data = health(&node.router).await;
    assert_eq!(data["last_backup_ok_at"], json!(local(23, 30)));
    assert_eq!(data["invariant_check"], startup);

    fixture.set(local(24, 0));
    settle().await;
    let dir = fixture.backups();
    assert_eq!(backup_numbers(&dir).unwrap(), [1]);
    assert_eq!(
        health(&node.router).await["last_backup_ok_at"],
        json!(local(23, 30))
    );

    drop(check_hold);
    let closing = json!({
        "last_attempt": attempt(local(24, 0), "violated"),
        "last_report": report(local(24, 0), 7, [1, 0, 0], vec![lot_sample(&flour, A, "7", "0")], false),
    });
    let data = wait_for_health(&node.router, "closing check", |d| {
        !d["invariant_check"]["last_attempt"]["finished_at"].eq(&json!(local(23, 10)))
    })
    .await;
    assert_eq!(data["invariant_check"], closing);

    wait_until("hourly backup", || backup_numbers(&dir).unwrap() == [1, 2]).await;
    wait_for_health(&node.router, "hourly backup result", |d| {
        d["last_backup_ok_at"] == json!(local(24, 0)) && d["last_backup_seq"] == json!(7)
    })
    .await;
    settle().await;
    assert_eq!(invariant(&node.router).await, closing);
    node.shutdown().await.unwrap();
}

// 「备份失败也执行」：已有序号 u64::MAX 的备份文件，闭店备份分配序号溢出而失败；自检开始时失败已在 /health 中，
// 自检照常执行并通过。两项结果分别报告：status 因备份失败为 degraded。
#[tokio::test]
async fn failed_closing_backup_still_checks() {
    let fixture = Fixture::new(local(23, 10));
    fs::create_dir_all(fixture.backups()).unwrap();
    fs::write(
        fixture.backups().join(format!(
            "boh-{TEST_STORE_ID}-20260101T000000Z-18446744073709551615-01890a5d-ac96-774b-bcce-b30209f20001.db"
        )),
        b"not a database",
    )
    .unwrap();
    let (node, _) = fixture.start().await;

    let hold = node.hold_invariant_checks();
    fixture.set(local(23, 30));
    tokio::time::timeout(Duration::from_secs(10), hold.started())
        .await
        .unwrap();
    let data = health(&node.router).await;
    assert_eq!(data["last_backup_failed_at"], json!(local(23, 30)));
    assert_eq!(data["last_backup_ok_at"], Value::Null);

    drop(hold);
    let data = wait_for_health(&node.router, "closing check", |d| {
        d["invariant_check"]["last_attempt"]["finished_at"] == json!(local(23, 30))
    })
    .await;
    assert_eq!(data["invariant_check"], passed(local(23, 30), 0));
    assert_eq!(data["status"], json!("degraded"));
    node.shutdown().await.unwrap();
}

// 「关闭：执行完正在进行的检查……再关写线程」：自检停在执行中时，shutdown 不返回；释放后正常返回，
// 闭店备份已完成。
#[tokio::test]
async fn shutdown_waits_for_the_running_check() {
    let fixture = Fixture::new(local(23, 10));
    let (node, _) = fixture.start().await;
    let hold = node.hold_invariant_checks();
    fixture.set(local(23, 30));
    tokio::time::timeout(Duration::from_secs(10), hold.started())
        .await
        .unwrap();

    let mut shutdown = pin!(node.shutdown());
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut shutdown)
            .await
            .is_err()
    );
    drop(hold);
    tokio::time::timeout(Duration::from_secs(10), shutdown)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(backup_numbers(&fixture.backups()).unwrap(), [1]);
}

// 「已接受的闭店流程」：闭店备份暂停期间开始关闭；释放备份后，这个闭店流程的自检仍然开始，shutdown 继续等待；
// 释放自检后 shutdown 返回，闭店备份已完成。
#[tokio::test]
async fn shutdown_finishes_an_accepted_closing_flow() {
    let fixture = Fixture::new(local(23, 10));
    let (node, _) = fixture.start().await;
    let backup_hold = node.hold_backups();
    let check_hold = node.hold_invariant_checks();
    fixture.set(local(23, 30));
    tokio::time::timeout(Duration::from_secs(10), backup_hold.started())
        .await
        .unwrap();

    let mut shutdown = pin!(node.shutdown());
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut shutdown)
            .await
            .is_err()
    );
    drop(backup_hold);
    tokio::time::timeout(Duration::from_secs(10), check_hold.started())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut shutdown)
            .await
            .is_err()
    );
    drop(check_hold);
    tokio::time::timeout(Duration::from_secs(10), shutdown)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(backup_numbers(&fixture.backups()).unwrap(), [1]);
}
