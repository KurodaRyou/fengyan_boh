//! 锁定测试：批次号。规则见 docs/domain.md「批次」「收货接口」「时间」「验收用例」，AGENTS.md「幂等」「ID 与时间」，
//! 接口见 docs/interfaces.md。批次号的格式、批次日期、流水号与到期提醒的比较范围。

mod spec_support;

use std::path::PathBuf;

use axum::Router;
use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use boh_storage::rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use spec_support::{JsonReply, MANAGER, STAFF, assert_error, assert_success};
use tempfile::TempDir;

const NOW: i64 = 1_791_248_400_000; // 2026-10-06 09:00 +08:00
const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;
/// 平板时钟比节点快 7 分钟，相对校准应抵消这个偏差。
const SKEW: i64 = 7 * MINUTE;
/// 2026-09-10 08:00 +08:00。
const AT_09_10_0800: i64 = 1_788_998_400_000;
/// 2026-09-10 14:00 +08:00。
const AT_09_10_1400: i64 = 1_789_020_000_000;
/// 2026-09-11 09:00 +08:00。
const AT_09_11_0900: i64 = 1_789_088_400_000;
/// 2026-09-12 10:00 +08:00。
const AT_09_12_1000: i64 = 1_789_178_400_000;
/// 2026-10-09 00:10 +08:00（营业日 2026-10-08）。
const AT_10_09_0010: i64 = 1_791_475_800_000;

struct Node {
    _dir: TempDir,
    db_path: PathBuf,
    clock: ManualClock,
    router: Router,
}

#[allow(clippy::unwrap_used)] // 测试夹具：临时目录或 Router 构造失败时测试无法开始，直接终止。
fn node() -> Node {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(UnixMillis(NOW));
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    Node {
        _dir: dir,
        db_path,
        clock,
        router,
    }
}

/// 第 `n` 个命令 ID（UUIDv7）。
fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209d1{n:04x}")
}

/// 收货请求的一行：`qty` 克，生产日期 2026-09-01，到期日 `expires_on`。
fn line(item_id: &str, qty: i64, expires_on: &str) -> Value {
    json!({
        "item_id": item_id,
        "input": { "qty": qty, "unit_code": "g", "base_qty_per_unit": 1 },
        "produced_on": "2026-09-01", "expires_on": expires_on, "line_cost_cents": 100,
    })
}

