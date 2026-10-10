//! 锁定测试：报损接口（正式提交与预检）。规则见 docs/domain.md「报损接口」「批次」「单位」「时间」「事件目录」「投影表」
//! 「验收用例」，AGENTS.md「幂等」「HTTP 约定」「ID 与时间」，接口见 docs/interfaces.md。
//! 盘点吸收随盘点切片实现：本文件中的报损行都不被吸收；被吸收分支的重建见 spec_replay.rs。

mod spec_support;

use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use axum::Router;
use axum::http::Method;
use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use boh_storage::rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use spec_support::{
    Actor, JsonReply, MANAGER, STAFF, assert_error, assert_success, is_uuid_v7,
    non_canonical_uuids, raw_request, request, send,
};
use tempfile::TempDir;

const NOW: i64 = 1_791_248_400_000; // 2026-10-06 09:00 +08:00
const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;
/// 相对校准的上限：72 小时。
const MAX_LAG: i64 = 259_200_000;
/// 平板发送时刻的平板本地时钟：比节点快 7 分钟，相对校准应抵消这个偏差。
const SENT: i64 = NOW + 7 * MINUTE;
const UNKNOWN_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8399";
const URI: &str = "/api/v1/waste-records";
const PRECHECK: &str = "/api/v1/waste-records/precheck";
/// `flour_ab` 中同一次收货的两个面粉批次：A 5 g、B 10 g。
const A: &str = "RAW-FLOUR-20261006-001";
const B: &str = "RAW-FLOUR-20261006-002";
/// `Node::sugar` 收货建的糖批次。
const S: &str = "RAW-SUGAR-20261006-001";

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

type Rows<T> = Result<T, Box<dyn Error>>;

/// `inventory_movements` 的一行：(event_seq, movement_no, item_id, lot_id, kind, alloc_source, nominal_qty,
/// qty_delta, absorbed_by_event_id, physical_at, business_date)。
type MovementRow = (
    i64,
    i64,
    String,
    Option<String>,
    String,
    String,
    i64,
    i64,
    Option<String>,
    i64,
    String,
);

#[derive(Debug, PartialEq)]
struct Event {
    seq: i64,
    event_type: String,
    schema_version: i64,
    aggregate_type: String,
    aggregate_id: String,
    aggregate_version: i64,
    command_id: String,
    actor_id: String,
    device_id: String,
    business_date: String,
    occurred_at: i64,
    recorded_at: i64,
    payload: Value,
}

/// 写入后可能变化的全部内容：账本、幂等记录和库存投影。
type State = Vec<Vec<Vec<SqlValue>>>;

impl Node {
    #[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试；状态码和错误码由调用方断言。
    async fn post(&self, actor: &Actor, uri: &str, body: &Value) -> JsonReply {
        spec_support::post(&self.router, uri, Some(actor), body)
            .await
            .unwrap()
    }

    /// 普通员工提交报损。
    async fn waste(&self, body: &Value) -> JsonReply {
        self.post(&STAFF, URI, body).await
    }

    /// 报损成功，返回 `data.waste_record`。
    async fn waste_ok(&self, body: &Value) -> Value {
        let reply = self.waste(body).await;
        record(&reply).clone()
    }

    /// 普通员工预检 `lines`。
    async fn precheck(&self, lines: Vec<Value>) -> JsonReply {
        self.post(&STAFF, PRECHECK, &json!({ "lines": lines }))
            .await
    }

    /// 预检成功时的 `data`；断言 warnings 为空。
    async fn precheck_ok(&self, lines: Vec<Value>) -> Value {
        let reply = self.precheck(lines).await;
        let data = assert_success(&reply).clone();
        assert_eq!(reply.body["warnings"], json!([]), "{}", reply.body);
        data
    }