/// 成功响应中各行的批次号。
fn lot_ids(reply: &JsonReply) -> Vec<String> {
    assert_success(reply)["receipt"]["lines"]
        .as_array()
        .unwrap_or_else(|| panic!("receipt lines missing: {}", reply.body))
        .iter()
        .map(|line| line["lot_id"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// `inventory_lots` 的一行：(lot_id, item_id, lot_date, lot_serial, source_event_seq, source_line_no)。
type LotRow = (String, String, String, i64, i64, i64);

impl Node {
    #[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态的请求失败时，后续步骤没有意义，直接终止测试。
    async fn master(&self, uri: &str, body: Value) -> Value {
        let reply = spec_support::post(&self.router, uri, Some(&MANAGER), &body)
            .await
            .unwrap();
        assert_success(&reply).clone()
    }

    /// 新建一个以克为基本单位、没有其他单位的物料。
    #[allow(clippy::unwrap_used)] // 同上。
    async fn item(&self, n: u16, code: &str, category: &str) -> String {
        let data = self
            .master(
                "/api/v1/items",
                json!({
                    "command_id": cmd(n), "code": code, "name": code, "base_unit": "g",
                    "category": category, "units": [], "active": true,
                }),
            )
            .await;
        data["item"]["item_id"].as_str().unwrap().to_owned()
    }

    /// 修改物料分类。
    #[allow(clippy::unwrap_used)] // 同上。
    async fn set_category(&self, n: u16, item_id: &str, base_revision: i64, category: &str) {
        let reply = spec_support::put(
            &self.router,
            &format!("/api/v1/items/{item_id}"),
            Some(&MANAGER),
            &json!({
                "command_id": cmd(n), "base_revision": base_revision, "name": "FLOUR",
                "category": category, "units": [], "active": true,
            }),
        )
        .await
        .unwrap();
        assert_success(&reply);
    }

    #[allow(clippy::unwrap_used)] // 同上。
    async fn supplier(&self, n: u16) -> String {
        let data = self
            .master(
                "/api/v1/suppliers",
                json!({ "command_id": cmd(n), "code": "S1", "name": "S1", "active": true }),
            )
            .await;
        data["supplier"]["supplier_id"].as_str().unwrap().to_owned()
    }

    /// 节点时钟设为 `recorded_at` 后收货：平板在 `recorded_at + SKEW − lag` 录入、在 `recorded_at + SKEW` 发送，
    /// 所以 `occurred_at = recorded_at − lag`。返回完整响应。
    #[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试。
    async fn receive(
        &self,
        n: u16,
        supplier: &str,
        lines: Vec<Value>,
        recorded_at: i64,
        lag: i64,
    ) -> JsonReply {
        self.clock.set(UnixMillis(recorded_at));
        let sent_at = recorded_at + SKEW;
        spec_support::post(
            &self.router,
            "/api/v1/receipts",
            Some(&STAFF),
            &json!({
                "command_id": cmd(n), "supplier_id": supplier, "lines": lines,
                "captured_at": sent_at - lag, "sent_at": sent_at,
            }),
        )
        .await
        .unwrap()
    }

    /// 某个物料在库存明细中的批次号，按查询返回的顺序（FIFO）。
    #[allow(clippy::unwrap_used)] // 测试夹具：查询失败已违反接口约定，直接终止测试。
    async fn fifo(&self, item_id: &str) -> Vec<String> {
        let reply = spec_support::get(
            &self.router,
            &format!("/api/v1/inventory?item_id={item_id}"),
            Some(&STAFF),
        )
        .await
        .unwrap();
        assert_success(&reply)["items"][0]["lots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|lot| lot["lot_id"].as_str().unwrap().to_owned())
            .collect()
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：只读查询失败时无法比对状态，直接终止测试。
    fn lots(&self) -> Vec<LotRow> {
        let reader = spec_support::reader(&self.db_path).unwrap();
        let mut statement = reader
            .prepare(
                "SELECT lot_id, item_id, lot_date, lot_serial, source_event_seq, source_line_no
                 FROM inventory_lots ORDER BY source_event_seq, source_line_no",
            )
            .unwrap();
        statement
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// 写入后可能变化的全部内容：账本、幂等记录和库存投影。
    #[allow(clippy::unwrap_used)] // 同上。
    fn state(&self) -> Vec<Vec<Vec<SqlValue>>> {
        let reader = spec_support::reader(&self.db_path).unwrap();
        [
            "SELECT * FROM store_events ORDER BY seq",
            "SELECT * FROM processed_commands ORDER BY command_id",
            "SELECT * FROM inventory_lots ORDER BY lot_id",
            "SELECT * FROM inventory_movements ORDER BY event_seq, movement_no",
        ]
        .iter()
        .map(|sql| spec_support::rows(&reader, sql).unwrap())
        .collect()
    }
}

// 验收用例「批次日期与流水号」：面粉（RAW）09-10 08:00 收货一行；09-10 14:00 一次收货三行（面粉、黄油、面粉）；
// 09-11 收货一行；09-12 10:00 用 72 小时 lag 补一张 09-09 10:00 的收货，再用 50 小时 lag 补一张 09-10 08:00 的收货。
// - 批次号依次为 RAW-FLOUR-20260910-001；…-20260910-002、SEMI-BUTTER-20260910-001、RAW-FLOUR-20260910-003；
//   …-20260911-001；…-20260909-001；…-20260910-004（旧日期按已有最大流水号 + 1）。
// - 同一命令内按行序取号；不同物料各自从 001 开始；黄油是 SEMI，类型段取物料分类。
// - 投影的 lot_date、lot_serial 由批次号拆出；source_event_seq、source_line_no 是建批次的事件和行下标。
// - 库存明细按 FIFO（批次日期、流水号）排列：补录的 09-09 批次排最前，09-10 的四个批次按流水号，最后是 09-11。
#[tokio::test]
async fn lot_numbers_follow_lot_date_and_entry_order() {
    let node = node();
    node.clock.set(UnixMillis(AT_09_10_0800));
    let flour = node.item(1, "FLOUR", "RAW").await;
    let butter = node.item(2, "BUTTER", "SEMI").await;
    let supplier = node.supplier(3).await;
    let exp = "2026-12-31";

    let first = node
        .receive(4, &supplier, vec![line(&flour, 1, exp)], AT_09_10_0800, 0)
        .await;
    let second = node
        .receive(
            5,
            &supplier,
            vec![
                line(&flour, 2, exp),
                line(&butter, 3, exp),
                line(&flour, 4, exp),
            ],
            AT_09_10_1400,
            0,
        )
        .await;
    let third = node
        .receive(6, &supplier, vec![line(&flour, 5, exp)], AT_09_11_0900, 0)
        .await;
    let backfill = node
        .receive(
            7,
            &supplier,
            vec![line(&flour, 6, exp)],
            AT_09_12_1000,
            72 * HOUR,
        )
        .await;
    let same_day = node
        .receive(
            8,
            &supplier,
            vec![line(&flour, 7, exp)],
            AT_09_12_1000,
            50 * HOUR,
        )
        .await;

    assert_eq!(lot_ids(&first), ["RAW-FLOUR-20260910-001"]);
    assert_eq!(
        lot_ids(&second),
        [
            "RAW-FLOUR-20260910-002",
            "SEMI-BUTTER-20260910-001",
            "RAW-FLOUR-20260910-003"
        ]
    );
    assert_eq!(lot_ids(&third), ["RAW-FLOUR-20260911-001"]);
    assert_eq!(lot_ids(&backfill), ["RAW-FLOUR-20260909-001"]);
    assert_eq!(
        assert_success(&backfill)["receipt"]["business_date"],
        json!("2026-09-09")
    );
    assert_eq!(lot_ids(&same_day), ["RAW-FLOUR-20260910-004"]);
    let row = |lot: &str, item: &str, date: &str, serial: i64, seq: i64, line_no: i64| {
        (
            lot.to_owned(),
            item.to_owned(),
            date.to_owned(),
            serial,
            seq,
            line_no,
        )
    };
    assert_eq!(
        node.lots(),
        [
            row("RAW-FLOUR-20260910-001", &flour, "2026-09-10", 1, 4, 0),
            row("RAW-FLOUR-20260910-002", &flour, "2026-09-10", 2, 5, 0),
            row("SEMI-BUTTER-20260910-001", &butter, "2026-09-10", 1, 5, 1),
            row("RAW-FLOUR-20260910-003", &flour, "2026-09-10", 3, 5, 2),
            row("RAW-FLOUR-20260911-001", &flour, "2026-09-11", 1, 6, 0),
            row("RAW-FLOUR-20260909-001", &flour, "2026-09-09", 1, 7, 0),
            row("RAW-FLOUR-20260910-004", &flour, "2026-09-10", 4, 8, 0),
        ]
    );
    assert_eq!(
        node.fifo(&flour).await,
        [
            "RAW-FLOUR-20260909-001",
            "RAW-FLOUR-20260910-001",
            "RAW-FLOUR-20260910-002",
            "RAW-FLOUR-20260910-003",
            "RAW-FLOUR-20260910-004",
            "RAW-FLOUR-20260911-001",
        ]
    );
}

// 验收用例「午夜前后的批次日期」：节点时间 10-09 00:10。lag 20 分钟，occurred_at 为 10-08 23:50，批次日期 20261008；
// lag 5 分钟，occurred_at 为 10-09 00:05，批次日期 20261009。批次日期是当地日历日期，不是营业日：两次的营业日都是 10-08（日切 04:00）。
// 之后再收一张 lag 30 分钟（10-08 23:40）的：实物更早，但入账在后，流水号为 002（同一天内不还原实物先后）。
#[tokio::test]
async fn lot_date_is_the_local_calendar_date_of_occurred_at() {
    let node = node();
    let flour = node.item(1, "FLOUR", "RAW").await;
    let supplier = node.supplier(2).await;
    let exp = "2026-10-20";

    let before_midnight = node
        .receive(
            3,
            &supplier,
            vec![line(&flour, 1, exp)],
            AT_10_09_0010,
            20 * MINUTE,
        )
        .await;
    let after_midnight = node
        .receive(
            4,
            &supplier,
            vec![line(&flour, 1, exp)],
            AT_10_09_0010,
            5 * MINUTE,
        )
        .await;
    let earlier_but_later = node
        .receive(
            5,
            &supplier,
            vec![line(&flour, 1, exp)],
            AT_10_09_0010,
            30 * MINUTE,
        )
        .await;

    for (reply, occurred_at, lot) in [
        (
            &before_midnight,
            AT_10_09_0010 - 20 * MINUTE,
            "RAW-FLOUR-20261008-001",
        ),
        (
            &after_midnight,
            AT_10_09_0010 - 5 * MINUTE,
            "RAW-FLOUR-20261009-001",
        ),
        (
            &earlier_but_later,
            AT_10_09_0010 - 30 * MINUTE,
            "RAW-FLOUR-20261008-002",
        ),
    ] {
        let receipt = &assert_success(reply)["receipt"];
        assert_eq!(receipt["occurred_at"], json!(occurred_at), "{lot}");
        assert_eq!(receipt["business_date"], json!("2026-10-08"), "{lot}");
        assert_eq!(lot_ids(reply), [lot]);
    }
}

// 验收用例「流水号用尽」：同一物料同一天已有 …-999 时再收一行：409 LOT_SERIAL_EXHAUSTED，details 为
// {line, item_id, lot_date: 'YYYY-MM-DD'}，line 是命中的行；整条命令不入账（同一命令中另一物料的行也不入账）。
// - 一次收货 999 行可以用到 …-999；幂等检查先于业务校验：用原 command_id、原内容重发仍返回首次响应。
// - 其他物料同一天、同一物料其他日期照常从 001 开始。
#[tokio::test]
async fn serial_999_is_the_last_one() {
    let node = node();
    let flour = node.item(1, "FLOUR", "RAW").await;
    let butter = node.item(2, "BUTTER", "RAW").await;
    let supplier = node.supplier(3).await;
    let exp = "2026-10-20";
    let full: Vec<Value> = (0..999).map(|_| line(&flour, 1, exp)).collect();

    let first = node.receive(4, &supplier, full.clone(), NOW, 0).await;
    let lots = lot_ids(&first);
    assert_eq!(lots.len(), 999);
    assert_eq!(lots[0], "RAW-FLOUR-20261006-001");
    assert_eq!(lots[998], "RAW-FLOUR-20261006-999");
    let before = node.state();

    let reply = node
        .receive(
            5,
            &supplier,
            vec![line(&butter, 1, exp), line(&flour, 1, exp)],
            NOW,
            0,
        )
        .await;
    assert_eq!(
        assert_error(&reply, 409, "LOT_SERIAL_EXHAUSTED"),
        &json!({ "line": 1, "item_id": flour, "lot_date": "2026-10-06" })
    );
    assert_eq!(node.state(), before);
    let retry = node.receive(4, &supplier, full, NOW, 0).await;
    assert_eq!(retry.body, first.body);
    assert_eq!(node.state(), before);

    let other_item = node
        .receive(6, &supplier, vec![line(&butter, 1, exp)], NOW, 0)
        .await;
    assert_eq!(lot_ids(&other_item), ["RAW-BUTTER-20261006-001"]);
    let other_day = node
        .receive(7, &supplier, vec![line(&flour, 1, exp)], NOW, 24 * HOUR)
        .await;
    assert_eq!(lot_ids(&other_day), ["RAW-FLOUR-20261005-001"]);
}

// 验收用例「流水号用尽」：一次收货同一物料 1000 行时，第 1000 行（line 999）用尽，整条命令不入账；
// 被业务校验拒绝的命令不落库，改成 999 行后可以用同一个 command_id 重提。
#[tokio::test]
async fn serial_exhaustion_within_one_receipt_rejects_the_whole_command() {
    let node = node();
    let flour = node.item(1, "FLOUR", "RAW").await;
    let supplier = node.supplier(2).await;
    let exp = "2026-10-20";
    let before = node.state();

    let reply = node
        .receive(
            3,
            &supplier,
            (0..1000).map(|_| line(&flour, 1, exp)).collect(),
            NOW,
            0,
        )
        .await;
    assert_eq!(
        assert_error(&reply, 409, "LOT_SERIAL_EXHAUSTED"),
        &json!({ "line": 999, "item_id": flour, "lot_date": "2026-10-06" })
    );
    assert_eq!(node.state(), before);

    let reply = node
        .receive(
            3,
            &supplier,
            (0..999).map(|_| line(&flour, 1, exp)).collect(),
            NOW,
            0,
        )
        .await;
    assert_eq!(lot_ids(&reply)[998], "RAW-FLOUR-20261006-999");
}

// 验收用例「修改物料分类」：面粉（RAW）已有批次 RAW-FLOUR-20261006-001；把分类改为 SEMI 后：
// - 原批次号不变：投影、库存明细、批次查询都还是它；用原 command_id、原内容重发原收货（改分类后、当天再建批次后各一次），
//   原样返回 RAW 批次号。
// - 当天再收货，新批次为 SEMI-FLOUR-20261006-002（流水号按物料与日期连续，不按类型重新编号）；
//   再改为 FINISHED 后收货为 FINISHED-FLOUR-20261006-003。
#[tokio::test]
async fn category_change_keeps_existing_lot_numbers() {
    let node = node();
    let flour = node.item(1, "FLOUR", "RAW").await;
    let supplier = node.supplier(2).await;
    let exp = "2026-10-20";
    let original = node
        .receive(3, &supplier, vec![line(&flour, 1, exp)], NOW, 0)
        .await;
    assert_eq!(lot_ids(&original), ["RAW-FLOUR-20261006-001"]);

    node.set_category(4, &flour, 1, "SEMI").await;
    // 一小时后重发：captured_at 不变，sent_at 晚一小时。
    let retry = node
        .receive(3, &supplier, vec![line(&flour, 1, exp)], NOW + HOUR, HOUR)
        .await;
    assert_eq!(retry.body, original.body);
    let semi = node
        .receive(5, &supplier, vec![line(&flour, 1, exp)], NOW + HOUR, 0)
        .await;
    assert_eq!(lot_ids(&semi), ["SEMI-FLOUR-20261006-002"]);
    node.set_category(6, &flour, 2, "FINISHED").await;
    let finished = node
        .receive(7, &supplier, vec![line(&flour, 1, exp)], NOW + HOUR, 0)
        .await;
    assert_eq!(lot_ids(&finished), ["FINISHED-FLOUR-20261006-003"]);
    // 同一天又建了两个批次之后再重发原收货：仍返回首次的 RAW 批次号，不重新取号。
    let late_retry = node
        .receive(
            3,
            &supplier,
            vec![line(&flour, 1, exp)],
            NOW + 2 * HOUR,
            2 * HOUR,
        )
        .await;
    assert_eq!(late_retry.body, original.body);

    assert_eq!(
        node.fifo(&flour).await,
        [
            "RAW-FLOUR-20261006-001",
            "SEMI-FLOUR-20261006-002",
            "FINISHED-FLOUR-20261006-003",
        ]
    );
    let reply = spec_support::get(
        &node.router,
        "/api/v1/lots/RAW-FLOUR-20261006-001",
        Some(&STAFF),
    )
    .await
    .unwrap();
    assert_eq!(assert_success(&reply)["lot"]["item_id"], json!(flour));
    let reply = spec_support::get(
        &node.router,
        "/api/v1/lots/SEMI-FLOUR-20261006-001",
        Some(&STAFF),
    )
    .await
    .unwrap();
    assert_eq!(
        assert_error(&reply, 404, "REFERENCE_NOT_FOUND"),
        &json!({ "entity": "LOT", "id": "SEMI-FLOUR-20261006-001" })
    );
}

/// 断言警告列表恰好是给定的 `EXPIRES_BEFORE_OLDER_STOCK` 行号（details 中的 lot_id 是该行的新批次）。
fn assert_expiry_warnings(reply: &JsonReply, item_id: &str, lines: &[usize]) {
    let lots = lot_ids(reply);
    let actual: Vec<(Value, Value)> = reply.body["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("warnings must be an array: {}", reply.body))
        .iter()
        .map(|w| (w["code"].clone(), w["details"].clone()))
        .collect();
    let expected: Vec<(Value, Value)> = lines
        .iter()
        .map(|&line| {
            (
                json!("EXPIRES_BEFORE_OLDER_STOCK"),
                json!({ "line": line, "item_id": item_id, "lot_id": lots[line] }),
            )
        })
        .collect();
    assert_eq!(actual, expected, "{}", reply.body);
}

// domain「批次」到期提醒：只比较批次号排在新批次之前（FIFO 先被扣）的批次，不按入账先后。现在是 10-06 09:00：
// - A：当天收货，到期 10-20，RAW-FLOUR-20261006-001；
// - B：补 10-04 的收货（lag 48 小时），到期 10-15，…-20261004-001：A 排在它之后，不警告（虽然 A 先入账、到期更晚）；
// - C：当天收货，到期 10-18，…-20261006-002：早于排在前面的 A，警告；
// - D：补 10-05 的收货（lag 24 小时），到期 10-14，…-20261005-001：早于排在前面的 B，警告；
// - E：补 10-05 的收货，到期 10-16，…-20261005-002：排在前面的 B（10-15）、D（10-14）都更早到期，不警告；
//   A、C 到期更晚但排在后面，不算。
// - F：当天收货但 occurred_at 为 07:00（lag 2 小时，早于 A、C 的 09:00），到期 10-17，…-20261006-003：同一天按流水号排在
//   A、C 之后，早于 A（10-20）、C（10-18），警告；不按实物发生钟点比较。
// - 把面粉改为 FINISHED 后当天收货 G，到期 10-19，FINISHED-FLOUR-20261006-004：按批次日期、流水号排在全部 RAW 批次之后，
//   早于 A（10-20），警告；不按完整批次号的字符串比较（"FINISHED" < "RAW"），也不只比较同一类型的批次。
#[tokio::test]
async fn expiry_warning_compares_lots_earlier_in_fifo_order() {
    let node = node();
    let flour = node.item(1, "FLOUR", "RAW").await;
    let supplier = node.supplier(2).await;

    let cases = [
        (3, "2026-10-20", 0, "RAW-FLOUR-20261006-001", false),
        (4, "2026-10-15", 48 * HOUR, "RAW-FLOUR-20261004-001", false),
        (5, "2026-10-18", 0, "RAW-FLOUR-20261006-002", true),
        (6, "2026-10-14", 24 * HOUR, "RAW-FLOUR-20261005-001", true),
        (7, "2026-10-16", 24 * HOUR, "RAW-FLOUR-20261005-002", false),
    ];
    for (n, expires_on, lag, lot, warns) in cases {
        let reply = node
            .receive(n, &supplier, vec![line(&flour, 1, expires_on)], NOW, lag)
            .await;
        assert_eq!(lot_ids(&reply), [lot]);
        assert_expiry_warnings(&reply, &flour, if warns { &[0] } else { &[] });
    }

    let f = node
        .receive(
            8,
            &supplier,
            vec![line(&flour, 1, "2026-10-17")],
            NOW,
            2 * HOUR,
        )
        .await;
    assert_eq!(lot_ids(&f), ["RAW-FLOUR-20261006-003"]);
    assert_expiry_warnings(&f, &flour, &[0]);
    node.set_category(9, &flour, 1, "FINISHED").await;
    let g = node
        .receive(10, &supplier, vec![line(&flour, 1, "2026-10-19")], NOW, 0)
        .await;
    assert_eq!(lot_ids(&g), ["FINISHED-FLOUR-20261006-004"]);
    assert_expiry_warnings(&g, &flour, &[0]);
    assert_eq!(
        node.fifo(&flour).await,
        [
            "RAW-FLOUR-20261004-001",
            "RAW-FLOUR-20261005-001",
            "RAW-FLOUR-20261005-002",
            "RAW-FLOUR-20261006-001",
            "RAW-FLOUR-20261006-002",
            "RAW-FLOUR-20261006-003",
            "FINISHED-FLOUR-20261006-004",
        ]
    );
}