    /// 同一组 `lines` 分别正式提交（`cmd(99)`，实时录入）和预检，用于两者相同的校验错误。
    async fn both(&self, lines: Vec<Value>) -> [JsonReply; 2] {
        [
            self.waste(&body(&cmd(99), lines.clone(), 0)).await,
            self.precheck(lines).await,
        ]
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态的请求失败时，后续步骤没有意义，直接终止测试。
    async fn master(&self, uri: &str, key: &str, id_key: &str, body: Value) -> String {
        let reply = self.post(&MANAGER, uri, &body).await;
        assert_success(&reply)[key][id_key]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[allow(clippy::unwrap_used)] // 同上。
    async fn put(&self, uri: &str, body: Value) {
        let reply = spec_support::put(&self.router, uri, Some(&MANAGER), &body)
            .await
            .unwrap();
        assert_success(&reply);
    }

    /// 新建一个以克为基本单位的 RAW 物料；`units` 为 `[(unit_code, base_qty_per_unit)]`，须按 `unit_code` 升序。
    async fn item(&self, n: u16, code: &str, units: &[(&str, i64)]) -> String {
        self.master(
            "/api/v1/items",
            "item",
            "item_id",
            json!({
                "command_id": cmd(n), "code": code, "name": code, "base_unit": "g",
                "category": "RAW", "units": units_json(units), "active": true,
            }),
        )
        .await
    }

    /// 修改物料的单位（同时可停用）。
    async fn update_item(
        &self,
        n: u16,
        item_id: &str,
        base_revision: i64,
        units: &[(&str, i64)],
        active: bool,
    ) {
        self.put(
            &format!("/api/v1/items/{item_id}"),
            json!({
                "command_id": cmd(n), "base_revision": base_revision, "name": "FLOUR",
                "category": "RAW", "units": units_json(units), "active": active,
            }),
        )
        .await;
    }

    async fn supplier(&self, n: u16) -> String {
        self.master(
            "/api/v1/suppliers",
            "supplier",
            "supplier_id",
            json!({ "command_id": cmd(n), "code": "S1", "name": "S1", "active": true }),
        )
        .await
    }

    /// 新建一个启用的报损原因，返回其 ID。
    async fn reason(&self, n: u16, code: &str) -> String {
        self.master(
            "/api/v1/waste-reasons",
            "waste_reason",
            "waste_reason_id",
            json!({ "command_id": cmd(n), "code": code, "name": code, "active": true }),
        )
        .await
    }

    /// 收货：平板在 `SENT − lag` 录入、在 `SENT` 发送；返回各行的批次号。
    #[allow(clippy::unwrap_used)] // 同上。
    async fn receive(&self, n: u16, supplier: &str, lines: Vec<Value>, lag: i64) -> Vec<String> {
        let reply = self
            .post(
                &STAFF,
                "/api/v1/receipts",
                &receipt_body(n, supplier, lines, lag),
            )
            .await;
        assert_success(&reply)["receipt"]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line["lot_id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// 已有的供应商（`flour_ab` 建的 S1）的 ID。
    #[allow(clippy::unwrap_used)] // 测试夹具：查询失败已违反接口约定，直接终止测试。
    async fn supplier_id(&self) -> String {
        let reply = spec_support::get(&self.router, "/api/v1/suppliers", Some(&STAFF))
            .await
            .unwrap();
        assert_success(&reply)["suppliers"][0]["supplier_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// 在 `flour_ab` 之后新建物料 SUGAR 并收货 `qty` g（cmd n、n + 1），返回 (物料 ID, 批次号 `S`)。
    async fn sugar(&self, n: u16, qty: i64) -> (String, String) {
        let sugar = self.item(n, "SUGAR", &[]).await;
        let supplier = self.supplier_id().await;
        let lots = self
            .receive(n + 1, &supplier, vec![rline(&sugar, qty, "2026-10-20")], 0)
            .await;
        assert_eq!(lots, [S]);
        (sugar, S.to_owned())
    }

    /// 只含 `WASTE_LOGGED` 的事件，按 `seq` 升序。
    fn wastes(&self) -> Rows<Vec<Event>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT seq, event_type, schema_version, aggregate_type, aggregate_id, aggregate_version,
                    command_id, actor_id, device_id, business_date, occurred_at, recorded_at, payload
             FROM store_events WHERE event_type = 'WASTE_LOGGED' ORDER BY seq",
        )?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    Event {
                        seq: r.get(0)?,
                        event_type: r.get(1)?,
                        schema_version: r.get(2)?,
                        aggregate_type: r.get(3)?,
                        aggregate_id: r.get(4)?,
                        aggregate_version: r.get(5)?,
                        command_id: r.get(6)?,
                        actor_id: r.get(7)?,
                        device_id: r.get(8)?,
                        business_date: r.get(9)?,
                        occurred_at: r.get(10)?,
                        recorded_at: r.get(11)?,
                        payload: Value::Null,
                    },
                    r.get::<_, String>(12)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(event, payload)| {
                Ok(Event {
                    payload: serde_json::from_str(&payload)?,
                    ..event
                })
            })
            .collect()
    }

    /// 某个事件展开的库存流水，按 `movement_no` 升序。
    fn movements(&self, event_seq: i64) -> Rows<Vec<MovementRow>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT event_seq, movement_no, item_id, lot_id, kind, alloc_source, nominal_qty,
                    qty_delta, absorbed_by_event_id, physical_at, business_date
             FROM inventory_movements WHERE event_seq = ?1 ORDER BY movement_no",
        )?;
        let rows = statement
            .query_map([event_seq], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// 全部批次的 (lot_id, remaining_qty)，按批次号排列。
    #[allow(clippy::unwrap_used)] // 测试夹具：只读查询失败时无法比对状态，直接终止测试。
    fn lots(&self) -> Vec<(String, i64)> {
        let reader = spec_support::reader(&self.db_path).unwrap();
        let mut statement = reader
            .prepare("SELECT lot_id, remaining_qty FROM inventory_lots ORDER BY lot_id")
            .unwrap();
        statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// `inventory_unallocated` 的全部行 (item_id, qty)。
    #[allow(clippy::unwrap_used)] // 同上。
    fn unallocated(&self) -> Vec<(String, i64)> {
        let reader = spec_support::reader(&self.db_path).unwrap();
        let mut statement = reader
            .prepare("SELECT item_id, qty FROM inventory_unallocated ORDER BY item_id")
            .unwrap();
        statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// 视图 `inventory_on_hand` 中该物料的账面数。
    #[allow(clippy::unwrap_used)] // 同上。
    fn on_hand(&self, item_id: &str) -> i64 {
        let reader = spec_support::reader(&self.db_path).unwrap();
        reader
            .query_row(
                "SELECT qty FROM inventory_on_hand WHERE item_id = ?1",
                [item_id],
                |r| r.get(0),
            )
            .unwrap()
    }

    /// `processed_commands` 中该命令的 (`command_type`, 规范化请求, `recorded_at`)；规范化请求去掉值为 false 的确认标记。
    fn processed(&self, command_id: &str) -> Rows<(String, Value, i64)> {
        let reader = spec_support::reader(&self.db_path)?;
        let (command_type, request, recorded_at): (String, String, i64) = reader.query_row(
            "SELECT command_type, request, recorded_at FROM processed_commands WHERE command_id = ?1",
            [command_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok((
            command_type,
            without_false_confirmations(serde_json::from_str(&request)?),
            recorded_at,
        ))
    }

    #[allow(clippy::unwrap_used)] // 同上。
    fn state(&self) -> State {
        let reader = spec_support::reader(&self.db_path).unwrap();
        [
            "SELECT * FROM store_events ORDER BY seq",
            "SELECT * FROM processed_commands ORDER BY command_id",
            "SELECT * FROM inventory_lots ORDER BY lot_id",
            "SELECT * FROM inventory_movements ORDER BY event_seq, movement_no",
            "SELECT * FROM inventory_unallocated ORDER BY item_id",
        ]
        .iter()
        .map(|sql| spec_support::rows(&reader, sql).unwrap())
        .collect()
    }
}

/// 面粉（基本单位 g，另有 bag = 1000 g）、供应商、报损原因 EXPIRED，以及同一次收货的批次 A 5 g、B 10 g（cmd 1～4）。
/// 之后的第一个事件 `seq` 为 5。
async fn flour_ab(node: &Node) -> String {
    let flour = node.item(1, "FLOUR", &[("bag", 1000)]).await;
    let supplier = node.supplier(2).await;
    node.reason(3, "EXPIRED").await;
    let lots = node
        .receive(
            4,
            &supplier,
            vec![
                rline(&flour, 5, "2026-10-20"),
                rline(&flour, 10, "2026-10-20"),
            ],
            0,
        )
        .await;
    assert_eq!(lots, [A, B]);
    flour
}

fn units_json(units: &[(&str, i64)]) -> Value {
    Value::Array(
        units
            .iter()
            .map(|(code, qty)| json!({ "unit_code": code, "base_qty_per_unit": qty }))
            .collect(),
    )
}

/// 第 `n` 个命令 ID（UUIDv7）。
fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209e1{n:04x}")
}

/// 收货请求的一行：`qty` 克，到期日 `expires_on`。
fn rline(item_id: &str, qty: i64, expires_on: &str) -> Value {
    json!({
        "item_id": item_id,
        "input": { "qty": qty, "unit_code": "g", "base_qty_per_unit": 1 },
        "produced_on": "2026-10-01", "expires_on": expires_on, "line_cost_cents": 100,
    })
}

fn receipt_body(n: u16, supplier: &str, lines: Vec<Value>, lag: i64) -> Value {
    json!({
        "command_id": cmd(n), "supplier_id": supplier, "lines": lines,
        "captured_at": SENT - lag, "sent_at": SENT,
    })
}

/// 报损请求的一行：`qty` 克，原因 EXPIRED，不指定批次，不带确认。
fn wline(item_id: &str, qty: i64) -> Value {
    json!({
        "item_id": item_id,
        "input": { "qty": qty, "unit_code": "g", "base_qty_per_unit": 1 },
        "reason_code": "EXPIRED",
    })
}

/// 指定批次。
fn at(mut line: Value, lot_id: &str) -> Value {
    line["lot_id"] = json!(lot_id);
    line
}

/// 带确认标记。
fn confirmed(mut line: Value) -> Value {
    line["confirm_shortage"] = json!(true);
    line
}

/// 实时录入的请求体：平板在 `SENT − lag` 录入、在 `SENT` 发送。
fn body(command_id: &str, lines: Vec<Value>, lag: i64) -> Value {
    timed_body(command_id, lines, SENT - lag, SENT)
}

fn timed_body(command_id: &str, lines: Vec<Value>, captured_at: i64, sent_at: i64) -> Value {
    json!({
        "command_id": command_id,
        "lines": lines,
        "captured_at": captured_at,
        "sent_at": sent_at,
    })
}

fn fifo(lot_id: &str, qty: i64) -> Value {
    json!({ "lot_id": lot_id, "qty": qty, "source": "FIFO" })
}

fn specified(lot_id: &str, qty: i64) -> Value {
    json!({ "lot_id": lot_id, "qty": qty, "source": "SPECIFIED" })
}

fn shortfall(qty: i64) -> Value {
    json!({ "qty": qty, "source": "SHORTFALL" })
}

/// payload / 响应中的一行（`qty` 克，原因 EXPIRED）。`lot` 为 (指定的批次, 该批次的账面)；不指定批次时不带
/// `lot_id`、`lot_book_qty` 两个键。
fn logged(
    item_id: &str,
    lot: Option<(&str, i64)>,
    qty: i64,
    item_book_qty: i64,
    alloc: Vec<Value>,
) -> Value {
    let mut line = json!({
        "item_id": item_id, "qty": qty,
        "input": { "qty": qty, "unit_code": "g", "base_qty_per_unit": 1 },
        "reason_code": "EXPIRED", "item_book_qty": item_book_qty, "alloc": alloc,
    });
    if let Some((lot_id, lot_book_qty)) = lot {
        line["lot_id"] = json!(lot_id);
        line["lot_book_qty"] = json!(lot_book_qty);
    }
    line
}

/// `WASTE_CONFIRMATION_REQUIRED` 的 `details.lines` 中的一项。
fn short(
    line: usize,
    item_id: &str,
    lot: Option<(&str, i64)>,
    qty: i64,
    item_book_qty: i64,
) -> Value {
    let mut row =
        json!({ "line": line, "item_id": item_id, "qty": qty, "item_book_qty": item_book_qty });
    if let Some((lot_id, lot_book_qty)) = lot {
        row["lot_id"] = json!(lot_id);
        row["lot_book_qty"] = json!(lot_book_qty);
    }
    row
}

/// 预检 `data.lines` 中的一项。
fn checked(
    line: usize,
    item_id: &str,
    lot: Option<(&str, i64)>,
    qty: i64,
    item_book_qty: i64,
    needs_confirmation: bool,
    alloc: Vec<Value>,
) -> Value {
    let mut row = short(line, item_id, lot, qty, item_book_qty);
    row["needs_confirmation"] = json!(needs_confirmation);
    row["alloc"] = json!(alloc);
    row
}

/// 报损流水：`kind = 'WASTE'`，`nominal_qty = qty_delta = −qty`，营业日 2026-10-06。
fn movement(
    seq: i64,
    no: i64,
    item_id: &str,
    lot_id: Option<&str>,
    source: &str,
    qty: i64,
    physical_at: i64,
) -> MovementRow {
    (
        seq,
        no,
        item_id.to_owned(),
        lot_id.map(str::to_owned),
        "WASTE".into(),
        source.into(),
        -qty,
        -qty,
        None,
        physical_at,
        "2026-10-06".into(),
    )
}

/// 期望的规范化请求：剥离 `command_id`、`sent_at`，并去掉值为 false 的确认标记（与省略等价）。
fn canonical(request: &Value) -> Value {
    let mut canonical = request.clone();
    if let Some(object) = canonical.as_object_mut() {
        object.remove("command_id");
        object.remove("sent_at");
    }
    without_false_confirmations(canonical)
}

/// 去掉各行中值为 false 的 `confirm_shortage`：省略与 false 的规范化请求相同，存储时用哪种写法不作规定。
fn without_false_confirmations(mut request: Value) -> Value {
    if let Some(lines) = request["lines"].as_array_mut() {
        for line in lines {
            if line["confirm_shortage"] == json!(false)
                && let Some(object) = line.as_object_mut()
            {
                object.remove("confirm_shortage");
            }
        }
    }
    request
}

#[allow(clippy::unwrap_used)] // 测试夹具：成功响应缺少这些字段已违反接口约定，直接终止测试。
/// 取出成功响应中的报损记录，并校验服务端生成的报损 ID：规范 UUIDv7，时间部分等于该命令的 `recorded_at`
/// （AGENTS.md「ID 与时间」：不用 `Uuid::now_v7()`）。
fn record(reply: &JsonReply) -> &Value {
    let record = &assert_success(reply)["waste_record"];
    let recorded_at = record["recorded_at"].as_i64().unwrap();
    let id = record["waste_record_id"].as_str().unwrap();
    assert!(is_uuid_v7(id), "{record}");
    assert_eq!(
        i64::from_str_radix(&format!("{}{}", &id[0..8], &id[9..13]), 16).unwrap(),
        recorded_at,
        "{id}"
    );
    record
}

/// 断言警告列表恰好是给定的 (警告码, details)；`message` 只要求是字符串。
fn assert_warnings(reply: &JsonReply, expected: &[(&str, Value)]) {
    let warnings = reply.body["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("warnings must be an array: {}", reply.body));
    let actual: Vec<(Value, Value)> = warnings
        .iter()
        .map(|w| {
            assert!(w["message"].is_string(), "{w}");
            let mut keys: Vec<&str> = w
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            keys.sort_unstable();
            assert_eq!(keys, ["code", "details", "message"], "{w}");
            (w["code"].clone(), w["details"].clone())
        })
        .collect();
    let expected: Vec<(Value, Value)> = expected
        .iter()
        .map(|(code, details)| (json!(code), details.clone()))
        .collect();
    assert_eq!(actual, expected, "{}", reply.body);
}

fn pair(lot_id: &str, qty: i64) -> (String, i64) {
    (lot_id.to_owned(), qty)
}

// ---------------------------------------------------------------------------------------------
// 写入成功
// ---------------------------------------------------------------------------------------------

// 报损接口「新建」：普通员工即可报损；一个命令写一条 WASTE_LOGGED@1（新的 WASTE_RECORD 聚合，version 1）和一行
// processed_commands（command_type waste.log）；账面足够、不带确认时直接入账，warnings 为空。
// - lines[0] 不指定批次，6 bag（每 bag 1000 g）= 6000 g；item_book_qty 是该行之前的物料净账面 15000，
//   按批次号 FIFO 分配 A 5000 + B 1000。不指定批次的行没有 lot_id、lot_book_qty 两个键。
// - lines[1] 指定 B 2000 g、原因 DAMAGED：看到的是 lines[0] 之后的账面（B 9000，净账面 9000），整行一项 SPECIFIED。
// - 流水按行序、alloc 顺序编号；physical_at 是 occurred_at（相对校准抵消平板时钟偏差）；reason_code 和账面值只在事件中。
// - 响应的 lines 与 payload 的 lines 相同；规范化请求剥离 command_id、sent_at，保留 captured_at。
#[tokio::test]
async fn waste_writes_event_movements_and_command() {
    let node = node();
    let flour = node.item(1, "FLOUR", &[("bag", 1000)]).await;
    let supplier = node.supplier(2).await;
    node.reason(3, "EXPIRED").await;
    node.reason(4, "DAMAGED").await;
    let lots = node
        .receive(
            5,
            &supplier,
            vec![
                rline(&flour, 5000, "2026-10-20"),
                rline(&flour, 10000, "2026-10-20"),
            ],
            0,
        )
        .await;
    assert_eq!(lots, [A, B]);
    let request = body(
        &cmd(6),
        vec![
            json!({
                "item_id": flour, "input": { "qty": 6, "unit_code": "bag", "base_qty_per_unit": 1000 },
                "reason_code": "EXPIRED",
            }),
            json!({
                "item_id": flour, "lot_id": B,
                "input": { "qty": 2000, "unit_code": "g", "base_qty_per_unit": 1 },
                "reason_code": "DAMAGED",
            }),
        ],
        10 * MINUTE,
    );

    let reply = node.waste(&request).await;

    let record = record(&reply).clone();
    let record_id = record["waste_record_id"].as_str().unwrap().to_owned();
    let occurred_at = NOW - 10 * MINUTE;
    let lines = json!([
        {
            "item_id": flour, "qty": 6000,
            "input": { "qty": 6, "unit_code": "bag", "base_qty_per_unit": 1000 },
            "reason_code": "EXPIRED", "item_book_qty": 15000,
            "alloc": [fifo(A, 5000), fifo(B, 1000)],
        },
        {
            "item_id": flour, "lot_id": B, "qty": 2000,
            "input": { "qty": 2000, "unit_code": "g", "base_qty_per_unit": 1 },
            "reason_code": "DAMAGED", "item_book_qty": 9000, "lot_book_qty": 9000,
            "alloc": [specified(B, 2000)],
        },
    ]);
    assert_eq!(
        reply.body,
        json!({
            "success": true,
            "data": { "waste_record": {
                "waste_record_id": record_id,
                "lines": lines,
                "business_date": "2026-10-06",
                "occurred_at": occurred_at,
                "recorded_at": NOW,
                "actor_id": STAFF.employee_id,
                "device_id": STAFF.device_id,
            } },
            "warnings": [],
            "error": null,
        })
    );
    let seq = 6;
    assert_eq!(
        node.wastes().unwrap(),
        [Event {
            seq,
            event_type: "WASTE_LOGGED".into(),
            schema_version: 1,
            aggregate_type: "WASTE_RECORD".into(),
            aggregate_id: record_id,
            aggregate_version: 1,
            command_id: cmd(6),
            actor_id: STAFF.employee_id.into(),
            device_id: STAFF.device_id.into(),
            business_date: "2026-10-06".into(),
            occurred_at,
            recorded_at: NOW,
            payload: json!({ "lines": lines }),
        }]
    );
    assert_eq!(
        node.movements(seq).unwrap(),
        [
            movement(seq, 0, &flour, Some(A), "FIFO", 5000, occurred_at),
            movement(seq, 1, &flour, Some(B), "FIFO", 1000, occurred_at),
            movement(seq, 2, &flour, Some(B), "SPECIFIED", 2000, occurred_at),
        ]
    );
    assert_eq!(node.lots(), [pair(A, 0), pair(B, 7000)]);
    assert_eq!(node.unallocated(), Vec::<(String, i64)>::new());
    assert_eq!(node.on_hand(&flour), 7000);
    assert_eq!(
        node.processed(&cmd(6)).unwrap(),
        ("waste.log".into(), canonical(&request), NOW)
    );
}

// 报损接口「权限」与「业务校验」：店长同样可以报损，身份写进事件和响应；每次报损是一个新聚合。
// 停用的物料、停用的报损原因照常受理（domain「主数据」：停用只在界面上隐藏）。
#[tokio::test]
async fn managers_and_inactive_references_are_accepted() {
    let node = node();
    let flour = flour_ab(&node).await;
    let reasons = spec_support::get(&node.router, "/api/v1/waste-reasons", Some(&STAFF))
        .await
        .unwrap();
    let reason_id = assert_success(&reasons)["waste_reasons"][0]["waste_reason_id"]
        .as_str()
        .unwrap()
        .to_owned();
    node.update_item(10, &flour, 1, &[("bag", 1000)], false)
        .await;
    node.put(
        &format!("/api/v1/waste-reasons/{reason_id}"),
        json!({ "command_id": cmd(11), "base_revision": 1, "name": "过期", "active": false }),
    )
    .await;

    let reply = node
        .post(&MANAGER, URI, &body(&cmd(12), vec![wline(&flour, 1)], 0))
        .await;
    let first = record(&reply).clone();
    let second = node
        .waste_ok(&body(&cmd(13), vec![wline(&flour, 1)], 0))
        .await;

    assert_eq!(first["actor_id"], json!(MANAGER.employee_id));
    assert_eq!(first["device_id"], json!(MANAGER.device_id));
    assert_eq!(second["actor_id"], json!(STAFF.employee_id));
    assert_ne!(first["waste_record_id"], second["waste_record_id"]);
    let events = node.wastes().unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.aggregate_version == 1));
    assert_eq!(node.lots(), [pair(A, 3), pair(B, 10)]);
}

// domain「时间」相对校准与营业日：
// - lag < 0 按 0 处理，返回警告 CAPTURE_TIME_ADJUSTED（details 为 {}），即使该行经确认产生了账外缺口也只有这一条警告：
//   报损不返回 STOCK_SHORTFALL。
// - 恰好 72 小时照常受理：occurred_at 为 10-03 09:00，营业日 2026-10-03；流水的 physical_at、business_date 随 occurred_at。
// - lag 6 小时：occurred_at 为 10-06 03:00，早于日切，营业日 2026-10-05。
// - 超过 72 小时 400 CAPTURE_TOO_OLD（details 为 {}），什么都不写。
#[tokio::test]
async fn relative_calibration_and_business_date_apply() {
    let node = node();
    let flour = flour_ab(&node).await;

    let reply = node
        .waste(&timed_body(
            &cmd(10),
            vec![confirmed(wline(&flour, 20))],
            SENT + MINUTE,
            SENT,
        ))
        .await;
    assert_eq!(record(&reply)["occurred_at"], json!(NOW));
    assert_warnings(&reply, &[("CAPTURE_TIME_ADJUSTED", json!({}))]);
    assert_eq!(node.unallocated(), [(flour.clone(), -5)]);

    let reply = node
        .waste(&body(&cmd(11), vec![confirmed(wline(&flour, 1))], MAX_LAG))
        .await;
    let record_72h = record(&reply).clone();
    assert_eq!(record_72h["occurred_at"], json!(NOW - MAX_LAG));
    assert_eq!(record_72h["business_date"], json!("2026-10-03"));
    assert_eq!(reply.body["warnings"], json!([]));
    let (_, _, _, _, _, _, _, _, _, physical_at, business_date) =
        node.movements(6).unwrap().remove(0);
    assert_eq!(
        (physical_at, business_date),
        (NOW - MAX_LAG, "2026-10-03".to_owned())
    );

    let record_6h = node
        .waste_ok(&body(&cmd(12), vec![confirmed(wline(&flour, 1))], 6 * HOUR))
        .await;
    assert_eq!(record_6h["occurred_at"], json!(NOW - 6 * HOUR));
    assert_eq!(record_6h["business_date"], json!("2026-10-05"));

    let before = node.state();
    let reply = node
        .waste(&body(
            &cmd(13),
            vec![confirmed(wline(&flour, 1))],
            MAX_LAG + 1,
        ))
        .await;
    assert_eq!(assert_error(&reply, 400, "CAPTURE_TOO_OLD"), &json!({}));
    assert_eq!(node.state(), before);
}

// ---------------------------------------------------------------------------------------------
// 不足确认（验收用例）
// ---------------------------------------------------------------------------------------------

// 验收用例「指定批次不足」：批次 A 5、B 10；报损 8 g，指定 A。
// - 不带确认：409 WASTE_CONFIRMATION_REQUIRED，details.lines 列出该行（lot_book_qty 5、item_book_qty 15），不写任何内容。
// - 同一 command_id 加确认重提：入账，A 8 SPECIFIED，不转去扣 B；A −3、B 10；没有账外缺口；warnings 为空（不返回 STOCK_SHORTFALL）。
// - 确认标记 true 保存在规范化请求中；payload 不带确认标记。
#[tokio::test]
async fn specified_lot_shortage_requires_confirmation() {
    let node = node();
    let flour = flour_ab(&node).await;
    let before = node.state();

    let reply = node
        .waste(&body(&cmd(10), vec![at(wline(&flour, 8), A)], 0))
        .await;
    assert_eq!(
        assert_error(&reply, 409, "WASTE_CONFIRMATION_REQUIRED"),
        &json!({ "lines": [short(0, &flour, Some((A, 5)), 8, 15)] })
    );
    assert_eq!(node.state(), before);

    let request = body(&cmd(10), vec![confirmed(at(wline(&flour, 8), A))], 0);
    let reply = node.waste(&request).await;
    assert_eq!(
        record(&reply)["lines"],
        json!([logged(&flour, Some((A, 5)), 8, 15, vec![specified(A, 8)])])
    );
    assert_eq!(reply.body["warnings"], json!([]));
    assert_eq!(node.lots(), [pair(A, -3), pair(B, 10)]);
    assert_eq!(node.unallocated(), Vec::<(String, i64)>::new());
    assert_eq!(node.on_hand(&flour), 7);
    assert_eq!(
        node.movements(5).unwrap(),
        [movement(5, 0, &flour, Some(A), "SPECIFIED", 8, NOW)]
    );
    let (_, stored, _) = node.processed(&cmd(10)).unwrap();
    assert_eq!(stored, canonical(&request));
    assert_eq!(stored["lines"][0]["confirm_shortage"], json!(true));
}

// 验收用例「指定批次、净账面不足」：只有批次 A 5 时报损 15 g（不指定批次，确认）→ A 0、账外缺口 −10；再收货建批次 B 5
// （RAW-FLOUR-20261006-002）。指定 B 报 3 g：批次够（5 ≥ 3），但物料净账面 0 + 5 − 10 = −5 不足，需要确认；
// 确认后 B 3 SPECIFIED，B 2，账外缺口仍为 −10（指定批次的差额不进账外缺口），账面 −8。
#[tokio::test]
async fn specified_lot_also_checks_the_item_book() {
    let node = node();
    let flour = node.item(1, "FLOUR", &[]).await;
    let supplier = node.supplier(2).await;
    node.reason(3, "EXPIRED").await;
    node.receive(4, &supplier, vec![rline(&flour, 5, "2026-10-20")], 0)
        .await;
    node.waste_ok(&body(&cmd(5), vec![confirmed(wline(&flour, 15))], 0))
        .await;
    let lots = node
        .receive(6, &supplier, vec![rline(&flour, 5, "2026-10-20")], 0)
        .await;
    assert_eq!(lots, [B]);
    assert_eq!(node.on_hand(&flour), -5);
    let before = node.state();

    let reply = node
        .waste(&body(&cmd(7), vec![at(wline(&flour, 3), B)], 0))
        .await;
    assert_eq!(
        assert_error(&reply, 409, "WASTE_CONFIRMATION_REQUIRED"),
        &json!({ "lines": [short(0, &flour, Some((B, 5)), 3, -5)] })
    );
    assert_eq!(node.state(), before);

    let record = node
        .waste_ok(&body(&cmd(7), vec![confirmed(at(wline(&flour, 3), B))], 0))
        .await;
    assert_eq!(
        record["lines"],
        json!([logged(&flour, Some((B, 5)), 3, -5, vec![specified(B, 3)])])
    );
    assert_eq!(node.lots(), [pair(A, 0), pair(B, 2)]);
    assert_eq!(node.unallocated(), [(flour.clone(), -10)]);
    assert_eq!(node.on_hand(&flour), -8);
}

// 验收用例「有负批次时不指定批次」：A 5、B 10，先指定 A 报 8（确认）→ A −3、B 10，净账面 7。再报 8 g，不指定批次：
// 正余量批次 B 足够分配，但净账面 7 < 8，需要确认（item_book_qty 7，不带 lot_id、lot_book_qty）；
// 确认后 FIFO 跳过余量不大于 0 的 A，分配 B 8 FIFO；A −3、B 2，不新增账外缺口，账面 −1。
#[tokio::test]
async fn unspecified_line_checks_the_item_book_including_negative_lots() {
    let node = node();
    let flour = flour_ab(&node).await;
    node.waste_ok(&body(&cmd(10), vec![confirmed(at(wline(&flour, 8), A))], 0))
        .await;
    let before = node.state();

    let reply = node.waste(&body(&cmd(11), vec![wline(&flour, 8)], 0)).await;
    assert_eq!(
        assert_error(&reply, 409, "WASTE_CONFIRMATION_REQUIRED"),
        &json!({ "lines": [short(0, &flour, None, 8, 7)] })
    );
    assert_eq!(node.state(), before);

    let record = node
        .waste_ok(&body(&cmd(11), vec![confirmed(wline(&flour, 8))], 0))
        .await;
    assert_eq!(
        record["lines"],
        json!([logged(&flour, None, 8, 7, vec![fifo(B, 8)])])
    );
    assert_eq!(node.lots(), [pair(A, -3), pair(B, 2)]);
    assert_eq!(node.unallocated(), Vec::<(String, i64)>::new());
    assert_eq!(node.on_hand(&flour), -1);
}

// 验收用例「不足记入账外缺口」：只有批次 A 5；报损 8 g，不指定批次。
// - 不带确认：409，item_book_qty 5。
// - 确认后 A 5 FIFO + 3 SHORTFALL（不带 lot_id）；A 0，账外缺口 −3，账面 −3；warnings 为空，不返回 STOCK_SHORTFALL。
//   SHORTFALL 流水的 lot_id 为 NULL；inventory_unallocated 没有该物料的行时新建。
// - 验收用例「收货后耗尽」：再报 2 g。面粉曾经收货（有批次 A），不受「从未收货」的限制：不带确认时照常 409
//   WASTE_CONFIRMATION_REQUIRED（item_book_qty −3）；确认后 A 余量为 0，被 FIFO 跳过，整行 2 SHORTFALL；
//   账外缺口累加到 −5，仍是一行。
#[tokio::test]
async fn shortfall_beyond_positive_lots_goes_off_book() {
    let node = node();
    let flour = node.item(1, "FLOUR", &[]).await;
    let supplier = node.supplier(2).await;
    node.reason(3, "EXPIRED").await;
    node.receive(4, &supplier, vec![rline(&flour, 5, "2026-10-20")], 0)
        .await;

    let reply = node.waste(&body(&cmd(5), vec![wline(&flour, 8)], 0)).await;
    assert_eq!(
        assert_error(&reply, 409, "WASTE_CONFIRMATION_REQUIRED"),
        &json!({ "lines": [short(0, &flour, None, 8, 5)] })
    );

    let reply = node
        .waste(&body(&cmd(5), vec![confirmed(wline(&flour, 8))], 0))
        .await;
    assert_eq!(
        record(&reply)["lines"],
        json!([logged(&flour, None, 8, 5, vec![fifo(A, 5), shortfall(3)])])
    );
    assert_eq!(reply.body["warnings"], json!([]));
    assert_eq!(
        node.movements(5).unwrap(),
        [
            movement(5, 0, &flour, Some(A), "FIFO", 5, NOW),
            movement(5, 1, &flour, None, "SHORTFALL", 3, NOW),
        ]
    );
    assert_eq!(node.lots(), [pair(A, 0)]);
    assert_eq!(node.unallocated(), [(flour.clone(), -3)]);
    assert_eq!(node.on_hand(&flour), -3);

    let reply = node.waste(&body(&cmd(6), vec![wline(&flour, 2)], 0)).await;
    assert_eq!(
        assert_error(&reply, 409, "WASTE_CONFIRMATION_REQUIRED"),
        &json!({ "lines": [short(0, &flour, None, 2, -3)] })
    );
    let record = node
        .waste_ok(&body(&cmd(6), vec![confirmed(wline(&flour, 2))], 0))
        .await;
    assert_eq!(
        record["lines"],
        json!([logged(&flour, None, 2, -3, vec![shortfall(2)])])
    );
    assert_eq!(
        node.movements(6).unwrap(),
        [movement(6, 0, &flour, None, "SHORTFALL", 2, NOW)]
    );
    assert_eq!(node.lots(), [pair(A, 0)]);
    assert_eq!(node.unallocated(), [(flour.clone(), -5)]);
    assert_eq!(node.on_hand(&flour), -5);
}

// 验收用例「从未收货的物料」与 domain「报损接口」：物料还没有任何批次时 409 ITEM_HAS_NO_LOTS，details 为 {line, item_id}，
// 整条命令不入账，不生成 SHORTFALL。
// - 带确认标记也拒绝：确认只处理数量不足，不授予报损资格。
// - 同一命令中面粉的正常行也不入账；line 是糖所在的行。
// - 预检同样拒绝（不把该行标为可提交或仅需确认）。
// 依据是有没有批次，不是 inventory_on_hand 有没有行：糖在 inventory_on_hand 中也有一行，账面 0。
#[tokio::test]
async fn item_without_lots_cannot_be_wasted() {
    let node = node();
    let flour = flour_ab(&node).await;
    let sugar = node.item(10, "SUGAR", &[]).await;
    assert_eq!(node.on_hand(&sugar), 0);
    let before = node.state();

    for (lines, line) in [
        (vec![wline(&sugar, 1)], 0),
        (vec![wline(&flour, 1), wline(&sugar, 1)], 1),
    ] {
        for reply in node.both(lines.clone()).await {
            assert_eq!(
                assert_error(&reply, 409, "ITEM_HAS_NO_LOTS"),
                &json!({ "line": line, "item_id": sugar }),
                "{lines:?}"
            );
        }
        let confirmed_lines: Vec<Value> = lines.into_iter().map(confirmed).collect();
        let reply = node.waste(&body(&cmd(11), confirmed_lines, 0)).await;
        assert_eq!(
            assert_error(&reply, 409, "ITEM_HAS_NO_LOTS"),
            &json!({ "line": line, "item_id": sugar })
        );
    }

    assert_eq!(node.state(), before);
}

// 验收用例「数量越界」与 domain「报损接口」：逐行处理中的账面、扣减后的批次余量或账外缺口超出 i64 时 400 VALIDATION_FAILED，
// details 为 {}，整条命令不入账（前面的正常行也不入账），不绕回、不截断、不钳制；预检同样拒绝。其他前提都满足：物料已收货、
// 引用与单位合法、带确认。
// - 准备：糖批次 S 1 g，指定 S 报 i64::MAX（确认）→ S 为 i64::MIN + 2；面粉 A 5、B 10，不指定批次报 i64::MAX（确认）
//   → A 5 + B 10 FIFO，i64::MAX − 15 SHORTFALL，账外缺口 i64::MIN + 16。
// - 批次余量越界：指定 S 报 3 g（S 会成为 i64::MIN − 1）。账外缺口越界：面粉不指定批次报 17 g。
// - 部分写入：面粉 16 g（单独提交可以入账）在前、指定 S 报 3 g 在后，整条拒绝，面粉也不变。
// - 合法边界不误拒：同一 command_id 改为面粉 16 g、指定 S 报 2 g，照常入账：S 与面粉账外缺口都恰好是 i64::MIN。
//   与被拒绝的请求只差数量，说明拒绝原因是数量越界，不是请求结构。
#[tokio::test]
async fn quantities_beyond_i64_reject_the_whole_command() {
    let node = node();
    let flour = flour_ab(&node).await;
    let (sugar, s) = node.sugar(10, 1).await;
    node.waste_ok(&body(
        &cmd(12),
        vec![confirmed(at(wline(&sugar, i64::MAX), &s))],
        0,
    ))
    .await;
    let record = node
        .waste_ok(&body(&cmd(13), vec![confirmed(wline(&flour, i64::MAX))], 0))
        .await;
    assert_eq!(
        record["lines"][0]["alloc"],
        json!([fifo(A, 5), fifo(B, 10), shortfall(i64::MAX - 15)])
    );
    assert_eq!(
        node.lots(),
        [pair(A, 0), pair(B, 0), pair(&s, i64::MIN + 2)]
    );
    assert_eq!(node.unallocated(), [(flour.clone(), i64::MIN + 16)]);
    let before = node.state();

    for lines in [
        vec![at(wline(&sugar, 3), &s)],
        vec![wline(&flour, 17)],
        vec![wline(&flour, 16), at(wline(&sugar, 3), &s)],
    ] {
        let reply = node.precheck(lines.clone()).await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "precheck {lines:?}"
        );
        let confirmed_lines: Vec<Value> = lines.into_iter().map(confirmed).collect();
        let reply = node.waste(&body(&cmd(14), confirmed_lines, 0)).await;
        assert_eq!(assert_error(&reply, 400, "VALIDATION_FAILED"), &json!({}));
        assert_eq!(node.state(), before);
    }

    let record = node
        .waste_ok(&body(
            &cmd(14),
            vec![
                confirmed(wline(&flour, 16)),
                confirmed(at(wline(&sugar, 2), &s)),
            ],
            0,
        ))
        .await;
    assert_eq!(
        record["lines"],
        json!([
            logged(&flour, None, 16, i64::MIN + 16, vec![shortfall(16)]),
            logged(
                &sugar,
                Some((&s, i64::MIN + 2)),
                2,
                i64::MIN + 2,
                vec![specified(&s, 2)]
            ),
        ])
    );
    assert_eq!(node.lots(), [pair(A, 0), pair(B, 0), pair(&s, i64::MIN)]);
    assert_eq!(node.unallocated(), [(flour.clone(), i64::MIN)]);
}

// domain「报损接口」数量越界（物料净账面）：各批次余量和账外缺口本身都可表示，但物料净账面超出 i64，同样 400 VALIDATION_FAILED，
// 整条命令不入账，预检同样拒绝。面粉 A、B 各 1 g，指定 A 报 i64::MAX（确认）→ A 为 i64::MIN + 2、B 1，净账面 i64::MIN + 3；
// 糖 S 5 g。
// - [糖 1, 指定 B 4, 指定 B 1]：lines[2] 之前的净账面是 i64::MIN − 1（B 只是 −3）；
// - [糖 1, 指定 B 4]：最后一行之后的净账面是 i64::MIN − 1。
// 同一 command_id 改为 [糖 1, 指定 B 3] 照常入账：B −2，面粉净账面恰好 i64::MIN，糖 4。
#[tokio::test]
async fn item_book_beyond_i64_rejects_the_whole_command() {
    let node = node();
    let flour = node.item(1, "FLOUR", &[]).await;
    let supplier = node.supplier(2).await;
    node.reason(3, "EXPIRED").await;
    let lots = node
        .receive(
            4,
            &supplier,
            vec![
                rline(&flour, 1, "2026-10-20"),
                rline(&flour, 1, "2026-10-20"),
            ],
            0,
        )
        .await;
    assert_eq!(lots, [A, B]);
    let (sugar, s) = node.sugar(5, 5).await;
    node.waste_ok(&body(
        &cmd(7),
        vec![confirmed(at(wline(&flour, i64::MAX), A))],
        0,
    ))
    .await;
    assert_eq!(node.on_hand(&flour), i64::MIN + 3);
    let before = node.state();

    for lines in [
        vec![
            wline(&sugar, 1),
            at(wline(&flour, 4), B),
            at(wline(&flour, 1), B),
        ],
        vec![wline(&sugar, 1), at(wline(&flour, 4), B)],
    ] {
        let reply = node.precheck(lines.clone()).await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "precheck {lines:?}"
        );
        let confirmed_lines: Vec<Value> = lines.into_iter().map(confirmed).collect();
        let reply = node.waste(&body(&cmd(8), confirmed_lines, 0)).await;
        assert_eq!(assert_error(&reply, 400, "VALIDATION_FAILED"), &json!({}));
        assert_eq!(node.state(), before);
    }

    let record = node
        .waste_ok(&body(
            &cmd(8),
            vec![wline(&sugar, 1), confirmed(at(wline(&flour, 3), B))],
            0,
        ))
        .await;
    assert_eq!(
        record["lines"],
        json!([
            logged(&sugar, None, 1, 5, vec![fifo(&s, 1)]),
            logged(&flour, Some((B, 1)), 3, i64::MIN + 3, vec![specified(B, 3)]),
        ])
    );
    assert_eq!(
        node.lots(),
        [pair(A, i64::MIN + 2), pair(B, -2), pair(&s, 4)]
    );
    assert_eq!(node.on_hand(&flour), i64::MIN);
}

// 验收用例「同一命令的后续行」：A 5、B 10；一次报损两行：lines[0] 不指定批次 8 g，lines[1] 指定 A 1 g。
// 每行看到的是前面各行处理之后的账面：lines[0] 不需要确认（净账面 15），分配 A 5 + B 3；lines[1] 看到 A 0、净账面 7，
// 批次不足，需要确认。409 只列 line 1（lot_book_qty 0、item_book_qty 7）。只给 lines[1] 加确认后入账：A −1、B 7。
#[tokio::test]
async fn later_lines_see_the_book_after_earlier_lines() {
    let node = node();
    let flour = flour_ab(&node).await;

    let reply = node
        .waste(&body(
            &cmd(10),
            vec![wline(&flour, 8), at(wline(&flour, 1), A)],
            0,
        ))
        .await;
    assert_eq!(
        assert_error(&reply, 409, "WASTE_CONFIRMATION_REQUIRED"),
        &json!({ "lines": [short(1, &flour, Some((A, 0)), 1, 7)] })
    );

    let record = node
        .waste_ok(&body(
            &cmd(10),
            vec![wline(&flour, 8), confirmed(at(wline(&flour, 1), A))],
            0,
        ))
        .await;
    assert_eq!(
        record["lines"],
        json!([
            logged(&flour, None, 8, 15, vec![fifo(A, 5), fifo(B, 3)]),
            logged(&flour, Some((A, 0)), 1, 7, vec![specified(A, 1)]),
        ])
    );
    assert_eq!(node.lots(), [pair(A, -1), pair(B, 7)]);
    assert_eq!(node.on_hand(&flour), 6);
}

// 报损接口「不足确认」：需要确认但没带确认的行，照确认后的结果继续试算后续各行；409 一次按 line 升序列出全部这样的行。
// A 5、B 10；lines[0] 指定 A 8（批次不足），lines[1] 不指定批次 1（净账面 7，不需要确认，扣 B），lines[2] 指定 A 1
// （A 已试算为 −3，净账面 6）。
// - 都不带确认：列出 line 0、line 2，lines[2] 的账面按 lines[0]、lines[1] 已扣减计算。
// - 只确认其中一行：只列另一行，账面相同。
// - 都确认后入账：A −4、B 9。
#[tokio::test]
async fn every_unconfirmed_shortage_is_listed_at_once() {
    let node = node();
    let flour = flour_ab(&node).await;
    let lines = |confirm0: bool, confirm2: bool| {
        let l0 = at(wline(&flour, 8), A);
        let l2 = at(wline(&flour, 1), A);
        vec![
            if confirm0 { confirmed(l0) } else { l0 },
            wline(&flour, 1),
            if confirm2 { confirmed(l2) } else { l2 },
        ]
    };
    let line0 = short(0, &flour, Some((A, 5)), 8, 15);
    let line2 = short(2, &flour, Some((A, -3)), 1, 6);
    let before = node.state();

    for (confirm0, confirm2, listed) in [
        (false, false, json!([line0, line2])),
        (true, false, json!([line2])),
        (false, true, json!([line0])),
    ] {
        let reply = node
            .waste(&body(&cmd(10), lines(confirm0, confirm2), 0))
            .await;
        assert_eq!(
            assert_error(&reply, 409, "WASTE_CONFIRMATION_REQUIRED"),
            &json!({ "lines": listed }),
            "{confirm0} {confirm2}"
        );
    }
    assert_eq!(node.state(), before);

    let record = node.waste_ok(&body(&cmd(10), lines(true, true), 0)).await;
    assert_eq!(
        record["lines"],
        json!([
            logged(&flour, Some((A, 5)), 8, 15, vec![specified(A, 8)]),
            logged(&flour, None, 1, 7, vec![fifo(B, 1)]),
            logged(&flour, Some((A, -3)), 1, 6, vec![specified(A, 1)]),
        ])
    );
    assert_eq!(node.lots(), [pair(A, -4), pair(B, 9)]);
    assert_eq!(node.on_hand(&flour), 5);
}

// domain「报损接口」需要确认的条件用严格大于：报损量等于批次账面、等于净账面时不需要确认，扣到 0。
// A 5、B 10；lines[0] 指定 A 5（批次 5、净账面 15），lines[1] 不指定批次 10（净账面 10）。预检两行 needs_confirmation 都是
// false，且不写任何内容；正式提交不带确认直接入账：lines[1] 的 FIFO 跳过余量为 0 的 A，分配 B 10；A 0、B 0，没有账外缺口。
#[tokio::test]
async fn quantity_equal_to_the_book_needs_no_confirmation() {
    let node = node();
    let flour = flour_ab(&node).await;
    let before = node.state();
    let data = node
        .precheck_ok(vec![at(wline(&flour, 5), A), wline(&flour, 10)])
        .await;
    assert_eq!(
        data,
        json!({ "lines": [
            checked(0, &flour, Some((A, 5)), 5, 15, false, vec![specified(A, 5)]),
            checked(1, &flour, None, 10, 10, false, vec![fifo(B, 10)]),
        ] })
    );
    assert_eq!(node.state(), before);

    let record = node
        .waste_ok(&body(
            &cmd(10),
            vec![at(wline(&flour, 5), A), wline(&flour, 10)],
            0,
        ))
        .await;

    assert_eq!(
        record["lines"],
        json!([
            logged(&flour, Some((A, 5)), 5, 15, vec![specified(A, 5)]),
            logged(&flour, None, 10, 10, vec![fifo(B, 10)]),
        ])
    );
    assert_eq!(node.lots(), [pair(A, 0), pair(B, 0)]);
    assert_eq!(node.unallocated(), Vec::<(String, i64)>::new());
    assert_eq!(node.on_hand(&flour), 0);
}

// domain「报损接口」不足确认：账面足够的行带了确认标记照常处理，分配与不带确认相同；确认标记保存在规范化请求中，
// 不写进 payload。
#[tokio::test]
async fn confirmation_on_a_sufficient_line_is_accepted() {
    let node = node();
    let flour = flour_ab(&node).await;
    let request = body(&cmd(10), vec![confirmed(wline(&flour, 3))], 0);

    let record = node.waste_ok(&request).await;

    assert_eq!(
        record["lines"],
        json!([logged(&flour, None, 3, 15, vec![fifo(A, 3)])])
    );
    assert_eq!(
        node.wastes().unwrap()[0].payload,
        json!({ "lines": record["lines"] })
    );
    let (_, stored, _) = node.processed(&cmd(10)).unwrap();
    assert_eq!(stored, canonical(&request));
}

// ---------------------------------------------------------------------------------------------
// 预检（验收用例「预检只读」「确认后不足加大」「提交时新出现的不足」）
// ---------------------------------------------------------------------------------------------

// 验收用例「预检只读」与 domain「报损接口」预检：data.lines 每行 {line, item_id, lot_id?, qty, item_book_qty, lot_book_qty?,
// needs_confirmation, alloc}，按正式提交的逐行规则计算，假设需要确认的行都已确认。面粉 A 5、B 10（净账面 15），糖 S 20：
// - lines[0] 面粉指定 A 8：批次 5，需要确认，预计 A 8 SPECIFIED（之后 A −3，面粉净账面 7）；
// - lines[1] 糖 3：item_book_qty 是糖自己的 20，不受面粉的扣减或两种物料合计的影响；预计 S 3（之后糖 17）；
// - lines[2] 面粉不指定批次 5：面粉净账面仍是 7（不受糖的行影响），不需要确认，FIFO 跳过 A，预计 B 5（之后 2）；
// - lines[3] 面粉不指定批次 6：需要确认，预计 B 5 + 1 SHORTFALL（之后 −4）；
// - lines[4] 面粉指定 B 2：批次 0、净账面 −4，需要确认，预计 B 2 SPECIFIED；
// - lines[5] 糖 4：糖的账面接着自己的 lines[1] 算，为 17。
// 不指定批次的行没有 lot_id、lot_book_qty 两个键；warnings 为空。预检不写事件、processed_commands 或任何投影。
// 店长同样可以预检：面粉 3 g 不需要确认，needs_confirmation 为 false；1 bag 按当前系数换算成 1000 g，qty 是基本单位数量，
// 净账面 12 不足，预计 A 2 + B 10 FIFO、988 SHORTFALL。
#[tokio::test]
async fn precheck_reports_each_line_and_writes_nothing() {
    let node = node();
    let flour = flour_ab(&node).await;
    let (sugar, s) = node.sugar(10, 20).await;
    let before = node.state();

    let data = node
        .precheck_ok(vec![
            at(wline(&flour, 8), A),
            wline(&sugar, 3),
            wline(&flour, 5),
            wline(&flour, 6),
            at(wline(&flour, 2), B),
            wline(&sugar, 4),
        ])
        .await;

    assert_eq!(
        data,
        json!({ "lines": [
            checked(0, &flour, Some((A, 5)), 8, 15, true, vec![specified(A, 8)]),
            checked(1, &sugar, None, 3, 20, false, vec![fifo(&s, 3)]),
            checked(2, &flour, None, 5, 7, false, vec![fifo(B, 5)]),
            checked(3, &flour, None, 6, 2, true, vec![fifo(B, 5), shortfall(1)]),
            checked(4, &flour, Some((B, 0)), 2, -4, true, vec![specified(B, 2)]),
            checked(5, &sugar, None, 4, 17, false, vec![fifo(&s, 4)]),
        ] })
    );
    let bag = json!({
        "item_id": flour, "input": { "qty": 1, "unit_code": "bag", "base_qty_per_unit": 1000 },
        "reason_code": "EXPIRED",
    });
    let reply = node
        .post(
            &MANAGER,
            PRECHECK,
            &json!({ "lines": [wline(&flour, 3), bag] }),
        )
        .await;
    assert_eq!(
        assert_success(&reply),
        &json!({ "lines": [
            checked(0, &flour, None, 3, 15, false, vec![fifo(A, 3)]),
            checked(1, &flour, None, 1000, 12, true, vec![fifo(A, 2), fifo(B, 10), shortfall(988)]),
        ] })
    );
    assert_eq!(node.state(), before);
}

// 验收用例「确认后不足加大」：A 5；预检指定 A 报 8（lot_book_qty 5，需要确认）后员工确认；提交前另一台平板报损 A 2
// （不需要确认）已入账；带确认提交照常入账，确认不绑定预检时的账面：lot_book_qty、item_book_qty 记提交时的值 3、13；A −5。
#[tokio::test]
async fn confirmation_holds_when_the_shortage_grows() {
    let node = node();
    let flour = flour_ab(&node).await;
    let data = node.precheck_ok(vec![at(wline(&flour, 8), A)]).await;
    assert_eq!(
        data["lines"][0],
        checked(0, &flour, Some((A, 5)), 8, 15, true, vec![specified(A, 8)])
    );
    let reply = node
        .post(
            &MANAGER,
            URI,
            &body(&cmd(10), vec![at(wline(&flour, 2), A)], 0),
        )
        .await;
    record(&reply);

    let record = node
        .waste_ok(&body(&cmd(11), vec![confirmed(at(wline(&flour, 8), A))], 0))
        .await;

    assert_eq!(
        record["lines"],
        json!([logged(&flour, Some((A, 3)), 8, 13, vec![specified(A, 8)])])
    );
    assert_eq!(node.lots(), [pair(A, -5), pair(B, 10)]);
}

// 验收用例「提交时新出现的不足」：A 5；预检指定 A 报 4 不需要确认；提交前另一台平板报损 A 2 已入账；不带确认提交时
// 在写事务内重新计算：409 WASTE_CONFIRMATION_REQUIRED（lot_book_qty 3、item_book_qty 13），不入账。
#[tokio::test]
async fn a_shortage_appearing_at_submit_requires_confirmation() {
    let node = node();
    let flour = flour_ab(&node).await;
    let data = node.precheck_ok(vec![at(wline(&flour, 4), A)]).await;
    assert_eq!(data["lines"][0]["needs_confirmation"], json!(false));
    let reply = node
        .post(
            &MANAGER,
            URI,
            &body(&cmd(10), vec![at(wline(&flour, 2), A)], 0),
        )
        .await;
    record(&reply);
    let before = node.state();

    let reply = node
        .waste(&body(&cmd(11), vec![at(wline(&flour, 4), A)], 0))
        .await;

    assert_eq!(
        assert_error(&reply, 409, "WASTE_CONFIRMATION_REQUIRED"),
        &json!({ "lines": [short(0, &flour, Some((A, 3)), 4, 13)] })
    );
    assert_eq!(node.state(), before);
}

// domain「报损接口」业务校验：不足确认在其余业务校验都通过之后判定。lines[0] 是没带确认的不足行，同时另一处有其他业务错误时，
// 返回那个错误，不是 WASTE_CONFIRMATION_REQUIRED：换算系数变了（409 UNIT_CONVERSION_CHANGED）、报损原因不存在（404）、
// 批次属于其他物料（400 LOT_ITEM_MISMATCH）、超过 72 小时（400 CAPTURE_TOO_OLD）。都不写任何内容。
#[tokio::test]
async fn confirmation_is_judged_after_other_business_checks() {
    let node = node();
    let flour = flour_ab(&node).await;
    let (sugar, _) = node.sugar(20, 1).await;
    node.update_item(11, &flour, 1, &[("bag", 2000)], true)
        .await;
    let short_line = at(wline(&flour, 8), A);
    let bag = json!({
        "item_id": flour, "input": { "qty": 1, "unit_code": "bag", "base_qty_per_unit": 1000 },
        "reason_code": "EXPIRED",
    });
    let mut unknown_reason = wline(&flour, 1);
    unknown_reason["reason_code"] = json!("MISSING");
    let before = node.state();

    let reply = node
        .waste(&body(&cmd(12), vec![short_line.clone(), bag], 0))
        .await;
    assert_eq!(
        assert_error(&reply, 409, "UNIT_CONVERSION_CHANGED"),
        &json!({ "line": 1, "item_id": flour, "unit_code": "bag", "base_qty_per_unit": 2000 })
    );
    let reply = node
        .waste(&body(&cmd(12), vec![short_line.clone(), unknown_reason], 0))
        .await;
    assert_eq!(
        assert_error(&reply, 404, "REFERENCE_NOT_FOUND"),
        &json!({ "entity": "WASTE_REASON", "code": "MISSING" })
    );
    let reply = node
        .waste(&body(
            &cmd(12),
            vec![short_line.clone(), at(wline(&sugar, 1), B)],
            0,
        ))
        .await;
    assert_eq!(
        assert_error(&reply, 400, "LOT_ITEM_MISMATCH"),
        &json!({ "line": 1, "item_id": sugar, "lot_id": B })
    );
    let reply = node
        .waste(&body(&cmd(12), vec![short_line], MAX_LAG + 1))
        .await;
    assert_eq!(assert_error(&reply, 400, "CAPTURE_TOO_OLD"), &json!({}));

    assert_eq!(node.state(), before);
}

// ---------------------------------------------------------------------------------------------
// FIFO（验收用例「同次收货多个批次」「同次收货重复提交」「批次日期与流水号」）
// ---------------------------------------------------------------------------------------------

// 验收用例「同次收货多个批次」「同次收货重复提交」（日期换成 10-06）：一次收货面粉两行，lines[0] 建 A（…-001）5 g、
// lines[1] 建 B（…-002）10 g；报损 8 g，不指定批次：按批次号分配 A 5 FIFO + B 3 FIFO，余量 A 0、B 7。
// 之后用原 command_id、原内容（sent_at 不同）重发收货：返回原批次号 A、B，seq 不增加，余量仍为 A 0、B 7。
#[tokio::test]
async fn fifo_follows_lot_numbers_within_one_receipt() {
    let node = node();
    let flour = node.item(1, "FLOUR", &[]).await;
    let supplier = node.supplier(2).await;
    node.reason(3, "EXPIRED").await;
    let receipt = receipt_body(
        4,
        &supplier,
        vec![
            rline(&flour, 5, "2026-10-20"),
            rline(&flour, 10, "2026-10-20"),
        ],
        0,
    );
    let first = node.post(&STAFF, "/api/v1/receipts", &receipt).await;
    assert_success(&first);

    let record = node
        .waste_ok(&body(&cmd(5), vec![wline(&flour, 8)], 0))
        .await;
    assert_eq!(
        record["lines"],
        json!([logged(&flour, None, 8, 15, vec![fifo(A, 5), fifo(B, 3)])])
    );
    assert_eq!(node.lots(), [pair(A, 0), pair(B, 7)]);

    let before = node.state();
    let mut retry = receipt.clone();
    retry["sent_at"] = json!(SENT + HOUR);
    let reply = node.post(&STAFF, "/api/v1/receipts", &retry).await;
    assert_eq!(reply.body, first.body);
    assert_eq!(
        reply.body["data"]["receipt"]["lines"][0]["lot_id"],
        json!(A)
    );
    assert_eq!(node.state(), before);
    assert_eq!(node.lots(), [pair(A, 0), pair(B, 7)]);
}

// 验收用例「批次日期与流水号」与 domain「批次」FIFO：按批次日期升序、同一日期内按流水号升序扣减，
// 不按 seq、到期日或实物发生钟点。现在是 10-06 09:00：
// - A：当天收货 5 g，到期 10-15，RAW-FLOUR-20261006-001；
// - C：当天收货 5 g，occurred_at 为 07:00，但后于 A 入账，因此流水号为 002（…-20261006-002）；到期 10-10（最早到期）；
// - D：最后入账、补 10-04 的收货 5 g，到期 10-30，…-20261004-001。
// 报损 12 g，不指定批次：D 5 → A 5 → C 2。
#[tokio::test]
async fn fifo_orders_by_lot_date_then_serial() {
    let node = node();
    let flour = node.item(1, "FLOUR", &[]).await;
    let supplier = node.supplier(2).await;
    node.reason(3, "EXPIRED").await;
    let a = node
        .receive(4, &supplier, vec![rline(&flour, 5, "2026-10-15")], 0)
        .await;
    let c = node
        .receive(5, &supplier, vec![rline(&flour, 5, "2026-10-10")], 2 * HOUR)
        .await;
    let d = node
        .receive(
            6,
            &supplier,
            vec![rline(&flour, 5, "2026-10-30")],
            48 * HOUR,
        )
        .await;
    assert_eq!([&a[0], &c[0], &d[0]], [A, B, "RAW-FLOUR-20261004-001"]);

    let record = node
        .waste_ok(&body(&cmd(7), vec![wline(&flour, 12)], 0))
        .await;

    assert_eq!(
        record["lines"],
        json!([logged(
            &flour,
            None,
            12,
            15,
            vec![fifo(&d[0], 5), fifo(A, 5), fifo(B, 2)]
        )])
    );
    assert_eq!(
        node.lots(),
        [pair("RAW-FLOUR-20261004-001", 0), pair(A, 0), pair(B, 3)]
    );
}

// ---------------------------------------------------------------------------------------------
// 幂等与处理顺序
// ---------------------------------------------------------------------------------------------

// 验收用例「幂等」「确认标记与幂等」与 domain「报损接口」重提：
// - 带确认的报损成功后，库存又被另一条报损改变；一小时后用原 command_id、原内容（含确认标记）重发，
//   sent_at 不同（包括会超过 72 小时的值）：原样返回首次响应（含 warnings 和首次的账面、分配），seq 不增加，不重新计算。
// - 确认标记省略与 false 等价：原请求省略的行改写为 false，或原请求为 false 的行改为省略，都返回首次响应，不是冲突。
#[tokio::test]
async fn retries_return_the_original_response() {
    let node = node();
    let flour = flour_ab(&node).await;
    let captured_at = SENT + MINUTE;
    let lines = vec![confirmed(at(wline(&flour, 8), A)), wline(&flour, 2)];
    let first = node
        .waste(&timed_body(&cmd(10), lines.clone(), captured_at, SENT))
        .await;
    assert_eq!(
        record(&first)["lines"],
        json!([
            logged(&flour, Some((A, 5)), 8, 15, vec![specified(A, 8)]),
            logged(&flour, None, 2, 7, vec![fifo(B, 2)]),
        ])
    );
    assert_warnings(&first, &[("CAPTURE_TIME_ADJUSTED", json!({}))]);
    let mut with_false = wline(&flour, 1);
    with_false["confirm_shortage"] = json!(false);
    let second = node.waste(&body(&cmd(11), vec![with_false], 0)).await;
    record(&second);
    let before = node.state();
    node.clock.advance(Duration::from_secs(3600));

    for sent_at in [SENT, captured_at + 10 * MINUTE, captured_at + MAX_LAG + 1] {
        let reply = node
            .waste(&timed_body(&cmd(10), lines.clone(), captured_at, sent_at))
            .await;
        assert_eq!(reply.status, 200, "sent_at = {sent_at}");
        assert_eq!(reply.body, first.body, "sent_at = {sent_at}");
    }
    let mut explicit_false = lines.clone();
    explicit_false[1]["confirm_shortage"] = json!(false);
    let reply = node
        .waste(&timed_body(&cmd(10), explicit_false, captured_at, SENT))
        .await;
    assert_eq!(reply.body, first.body);
    let reply = node.waste(&body(&cmd(11), vec![wline(&flour, 1)], 0)).await;
    assert_eq!(reply.body, second.body);

    assert_eq!(node.state(), before);
}

// AGENTS「幂等」：幂等检查先于业务校验，包括单位核对。原报损用 2 bag（2000 g）成功（lag 为负，带 CAPTURE_TIME_ADJUSTED）；
// 之后 bag 的系数改为 2000，再删除 bag。每次修改后：新命令用同样内容会被拒绝（409 UNIT_CONVERSION_CHANGED、400 UNKNOWN_UNIT），
// 说明修改已生效；用原 command_id、原内容重试仍原样返回首次响应（含 warnings），什么都不写。
#[tokio::test]
async fn retries_skip_unit_checks_after_the_unit_changes() {
    let node = node();
    let flour = node.item(1, "FLOUR", &[("bag", 1000)]).await;
    let supplier = node.supplier(2).await;
    node.reason(3, "EXPIRED").await;
    node.receive(4, &supplier, vec![rline(&flour, 5000, "2026-10-20")], 0)
        .await;
    let lines = vec![json!({
        "item_id": flour, "input": { "qty": 2, "unit_code": "bag", "base_qty_per_unit": 1000 },
        "reason_code": "EXPIRED",
    })];
    let first = node
        .waste(&timed_body(&cmd(5), lines.clone(), SENT + MINUTE, SENT))
        .await;
    assert_eq!(record(&first)["lines"][0]["qty"], json!(2000));
    assert_warnings(&first, &[("CAPTURE_TIME_ADJUSTED", json!({}))]);

    for (n, units, status, code) in [
        (6, vec![("bag", 2000)], 409, "UNIT_CONVERSION_CHANGED"),
        (7, vec![], 400, "UNKNOWN_UNIT"),
    ] {
        node.update_item(n, &flour, i64::from(n) - 5, &units, true)
            .await;
        let before = node.state();
        let reply = node.waste(&body(&cmd(20 + n), lines.clone(), 0)).await;
        assert_error(&reply, status, code);
        let reply = node
            .waste(&timed_body(
                &cmd(5),
                lines.clone(),
                SENT + MINUTE,
                SENT + HOUR,
            ))
            .await;
        assert_eq!(reply.body, first.body, "{code}");
        assert_eq!(node.state(), before, "{code}");
    }
}

// AGENTS「幂等」与 domain「报损接口」重提：成功之后同一 command_id、内容不同时 409 IDEMPOTENCY_CONFLICT，
// details.fields 为不同的顶层字段名（字典序）。去掉、改为 false 或新加确认标记，改数量、批次、原因，行序不同，都列为 "lines"；
// command_type 不同时只列 "command_type"。幂等检查先于业务校验：改成不存在的原因、72 小时以外的 captured_at，仍是 409。
#[tokio::test]
async fn idempotency_conflicts_list_differing_fields() {
    let node = node();
    let flour = flour_ab(&node).await;
    let lines = vec![confirmed(at(wline(&flour, 8), A)), wline(&flour, 2)];
    let x = cmd(10);
    node.waste_ok(&body(&x, lines.clone(), 10 * MINUTE)).await;
    let before = node.state();

    let changed = |i: usize, f: &dyn Fn(&mut Value)| {
        let mut changed = lines.clone();
        f(&mut changed[i]);
        changed
    };
    let cases: Vec<(Value, Value)> = vec![
        (
            body(
                &x,
                changed(0, &|l| {
                    l.as_object_mut().unwrap().remove("confirm_shortage");
                }),
                10 * MINUTE,
            ),
            json!(["lines"]),
        ),
        (
            body(
                &x,
                changed(0, &|l| l["confirm_shortage"] = json!(false)),
                10 * MINUTE,
            ),
            json!(["lines"]),
        ),
        (
            body(
                &x,
                changed(1, &|l| l["confirm_shortage"] = json!(true)),
                10 * MINUTE,
            ),
            json!(["lines"]),
        ),
        (
            body(
                &x,
                changed(1, &|l| l["input"]["qty"] = json!(3)),
                10 * MINUTE,
            ),
            json!(["lines"]),
        ),
        (
            body(&x, changed(1, &|l| l["lot_id"] = json!(B)), 10 * MINUTE),
            json!(["lines"]),
        ),
        (
            body(
                &x,
                changed(1, &|l| l["reason_code"] = json!("MISSING")),
                10 * MINUTE,
            ),
            json!(["lines"]),
        ),
        (
            body(&x, vec![lines[1].clone(), lines[0].clone()], 10 * MINUTE),
            json!(["lines"]),
        ),
        // captured_at 与 sent_at 同时前移：lag 不变，只有 captured_at 不同。
        (
            timed_body(&x, lines.clone(), SENT - 11 * MINUTE, SENT - MINUTE),
            json!(["captured_at"]),
        ),
        (body(&x, lines.clone(), MAX_LAG + 1), json!(["captured_at"])),
        (
            body(&x, vec![lines[0].clone()], 0),
            json!(["captured_at", "lines"]),
        ),
        // 物料新建命令的 command_id 用于报损。
        (
            body(&cmd(1), lines.clone(), 10 * MINUTE),
            json!(["command_type"]),
        ),
    ];
    for (request, fields) in &cases {
        let reply = node.waste(request).await;
        assert_eq!(
            assert_error(&reply, 409, "IDEMPOTENCY_CONFLICT"),
            &json!({ "fields": fields }),
            "{request}"
        );
    }
    // 报损的 command_id 用于报损原因新建。
    let reply = node
        .post(
            &MANAGER,
            "/api/v1/waste-reasons",
            &json!({ "command_id": x, "code": "DAMAGED", "name": "损坏", "active": true }),
        )
        .await;
    assert_eq!(
        assert_error(&reply, 409, "IDEMPOTENCY_CONFLICT"),
        &json!({ "fields": ["command_type"] })
    );

    assert_eq!(node.state(), before);
}

// AGENTS「幂等」与 domain「报损接口」重提：被业务校验拒绝的命令不落库。收到 WASTE_CONFIRMATION_REQUIRED、
// 404 REFERENCE_NOT_FOUND 之后，同一 command_id 可以改内容重提（例如改小数量、不再需要确认）；成功之后再用被拒绝时的内容提交，
// 则是幂等冲突。
#[tokio::test]
async fn rejected_commands_can_be_resubmitted_with_changes() {
    let node = node();
    let flour = flour_ab(&node).await;
    let mut unknown_reason = wline(&flour, 1);
    unknown_reason["reason_code"] = json!("MISSING");

    let reply = node.waste(&body(&cmd(10), vec![unknown_reason], 0)).await;
    assert_error(&reply, 404, "REFERENCE_NOT_FOUND");
    let reply = node
        .waste(&body(&cmd(10), vec![at(wline(&flour, 8), A)], 0))
        .await;
    assert_error(&reply, 409, "WASTE_CONFIRMATION_REQUIRED");

    let record = node
        .waste_ok(&body(&cmd(10), vec![at(wline(&flour, 3), A)], 0))
        .await;
    assert_eq!(
        record["lines"],
        json!([logged(&flour, Some((A, 5)), 3, 15, vec![specified(A, 3)])])
    );
    let reply = node
        .waste(&body(&cmd(10), vec![at(wline(&flour, 8), A)], 0))
        .await;
    assert_eq!(
        assert_error(&reply, 409, "IDEMPOTENCY_CONFLICT"),
        &json!({ "fields": ["lines"] })
    );
    assert_eq!(node.wastes().unwrap().len(), 1);
}

// AGENTS「HTTP 约定」处理顺序：请求结构与取值校验先于幂等检查——已成功的 command_id 携带非法内容重发时是 400，不是 409。
#[tokio::test]
async fn validation_precedes_the_idempotency_check() {
    let node = node();
    let flour = flour_ab(&node).await;
    let original = body(&cmd(10), vec![wline(&flour, 1)], 0);
    node.waste_ok(&original).await;
    let before = node.state();

    for (key, value) in [
        ("lines", json!([])),
        ("lot_id", json!("raw-flour-20261006-001")),
        ("reason_code", json!("")),
        ("confirm_shortage", Value::Null),
        ("item_id", json!("FLOUR")),
    ] {
        let mut request = original.clone();
        if key == "lines" {
            request[key] = value;
        } else {
            request["lines"][0][key] = value;
        }
        let reply = node.waste(&request).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }

    assert_eq!(node.state(), before);
}

// domain「员工认证」开发桩与 AGENTS「HTTP 约定」：正式提交和预检都要求身份；缺少身份时 401 UNAUTHENTICATED，身份检查先于请求体——
// 请求体无法解析或取值非法时，无身份仍是 401。有身份时，请求体不是 JSON 对象、缺少 Content-Type 或不是 application/json
// 都是 400 VALIDATION_FAILED。两个路径都只有 POST，其他方法 405 METHOD_NOT_ALLOWED。什么都不写。
#[tokio::test]
async fn identity_is_required_and_checked_first() {
    let node = node();
    let flour = flour_ab(&node).await;
    let valid_submit = body(&cmd(10), vec![wline(&flour, 1)], 0);
    let valid_precheck = json!({ "lines": [wline(&flour, 1)] });
    let before = node.state();

    for (uri, valid) in [(URI, &valid_submit), (PRECHECK, &valid_precheck)] {
        let mut invalid = valid.clone();
        invalid["lines"] = json!([]);
        for payload in [valid, &invalid] {
            let reply = send(
                &node.router,
                request(Method::POST, uri, None, Some(payload)).unwrap(),
            )
            .await
            .unwrap();
            assert_error(&reply, 401, "UNAUTHENTICATED");
        }
        let reply = send(
            &node.router,
            raw_request(Method::POST, uri, None, Some("application/json"), "{").unwrap(),
        )
        .await
        .unwrap();
        assert_error(&reply, 401, "UNAUTHENTICATED");
        let json = Some("application/json");
        let valid_text = valid.to_string();
        for (text, content_type) in [
            ("{", json),
            ("[]", json),
            ("", json),
            (valid_text.as_str(), None),
            (valid_text.as_str(), Some("text/plain")),
        ] {
            let req = raw_request(Method::POST, uri, Some(&STAFF), content_type, text).unwrap();
            let reply = send(&node.router, req).await.unwrap();
            assert_error(&reply, 400, "VALIDATION_FAILED");
        }
        for method in [Method::GET, Method::PUT, Method::DELETE] {
            let reply = send(
                &node.router,
                request(method.clone(), uri, Some(&STAFF), None).unwrap(),
            )
            .await
            .unwrap();
            assert_error(&reply, 405, "METHOD_NOT_ALLOWED");
        }
    }

    assert_eq!(node.state(), before);
}

// ---------------------------------------------------------------------------------------------
// 取值、引用与单位
// ---------------------------------------------------------------------------------------------

/// 一行的取值错误：引用都存在、单位合法，只有被改动的字段不合法。正式提交和预检共用。
#[allow(clippy::unwrap_used)] // 测试夹具：改写的是调用方给出的合法行，取不到字段已是测试写错。
fn invalid_lines(valid: &Value) -> Vec<(&'static str, Value)> {
    let mut cases: Vec<(&'static str, Value)> = Vec::new();
    let mut row = |name: &'static str, f: &dyn Fn(&mut Value)| {
        let mut line = valid.clone();
        f(&mut line);
        cases.push((name, line));
    };
    row("line null", &|l| *l = Value::Null);
    row("item_id missing", &|l| {
        l.as_object_mut().unwrap().remove("item_id");
    });
    row("item_id not uuid", &|l| l["item_id"] = json!("FLOUR"));
    row("item_id uppercase", &|l| {
        l["item_id"] = json!(l["item_id"].as_str().unwrap().to_uppercase())
    });
    row("input missing", &|l| {
        l.as_object_mut().unwrap().remove("input");
    });
    row("input null", &|l| l["input"] = Value::Null);
    row("input.qty 0", &|l| l["input"]["qty"] = json!(0));
    row("input.qty negative", &|l| l["input"]["qty"] = json!(-1));
    row("input.qty string", &|l| l["input"]["qty"] = json!("2"));
    row("input.qty fraction", &|l| l["input"]["qty"] = json!(1.5));
    row("factor 0", &|l| l["input"]["base_qty_per_unit"] = json!(0));
    row("factor negative", &|l| {
        l["input"]["base_qty_per_unit"] = json!(-1)
    });
    row("unit_code empty", &|l| l["input"]["unit_code"] = json!(""));
    row("unit_code padded", &|l| {
        l["input"]["unit_code"] = json!(" g")
    });
    row("input unknown field", &|l| l["input"]["unit"] = json!("g"));
    row("qty overflow", &|l| {
        l["input"]["qty"] = json!(i64::MAX);
        l["input"]["base_qty_per_unit"] = json!(2);
    });
    row("reason_code missing", &|l| {
        l.as_object_mut().unwrap().remove("reason_code");
    });
    row("reason_code null", &|l| l["reason_code"] = Value::Null);
    row("reason_code empty", &|l| l["reason_code"] = json!(""));
    row("reason_code padded", &|l| {
        l["reason_code"] = json!("EXPIRED ")
    });
    row("reason_code number", &|l| l["reason_code"] = json!(1));
    row("lot_id null", &|l| l["lot_id"] = Value::Null);
    row("lot_id empty", &|l| l["lot_id"] = json!(""));
    row("lot_id uuid", &|l| l["lot_id"] = json!(UNKNOWN_ID));
    row("lot_id lowercase", &|l| {
        l["lot_id"] = json!("raw-flour-20261006-001")
    });
    row("lot_id unknown category", &|l| {
        l["lot_id"] = json!("PKG-FLOUR-20261006-001")
    });
    row("lot_id not a date", &|l| {
        l["lot_id"] = json!("RAW-FLOUR-20270229-001")
    });
    row("lot_id serial 000", &|l| {
        l["lot_id"] = json!("RAW-FLOUR-20261006-000")
    });
    row("lot_id padded", &|l| {
        l["lot_id"] = json!(" RAW-FLOUR-20261006-001")
    });
    row("qty field present", &|l| l["qty"] = json!(5));
    row("alloc present", &|l| l["alloc"] = json!([fifo(A, 5)]));
    row("item_book_qty present", &|l| l["item_book_qty"] = json!(15));
    row("unknown line field", &|l| l["note"] = json!("x"));
    cases
}

// domain「报损接口」取值与 AGENTS「HTTP 约定」：结构或取值错误一律 400 VALIDATION_FAILED，details 为 {}，什么都不写。
// lot_id 按批次号格式校验（同「批次查询」）；confirm_shortage 只接受布尔值，不接受 null、字符串或数字。
#[tokio::test]
async fn invalid_values_are_validation_failed() {
    let node = node();
    let flour = flour_ab(&node).await;
    let valid = body(&cmd(10), vec![wline(&flour, 1)], 0);
    let before = node.state();

    let mut cases: Vec<(&str, Value)> = Vec::new();
    let mut top = |name: &'static str, f: &dyn Fn(&mut Value)| {
        let mut request = valid.clone();
        f(&mut request);
        cases.push((name, request));
    };
    top("lines empty", &|r| r["lines"] = json!([]));
    top("lines missing", &|r| {
        r.as_object_mut().unwrap().remove("lines");
    });
    top("lines null", &|r| r["lines"] = Value::Null);
    top("lines object", &|r| r["lines"] = json!({}));
    top("captured_at missing", &|r| {
        r.as_object_mut().unwrap().remove("captured_at");
    });
    top("sent_at missing", &|r| {
        r.as_object_mut().unwrap().remove("sent_at");
    });
    top("occurred_at present", &|r| r["occurred_at"] = json!(NOW));
    top("unknown field", &|r| r["note"] = json!("x"));
    top("command_id missing", &|r| {
        r.as_object_mut().unwrap().remove("command_id");
    });
    top("command_id null", &|r| r["command_id"] = Value::Null);
    top("command_id not v7", &|r| {
        r["command_id"] = json!("01890a5d-ac96-474b-bcce-b30209e10010")
    });
    for (name, line) in invalid_lines(&valid["lines"][0]) {
        let mut request = valid.clone();
        request["lines"][0] = line;
        cases.push((name, request));
    }
    for (name, value) in [
        ("confirm_shortage null", Value::Null),
        ("confirm_shortage string", json!("true")),
        ("confirm_shortage number", json!(1)),
    ] {
        let mut request = valid.clone();
        request["lines"][0]["confirm_shortage"] = value;
        cases.push((name, request));
    }
    for text in non_canonical_uuids(&cmd(10)) {
        let mut request = valid.clone();
        request["command_id"] = json!(text);
        cases.push(("command_id non-canonical", request));
    }

    for (name, request) in &cases {
        let reply = node.waste(request).await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "{name}"
        );
    }

    assert_eq!(node.state(), before);
}

// domain「报损接口」预检：请求体只有 lines，行的取值规则同正式提交；不接受 command_id、captured_at、sent_at、occurred_at，
// 行内不接受 confirm_shortage（true、false 都不接受）。都是 400 VALIDATION_FAILED，details 为 {}，什么都不写。
#[tokio::test]
async fn invalid_precheck_requests_are_validation_failed() {
    let node = node();
    let flour = flour_ab(&node).await;
    let valid = json!({ "lines": [wline(&flour, 1)] });
    let before = node.state();

    let mut cases: Vec<(&str, Value)> = Vec::new();
    let mut top = |name: &'static str, f: &dyn Fn(&mut Value)| {
        let mut request = valid.clone();
        f(&mut request);
        cases.push((name, request));
    };
    top("lines empty", &|r| r["lines"] = json!([]));
    top("lines missing", &|r| {
        r.as_object_mut().unwrap().remove("lines");
    });
    top("lines null", &|r| r["lines"] = Value::Null);
    top("command_id present", &|r| r["command_id"] = json!(cmd(10)));
    top("captured_at present", &|r| r["captured_at"] = json!(SENT));
    top("sent_at present", &|r| r["sent_at"] = json!(SENT));
    top("occurred_at present", &|r| r["occurred_at"] = json!(NOW));
    top("unknown field", &|r| r["note"] = json!("x"));
    for (name, line) in invalid_lines(&valid["lines"][0]) {
        let mut request = valid.clone();
        request["lines"][0] = line;
        cases.push((name, request));
    }
    for (name, value) in [
        ("confirm_shortage true", json!(true)),
        ("confirm_shortage false", json!(false)),
    ] {
        let mut request = valid.clone();
        request["lines"][0]["confirm_shortage"] = value;
        cases.push((name, request));
    }

    for (name, request) in &cases {
        let reply = node.post(&STAFF, PRECHECK, request).await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "{name}"
        );
    }

    assert_eq!(node.state(), before);
}

// domain「报损接口」业务校验（正式提交与预检相同）：引用不存在时 404 REFERENCE_NOT_FOUND——物料 {entity: "ITEM", id}
// （含已存在的供应商 ID）；格式合法但不存在的批次 {entity: "LOT", id}（含同号不同类型；面粉已有合法批次 A、B，
// 也不能替它通过，不转去 FIFO 或记账外缺口）；报损原因按 code 查，区分大小写，
// {entity: "WASTE_REASON", code}。什么都不写。
#[tokio::test]
async fn unknown_references_are_not_found() {
    let node = node();
    let flour = flour_ab(&node).await;
    let supplier = spec_support::get(&node.router, "/api/v1/suppliers", Some(&STAFF))
        .await
        .unwrap()
        .body["data"]["suppliers"][0]["supplier_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let before = node.state();
    let reason = |code: &str| {
        let mut line = wline(&flour, 1);
        line["reason_code"] = json!(code);
        line
    };

    for (line, details) in [
        (
            wline(UNKNOWN_ID, 1),
            json!({ "entity": "ITEM", "id": UNKNOWN_ID }),
        ),
        (
            wline(&supplier, 1),
            json!({ "entity": "ITEM", "id": supplier }),
        ),
        (
            at(wline(&flour, 1), "RAW-FLOUR-20261006-003"),
            json!({ "entity": "LOT", "id": "RAW-FLOUR-20261006-003" }),
        ),
        (
            at(wline(&flour, 1), "SEMI-FLOUR-20261006-001"),
            json!({ "entity": "LOT", "id": "SEMI-FLOUR-20261006-001" }),
        ),
        (
            reason("MISSING"),
            json!({ "entity": "WASTE_REASON", "code": "MISSING" }),
        ),
        (
            reason("expired"),
            json!({ "entity": "WASTE_REASON", "code": "expired" }),
        ),
    ] {
        for reply in node.both(vec![wline(&flour, 1), line.clone()]).await {
            assert_eq!(
                assert_error(&reply, 404, "REFERENCE_NOT_FOUND"),
                &details,
                "{line}"
            );
        }
    }

    assert_eq!(node.state(), before);
}

// domain「报损接口」：指定的批次属于其他物料时 400 LOT_ITEM_MISMATCH，details 为 {line, item_id, lot_id}（line 是该行下标，
// item_id 是该行的物料）；正式提交与预检相同，什么都不写。糖已收货（有自己的批次），只有批次归属不对。
#[tokio::test]
async fn lot_of_another_item_is_a_mismatch() {
    let node = node();
    let flour = flour_ab(&node).await;
    let (sugar, _) = node.sugar(10, 1).await;
    let before = node.state();

    for reply in node
        .both(vec![wline(&flour, 1), at(wline(&sugar, 1), A)])
        .await
    {
        assert_eq!(
            assert_error(&reply, 400, "LOT_ITEM_MISMATCH"),
            &json!({ "line": 1, "item_id": sugar, "lot_id": A })
        );
    }

    assert_eq!(node.state(), before);
}

// domain「单位」与「报损接口」（同收货，正式提交与预检相同）：unit_code 既不是基本单位也不在 units 中（含已删除的单位、
// 大小写不同）：400 UNKNOWN_UNIT，details 为 {line, item_id, unit_code}；系数与当前值不同（用基本单位提交时当前值为 1）：
// 409 UNIT_CONVERSION_CHANGED，details 为 {line, item_id, unit_code, base_qty_per_unit: 当前值}。什么都不写。
// 按当前系数重新录入后，同一 command_id 可以重提。
#[tokio::test]
async fn unit_mismatches_are_rejected() {
    let node = node();
    let flour = flour_ab(&node).await;
    node.update_item(10, &flour, 1, &[("box", 3)], true).await;
    let unit = |qty: i64, unit_code: &str, factor: i64| {
        json!({
            "item_id": flour, "input": { "qty": qty, "unit_code": unit_code, "base_qty_per_unit": factor },
            "reason_code": "EXPIRED",
        })
    };
    let before = node.state();

    for unit_code in ["bag", "BOX", "kg"] {
        for reply in node
            .both(vec![wline(&flour, 1), unit(1, unit_code, 1000)])
            .await
        {
            assert_eq!(
                assert_error(&reply, 400, "UNKNOWN_UNIT"),
                &json!({ "line": 1, "item_id": flour, "unit_code": unit_code }),
                "{unit_code}"
            );
        }
    }
    for (line, current) in [(unit(1, "box", 4), 3), (unit(1, "g", 2), 1)] {
        for reply in node.both(vec![wline(&flour, 1), line.clone()]).await {
            let unit_code = line["input"]["unit_code"].clone();
            assert_eq!(
                assert_error(&reply, 409, "UNIT_CONVERSION_CHANGED"),
                &json!({ "line": 1, "item_id": flour, "unit_code": unit_code, "base_qty_per_unit": current }),
                "{line}"
            );
        }
    }
    assert_eq!(node.state(), before);

    let record = node
        .waste_ok(&body(
            &cmd(99),
            vec![wline(&flour, 1), unit(2, "box", 3)],
            0,
        ))
        .await;
    assert_eq!(record["lines"][1]["qty"], json!(6));
    assert_eq!(
        record["lines"][1]["input"],
        json!({ "qty": 2, "unit_code": "box", "base_qty_per_unit": 3 })
    );
}

// interfaces「calibrate」：sent_at − captured_at 溢出（正、负两个方向）时 400 VALIDATION_FAILED（details 为 {}）；
// 时间差恰好是 i64::MAX 时不溢出，先命中「超过 72 小时」，400 CAPTURE_TOO_OLD。都什么都不写。
#[tokio::test]
async fn out_of_range_times_are_validation_failed() {
    let node = node();
    let flour = flour_ab(&node).await;
    let before = node.state();

    for (captured_at, sent_at) in [(i64::MIN, i64::MAX), (i64::MAX, i64::MIN)] {
        let reply = node
            .waste(&timed_body(
                &cmd(10),
                vec![wline(&flour, 1)],
                captured_at,
                sent_at,
            ))
            .await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "{captured_at} {sent_at}"
        );
    }
    let reply = node
        .waste(&timed_body(
            &cmd(10),
            vec![wline(&flour, 1)],
            i64::MIN + 1,
            0,
        ))
        .await;
    assert_eq!(assert_error(&reply, 400, "CAPTURE_TOO_OLD"), &json!({}));

    assert_eq!(node.state(), before);
}
