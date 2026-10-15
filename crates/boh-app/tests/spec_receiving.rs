//! 锁定测试：收货接口。规则见 docs/domain.md「收货接口」「批次」「数量」「单位」「时间」「事件目录」「投影表」「验收用例」，
//! AGENTS.md「幂等」「HTTP 约定」「ID 与时间」「数值」，接口见 docs/interfaces.md。
//! 数量与换算系数在请求、响应和 payload 中是小数字符串；inventory_lots、inventory_movements、inventory_on_hand 中的数量
//! 是千分之一基本单位（1 g 存为 1000）。

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
    Actor, JsonReply, MANAGER, STAFF, assert_error, assert_success, is_lot_number, is_uuid_v7,
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
const URI: &str = "/api/v1/receipts";

/// `expires_on` 在 Asia/Shanghai 的当日最后一毫秒（次日当地 0 点的 UTC 毫秒减 1）。
const END_OF_10_06: i64 = 1_791_302_399_999;
const END_OF_10_07: i64 = 1_791_388_799_999;
const END_OF_10_08: i64 = 1_791_475_199_999;
const END_OF_10_15: i64 = 1_792_079_999_999;
const END_OF_10_20: i64 = 1_792_511_999_999;
const END_OF_10_22: i64 = 1_792_684_799_999;
const END_OF_9998_12_31: i64 = 253_370_735_999_999;
/// 2026-10-08 02:00 +08:00（营业日 2026-10-07）。
const AT_10_08_0200: i64 = 1_791_396_000_000;
/// 2026-10-08 00:30 +08:00。
const AT_10_08_0030: i64 = 1_791_390_600_000;

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

/// `inventory_lots` 的一行：(lot_id, item_id, lot_date, lot_serial, origin, source_event_seq, source_line_no,
/// remaining_qty, expires_at, manufacturer_lot_no)。
type LotRow = (
    String,
    String,
    String,
    i64,
    String,
    i64,
    i64,
    i64,
    Option<i64>,
    Option<String>,
);

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
type State = (
    Vec<Vec<SqlValue>>,
    Vec<Vec<SqlValue>>,
    Vec<Vec<SqlValue>>,
    Vec<Vec<SqlValue>>,
    Vec<Vec<SqlValue>>,
);

impl Node {
    #[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试；状态码和错误码由调用方断言。
    async fn receive(&self, actor: &Actor, body: &Value) -> JsonReply {
        spec_support::post(&self.router, URI, Some(actor), body)
            .await
            .unwrap()
    }

    /// 收货成功，返回 `data.receipt`。
    async fn receive_ok(&self, actor: &Actor, body: &Value) -> Value {
        let reply = self.receive(actor, body).await;
        receipt(&reply).clone()
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态的请求失败时，后续步骤没有意义，直接终止测试。
    async fn master(&self, uri: &str, key: &str, id_key: &str, body: Value) -> String {
        let reply = spec_support::post(&self.router, uri, Some(&MANAGER), &body)
            .await
            .unwrap();
        assert_success(&reply)[key][id_key]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// 新建一个以克为基本单位的物料；`units` 为 `[(unit_code, base_qty_per_unit)]`，须按 `unit_code` 升序。
    async fn item(&self, command_id: &str, code: &str, units: &[(&str, &str)]) -> String {
        self.master(
            "/api/v1/items",
            "item",
            "item_id",
            json!({
                "command_id": command_id, "code": code, "name": code, "base_unit": "g",
                "category": "RAW", "units": units_json(units), "active": true,
            }),
        )
        .await
    }

    /// 修改物料的单位（同时可停用），`base_revision` 由调用方给出。
    #[allow(clippy::unwrap_used)] // 同上。
    async fn update_item(
        &self,
        command_id: &str,
        item_id: &str,
        base_revision: i64,
        units: &[(&str, &str)],
        active: bool,
    ) {
        let reply = spec_support::put(
            &self.router,
            &format!("/api/v1/items/{item_id}"),
            Some(&MANAGER),
            &json!({
                "command_id": command_id, "base_revision": base_revision, "name": item_id,
                "category": "RAW", "units": units_json(units), "active": active,
            }),
        )
        .await
        .unwrap();
        assert_success(&reply);
    }

    async fn supplier(&self, command_id: &str, code: &str) -> String {
        self.master(
            "/api/v1/suppliers",
            "supplier",
            "supplier_id",
            json!({ "command_id": command_id, "code": code, "name": code, "active": true }),
        )
        .await
    }

    #[allow(clippy::unwrap_used)] // 同上。
    async fn deactivate_supplier(&self, command_id: &str, supplier_id: &str) {
        let reply = spec_support::put(
            &self.router,
            &format!("/api/v1/suppliers/{supplier_id}"),
            Some(&MANAGER),
            &json!({ "command_id": command_id, "base_revision": 1, "name": "S", "active": false }),
        )
        .await
        .unwrap();
        assert_success(&reply);
    }

    /// 只含 `GOODS_RECEIVED` 的事件，按 `seq` 升序。
    fn receipts(&self) -> Rows<Vec<Event>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT seq, event_type, schema_version, aggregate_type, aggregate_id, aggregate_version,
                    command_id, actor_id, device_id, business_date, occurred_at, recorded_at, payload
             FROM store_events WHERE event_type = 'GOODS_RECEIVED' ORDER BY seq",
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

    fn lots(&self) -> Rows<Vec<LotRow>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT lot_id, item_id, lot_date, lot_serial, origin, source_event_seq, source_line_no,
                    remaining_qty, expires_at, manufacturer_lot_no
             FROM inventory_lots ORDER BY source_event_seq, source_line_no",
        )?;
        let rows = statement
            .query_map([], |r| {
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
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    fn movements(&self) -> Rows<Vec<MovementRow>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT event_seq, movement_no, item_id, lot_id, kind, alloc_source, nominal_qty,
                    qty_delta, absorbed_by_event_id, physical_at, business_date
             FROM inventory_movements ORDER BY event_seq, movement_no",
        )?;
        let rows = statement
            .query_map([], |r| {
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

    /// 视图 `inventory_on_hand` 中该物料的账面数。
    fn on_hand(&self, item_id: &str) -> Rows<i64> {
        let reader = spec_support::reader(&self.db_path)?;
        Ok(reader.query_row(
            "SELECT qty FROM inventory_on_hand WHERE item_id = ?1",
            [item_id],
            |r| r.get(0),
        )?)
    }

    /// `processed_commands` 中该命令的 (`command_type`, 规范化请求, `recorded_at`)。
    fn processed(&self, command_id: &str) -> Rows<(String, Value, i64)> {
        let reader = spec_support::reader(&self.db_path)?;
        let (command_type, request, recorded_at): (String, String, i64) = reader.query_row(
            "SELECT command_type, request, recorded_at FROM processed_commands WHERE command_id = ?1",
            [command_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok((command_type, serde_json::from_str(&request)?, recorded_at))
    }

    fn try_state(&self) -> Rows<State> {
        let reader = spec_support::reader(&self.db_path)?;
        Ok((
            spec_support::rows(&reader, "SELECT * FROM store_events ORDER BY seq")?,
            spec_support::rows(
                &reader,
                "SELECT * FROM processed_commands ORDER BY command_id",
            )?,
            spec_support::rows(&reader, "SELECT * FROM inventory_lots ORDER BY lot_id")?,
            spec_support::rows(
                &reader,
                "SELECT * FROM inventory_movements ORDER BY event_seq, movement_no",
            )?,
            spec_support::rows(
                &reader,
                "SELECT * FROM inventory_unallocated ORDER BY item_id",
            )?,
        ))
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：只读查询失败时无法比对状态，直接终止测试。
    fn state(&self) -> State {
        self.try_state().unwrap()
    }
}

fn units_json(units: &[(&str, &str)]) -> Value {
    Value::Array(
        units
            .iter()
            .map(|(code, qty)| json!({ "unit_code": code, "base_qty_per_unit": qty }))
            .collect(),
    )
}

/// 第 `n` 个命令 ID（UUIDv7）。
fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209a9{n:04x}")
}

/// 请求中的一行：`qty` 个 `unit_code`（每个 `factor` 个基本单位，都是小数字符串），默认生产日期 2026-10-01、到期 2026-10-20、
/// 金额 1000 分、没有生产商批号。
fn line(item_id: &str, qty: &str, unit_code: &str, factor: &str) -> Value {
    json!({
        "item_id": item_id,
        "input": { "qty": qty, "unit_code": unit_code, "base_qty_per_unit": factor },
        "produced_on": "2026-10-01",
        "expires_on": "2026-10-20",
        "line_cost_cents": 1000,
    })
}

/// 改写一行的到期日。
fn expiring(mut line: Value, expires_on: &str) -> Value {
    line["expires_on"] = json!(expires_on);
    line
}

/// 实时录入的请求体：平板在 `SENT − lag` 录入、在 `SENT` 发送。
fn body(command_id: &str, supplier_id: &str, lines: Vec<Value>, lag: i64) -> Value {
    timed_body(command_id, supplier_id, lines, SENT - lag, SENT)
}

fn timed_body(
    command_id: &str,
    supplier_id: &str,
    lines: Vec<Value>,
    captured_at: i64,
    sent_at: i64,
) -> Value {
    json!({
        "command_id": command_id,
        "supplier_id": supplier_id,
        "lines": lines,
        "captured_at": captured_at,
        "sent_at": sent_at,
    })
}

#[allow(clippy::unwrap_used)] // 测试夹具：成功响应缺少这些字段已违反接口约定，直接终止测试。
/// 取出成功响应中的收货行，并校验服务端生成的标识：收货 ID 是规范 UUIDv7，时间部分等于该命令的 `recorded_at`
/// （AGENTS.md「ID 与时间」：不用 `Uuid::now_v7()`）；每行的 `lot_id` 是批次号（domain.md「批次」）。
fn receipt(reply: &JsonReply) -> &Value {
    let receipt = &assert_success(reply)["receipt"];
    let recorded_at = receipt["recorded_at"].as_i64().unwrap();
    let receipt_id = receipt["receipt_id"].as_str().unwrap();
    assert!(is_uuid_v7(receipt_id), "{receipt}");
    assert_eq!(uuid_v7_millis(receipt_id), recorded_at, "{receipt_id}");
    for line in receipt["lines"].as_array().unwrap() {
        assert!(is_lot_number(line["lot_id"].as_str().unwrap()), "{receipt}");
    }
    receipt
}

#[allow(clippy::unwrap_used)] // 测试夹具：调用方已用 is_uuid_v7 校验格式，解析失败直接终止测试。
/// UUIDv7 文本前 48 位表示的 Unix 毫秒。
fn uuid_v7_millis(id: &str) -> i64 {
    i64::from_str_radix(&format!("{}{}", &id[0..8], &id[9..13]), 16).unwrap()
}

fn lot_ids(receipt: &Value) -> Vec<String> {
    receipt["lines"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|line| line["lot_id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
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

// ---------------------------------------------------------------------------------------------
// 写入成功
// ---------------------------------------------------------------------------------------------

// 收货接口「新建」：普通员工即可收货；一个命令写一条 GOODS_RECEIVED@2（新的 RECEIPT 聚合，version 1）和一行
// processed_commands；每行建一个批次并写一条流水。
// - qty 是 input 的单位换算结果（2 bag × 25000 = "50000"）；expires_at = expires_on 当日最后一毫秒（Asia/Shanghai）。
// - 投影的数量是千分之一基本单位：50000 g 的批次余量与流水都是 50000000。
// - 批次号 <类型>-<编码>-<批次日期>-<流水号>：面粉的两行按行序取 001、002，黄油另从 001 开始；批次日期是 occurred_at
//   （10-06 08:50）的当地日期；投影的 lot_date、lot_serial 由批次号拆出。
// - 同一物料的两行分别建批次，source_line_no 为原下标（domain「批次」来源）；用基本单位提交时系数为 1。
// - payload 的键顺序锁定在 golden 样本中；这里按 JSON 值比对。manufacturer_lot_no 省略时 payload、投影、响应都没有它。
// - 相对校准抵消平板时钟偏差；规范化请求保留 captured_at，剥离 command_id、sent_at。
#[tokio::test]
async fn receive_writes_event_lots_movements_and_command() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[("bag", "25000")]).await;
    let butter = node.item(&cmd(2), "BUTTER", &[]).await;
    let supplier = node.supplier(&cmd(3), "S1").await;
    let mut line0 = line(&flour, "2", "bag", "25000");
    line0["manufacturer_lot_no"] = json!("L-2026-1001");
    let line1 = expiring(line(&butter, "500", "g", "1"), "2026-10-15");
    let mut line2 = expiring(line(&flour, "3000", "g", "1"), "2026-10-22");
    line2["produced_on"] = json!("2026-10-06");
    line2["line_cost_cents"] = json!(0);
    let request = body(
        &cmd(4),
        &supplier,
        vec![line0.clone(), line1.clone(), line2.clone()],
        10 * MINUTE,
    );

    let reply = node.receive(&STAFF, &request).await;

    let receipt = receipt(&reply).clone();
    let receipt_id = receipt["receipt_id"].as_str().unwrap().to_owned();
    let lots = lot_ids(&receipt);
    assert_eq!(
        lots,
        [
            "RAW-FLOUR-20261006-001",
            "RAW-BUTTER-20261006-001",
            "RAW-FLOUR-20261006-002"
        ]
    );
    let occurred_at = NOW - 10 * MINUTE;
    let payload_lines = json!([
        {
            "item_id": flour, "qty": "50000",
            "input": { "qty": "2", "unit_code": "bag", "base_qty_per_unit": "25000" },
            "lot_id": lots[0], "manufacturer_lot_no": "L-2026-1001",
            "produced_on": "2026-10-01", "expires_on": "2026-10-20", "expires_at": END_OF_10_20,
            "line_cost_cents": 1000,
        },
        {
            "item_id": butter, "qty": "500",
            "input": { "qty": "500", "unit_code": "g", "base_qty_per_unit": "1" },
            "lot_id": lots[1],
            "produced_on": "2026-10-01", "expires_on": "2026-10-15", "expires_at": END_OF_10_15,
            "line_cost_cents": 1000,
        },
        {
            "item_id": flour, "qty": "3000",
            "input": { "qty": "3000", "unit_code": "g", "base_qty_per_unit": "1" },
            "lot_id": lots[2],
            "produced_on": "2026-10-06", "expires_on": "2026-10-22", "expires_at": END_OF_10_22,
            "line_cost_cents": 0,
        },
    ]);
    assert_eq!(
        reply.body,
        json!({
            "success": true,
            "data": { "receipt": {
                "receipt_id": receipt_id,
                "supplier_id": supplier,
                "lines": payload_lines,
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
    let seq = 4;
    assert_eq!(
        node.receipts().unwrap(),
        [Event {
            seq,
            event_type: "GOODS_RECEIVED".into(),
            schema_version: 2,
            aggregate_type: "RECEIPT".into(),
            aggregate_id: receipt_id,
            aggregate_version: 1,
            command_id: cmd(4),
            actor_id: STAFF.employee_id.into(),
            device_id: STAFF.device_id.into(),
            business_date: "2026-10-06".into(),
            occurred_at,
            recorded_at: NOW,
            payload: json!({ "supplier_id": supplier, "lines": payload_lines }),
        }]
    );
    assert_eq!(
        node.lots().unwrap(),
        [
            (
                lots[0].clone(),
                flour.clone(),
                "2026-10-06".into(),
                1,
                "RECEIPT".into(),
                seq,
                0,
                50_000_000,
                Some(END_OF_10_20),
                Some("L-2026-1001".into()),
            ),
            (
                lots[1].clone(),
                butter.clone(),
                "2026-10-06".into(),
                1,
                "RECEIPT".into(),
                seq,
                1,
                500_000,
                Some(END_OF_10_15),
                None,
            ),
            (
                lots[2].clone(),
                flour.clone(),
                "2026-10-06".into(),
                2,
                "RECEIPT".into(),
                seq,
                2,
                3_000_000,
                Some(END_OF_10_22),
                None,
            ),
        ]
    );
    let movement = |no: i64, item: &str, lot: &str, qty: i64| -> MovementRow {
        (
            seq,
            no,
            item.to_owned(),
            Some(lot.to_owned()),
            "RECEIPT".into(),
            "NEW_LOT".into(),
            qty,
            qty,
            None,
            occurred_at,
            "2026-10-06".into(),
        )
    };
    assert_eq!(
        node.movements().unwrap(),
        [
            movement(0, &flour, &lots[0], 50_000_000),
            movement(1, &butter, &lots[1], 500_000),
            movement(2, &flour, &lots[2], 3_000_000),
        ]
    );
    assert_eq!(node.on_hand(&flour).unwrap(), 53_000_000);
    assert_eq!(node.on_hand(&butter).unwrap(), 500_000);
    let mut canonical = request.clone();
    canonical.as_object_mut().unwrap().remove("command_id");
    canonical.as_object_mut().unwrap().remove("sent_at");
    assert_eq!(
        node.processed(&cmd(4)).unwrap(),
        ("receipt.create".into(), canonical, NOW)
    );
}

// 收货接口「权限」：店长同样可以收货；身份写进事件和响应。每次收货是一个新聚合。
#[tokio::test]
async fn managers_can_receive_and_each_receipt_is_a_new_aggregate() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;

    let first = node
        .receive_ok(
            &MANAGER,
            &body(&cmd(3), &supplier, vec![line(&flour, "1", "g", "1")], 0),
        )
        .await;
    let second = node
        .receive_ok(
            &STAFF,
            &body(&cmd(4), &supplier, vec![line(&flour, "1", "g", "1")], 0),
        )
        .await;

    assert_eq!(first["actor_id"], json!(MANAGER.employee_id));
    assert_eq!(first["device_id"], json!(MANAGER.device_id));
    assert_ne!(first["receipt_id"], second["receipt_id"]);
    let events = node.receipts().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].actor_id, MANAGER.employee_id);
    assert_eq!(events[0].aggregate_version, 1);
    assert_eq!(events[1].aggregate_version, 1);
    assert_ne!(events[0].aggregate_id, events[1].aggregate_id);
}

// 取值边界照常受理：manufacturer_lot_no 恰好 64 个字符（按 Unicode 字符计，含非 ASCII），首尾以外的空白原样保存；
// line_cost_cents = 0 与上限 99999999；produced_on = expires_on；input.qty 为录入上限 9999.999 和最小值 0.001；
// 换算系数为上限 1000000000（0.001 个 = 1000000 g）；9999.999 mg × 0.001 = 9.999999 g，四舍五入为 "10"；
// expires_on 恰好是上限 9998-12-31。投影中的余量是千分之一克。
#[tokio::test]
async fn boundary_values_are_accepted() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let big = node
        .item(&cmd(4), "BIG", &[("max", "1000000000"), ("mg", "0.001")])
        .await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    let lot_no = format!("批 号{}", "x".repeat(61));
    assert_eq!(lot_no.chars().count(), 64);
    let mut line0 = line(&flour, "1", "g", "1");
    line0["manufacturer_lot_no"] = json!(lot_no);
    line0["line_cost_cents"] = json!(0);
    line0["produced_on"] = json!("2026-10-06");
    line0["expires_on"] = json!("2026-10-06");
    let line1 = line(&flour, "9999.999", "g", "1");
    let mut line2 = line(&flour, "0.001", "g", "1");
    line2["expires_on"] = json!("9998-12-31");
    line2["line_cost_cents"] = json!(99_999_999);
    let line3 = line(&big, "0.001", "max", "1000000000");
    let line4 = line(&big, "9999.999", "mg", "0.001");

    let receipt = node
        .receive_ok(
            &STAFF,
            &body(
                &cmd(3),
                &supplier,
                vec![line0, line1, line2, line3, line4],
                0,
            ),
        )
        .await;

    assert_eq!(receipt["lines"][0]["manufacturer_lot_no"], json!(lot_no));
    assert_eq!(receipt["lines"][0]["expires_at"], json!(END_OF_10_06));
    assert_eq!(receipt["lines"][1]["qty"], json!("9999.999"));
    assert_eq!(receipt["lines"][2]["qty"], json!("0.001"));
    assert_eq!(receipt["lines"][2]["expires_at"], json!(END_OF_9998_12_31));
    assert_eq!(receipt["lines"][2]["line_cost_cents"], json!(99_999_999));
    assert_eq!(receipt["lines"][3]["qty"], json!("1000000"));
    assert_eq!(receipt["lines"][4]["qty"], json!("10"));
    let remaining: Vec<i64> = node.lots().unwrap().iter().map(|lot| lot.7).collect();
    assert_eq!(remaining, [1_000, 9_999_999, 1, 1_000_000_000, 10_000]);
}

// domain「主数据」：命令引用已停用的供应商和物料照常受理。
#[tokio::test]
async fn inactive_supplier_and_item_are_accepted() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    node.update_item(&cmd(3), &flour, 1, &[], false).await;
    node.deactivate_supplier(&cmd(4), &supplier).await;

    node.receive_ok(
        &STAFF,
        &body(&cmd(5), &supplier, vec![line(&flour, "1", "g", "1")], 0),
    )
    .await;

    assert_eq!(node.lots().unwrap().len(), 1);
}

// domain「时间」营业日：由 occurred_at（不是 recorded_at）按 Asia/Shanghai 与日切 04:00 计算；
// 批次流水的 physical_at 是事件的 occurred_at。
#[tokio::test]
async fn business_date_and_physical_at_follow_occurred_at() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;

    // recorded_at 09:00；lag 5 小时得 04:00，归当日；再早 1ms 归前一营业日。
    let today = node
        .receive_ok(
            &STAFF,
            &body(
                &cmd(3),
                &supplier,
                vec![line(&flour, "1", "g", "1")],
                5 * HOUR,
            ),
        )
        .await;
    let yesterday = node
        .receive_ok(
            &STAFF,
            &body(
                &cmd(4),
                &supplier,
                vec![line(&flour, "1", "g", "1")],
                5 * HOUR + 1,
            ),
        )
        .await;

    assert_eq!(today["business_date"], json!("2026-10-06"));
    assert_eq!(today["occurred_at"], json!(NOW - 5 * HOUR));
    assert_eq!(yesterday["business_date"], json!("2026-10-05"));
    assert_eq!(yesterday["occurred_at"], json!(NOW - 5 * HOUR - 1));
    let physical: Vec<(i64, String)> = node
        .movements()
        .unwrap()
        .into_iter()
        .map(|m| (m.9, m.10))
        .collect();
    assert_eq!(
        physical,
        [
            (NOW - 5 * HOUR, "2026-10-06".into()),
            (NOW - 5 * HOUR - 1, "2026-10-05".into()),
        ]
    );
}

// domain「时间」相对校准：lag < 0 按 0 处理并返回警告 CAPTURE_TIME_ADJUSTED（details 为 {}）；
// lag 超过 72 小时 400 CAPTURE_TOO_OLD（details 为 {}），恰好 72 小时照常受理。
#[tokio::test]
async fn relative_calibration_applies() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;

    let reply = node
        .receive(
            &STAFF,
            &body(
                &cmd(3),
                &supplier,
                vec![line(&flour, "1", "g", "1")],
                -MINUTE,
            ),
        )
        .await;
    assert_eq!(receipt(&reply)["occurred_at"], json!(NOW));
    assert_warnings(&reply, &[("CAPTURE_TIME_ADJUSTED", json!({}))]);

    let before = node.state();
    let reply = node
        .receive(
            &STAFF,
            &body(
                &cmd(4),
                &supplier,
                vec![line(&flour, "1", "g", "1")],
                MAX_LAG + 1,
            ),
        )
        .await;
    assert_eq!(assert_error(&reply, 400, "CAPTURE_TOO_OLD"), &json!({}));
    assert_eq!(node.state(), before);

    // 72 小时前是 2026-10-03 09:00，生产日期 2026-10-01 不晚于它，到期日也不早于它。
    let reply = node
        .receive(
            &STAFF,
            &body(
                &cmd(4),
                &supplier,
                vec![line(&flour, "1", "g", "1")],
                MAX_LAG,
            ),
        )
        .await;
    assert_eq!(receipt(&reply)["business_date"], json!("2026-10-03"));
    assert_warnings(&reply, &[]);
}

// ---------------------------------------------------------------------------------------------
// 到期提醒与日期校验（验收用例「收货日期」，场景「到期早于先扣的批次」「到货即过期」）
// ---------------------------------------------------------------------------------------------

// domain「批次」EXPIRES_BEFORE_OLDER_STOCK：同一天已有批次 A（RAW-FLOUR-20261006-001）余量 5、到期 10-20，
// B（…-002）余量 5、到期 10-25；一次收货的四行依次建 …-003～…-006，批次号都排在 A、B 之后：
// lines[0] 到期 10-15（早于 A、B），lines[1] 到期 10-25（不早于任何先扣的批次），lines[2] 到期 10-22（早于 B 和同次的 lines[1]）。
// 每个命中的行恰好一条警告（早于多个批次也只一条），按 line 升序，details 为 {line, item_id, lot_id}（新批次）；各行都入账。
// 另一物料的批次、到期相同的批次都不触发；CAPTURE_TIME_ADJUSTED 排在最前。
#[tokio::test]
async fn expiry_before_stock_used_first_warns_per_line() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let sugar = node.item(&cmd(2), "SUGAR", &[]).await;
    let supplier = node.supplier(&cmd(3), "S1").await;
    let a = node
        .receive_ok(
            &STAFF,
            &body(
                &cmd(4),
                &supplier,
                vec![
                    expiring(line(&flour, "5", "g", "1"), "2026-10-20"),
                    expiring(line(&flour, "5", "g", "1"), "2026-10-25"),
                ],
                0,
            ),
        )
        .await;
    // 另一物料到期很晚，不影响面粉。
    node.receive_ok(
        &STAFF,
        &body(
            &cmd(5),
            &supplier,
            vec![expiring(line(&sugar, "5", "g", "1"), "2027-04-06")],
            0,
        ),
    )
    .await;

    let reply = node
        .receive(
            &STAFF,
            &body(
                &cmd(6),
                &supplier,
                vec![
                    expiring(line(&flour, "1", "g", "1"), "2026-10-15"),
                    expiring(line(&flour, "1", "g", "1"), "2026-10-25"),
                    expiring(line(&flour, "1", "g", "1"), "2026-10-22"),
                    // 与同次的 lines[1] 同日到期：不早于任何先扣的批次。
                    expiring(line(&flour, "1", "g", "1"), "2026-10-25"),
                ],
                -MINUTE,
            ),
        )
        .await;

    let lots = lot_ids(receipt(&reply));
    assert_eq!(
        lots,
        [
            "RAW-FLOUR-20261006-003",
            "RAW-FLOUR-20261006-004",
            "RAW-FLOUR-20261006-005",
            "RAW-FLOUR-20261006-006"
        ]
    );
    assert_warnings(
        &reply,
        &[
            ("CAPTURE_TIME_ADJUSTED", json!({})),
            (
                "EXPIRES_BEFORE_OLDER_STOCK",
                json!({ "line": 0, "item_id": flour, "lot_id": lots[0] }),
            ),
            (
                "EXPIRES_BEFORE_OLDER_STOCK",
                json!({ "line": 2, "item_id": flour, "lot_id": lots[2] }),
            ),
        ],
    );
    assert_eq!(node.lots().unwrap().len(), 7);
    assert_eq!(node.on_hand(&flour).unwrap(), 14_000);
    assert_eq!(lot_ids(&a).len(), 2);
}

// domain「批次」EXPIRES_BEFORE_OLDER_STOCK 比较同次收货中排在前面的行：已有批次只有一个、到期 10-20；
// 一次收货 lines[0] 到期 10-25，lines[1] 到期 10-22。lines[1] 不早于已有批次，只早于同次的 lines[0]，
// 所以只警告 lines[1]；两行都入账。
#[tokio::test]
async fn expiry_before_earlier_line_of_same_receipt_warns() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    node.receive_ok(
        &STAFF,
        &body(
            &cmd(3),
            &supplier,
            vec![expiring(line(&flour, "5", "g", "1"), "2026-10-20")],
            0,
        ),
    )
    .await;

    let reply = node
        .receive(
            &STAFF,
            &body(
                &cmd(4),
                &supplier,
                vec![
                    expiring(line(&flour, "1", "g", "1"), "2026-10-25"),
                    expiring(line(&flour, "1", "g", "1"), "2026-10-22"),
                ],
                0,
            ),
        )
        .await;

    let lots = lot_ids(receipt(&reply));
    assert_warnings(
        &reply,
        &[(
            "EXPIRES_BEFORE_OLDER_STOCK",
            json!({ "line": 1, "item_id": flour, "lot_id": lots[1] }),
        )],
    );
    assert_eq!(node.lots().unwrap().len(), 3);
    assert_eq!(node.on_hand(&flour).unwrap(), 7_000);
}

// domain「批次」：同到期日不算「早于」；按 FIFO 先被扣的批次到期更早时也不警告。
#[tokio::test]
async fn equal_or_later_expiry_does_not_warn() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    node.receive_ok(
        &STAFF,
        &body(
            &cmd(3),
            &supplier,
            vec![expiring(line(&flour, "5", "g", "1"), "2026-10-15")],
            0,
        ),
    )
    .await;

    for (n, expires_on) in [(4, "2026-10-15"), (5, "2026-10-20")] {
        let reply = node
            .receive(
                &STAFF,
                &body(
                    &cmd(n),
                    &supplier,
                    vec![expiring(line(&flour, "1", "g", "1"), expires_on)],
                    0,
                ),
            )
            .await;
        receipt(&reply);
        assert_warnings(&reply, &[]);
    }
}

// 场景「到货即过期」（domain「收货接口」业务校验）：occurred_at 的当地日期为 10-08，expires_on 10-07 时 400 INVALID_LOT_DATES，
// details 为 {line, item_id, reason: "EXPIRED_ON_RECEIPT"}，什么都不写；expires_on 10-08 照常受理。
#[tokio::test]
async fn expired_on_receipt_is_rejected() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    node.clock.set(UnixMillis(AT_10_08_0200 + 8 * HOUR)); // 10-08 10:00
    let sent = AT_10_08_0200 + 8 * HOUR;
    let before = node.state();

    let reply = node
        .receive(
            &STAFF,
            &timed_body(
                &cmd(3),
                &supplier,
                vec![
                    line(&flour, "1", "g", "1"),
                    expiring(line(&flour, "1", "g", "1"), "2026-10-07"),
                ],
                sent,
                sent,
            ),
        )
        .await;
    assert_eq!(
        assert_error(&reply, 400, "INVALID_LOT_DATES"),
        &json!({ "line": 1, "item_id": flour, "reason": "EXPIRED_ON_RECEIPT" })
    );
    assert_eq!(node.state(), before);

    // 被业务校验拒绝的命令不落库，修正后可以用同一个 command_id 重提。
    let receipt = node
        .receive_ok(
            &STAFF,
            &timed_body(
                &cmd(3),
                &supplier,
                vec![expiring(line(&flour, "1", "g", "1"), "2026-10-08")],
                sent,
                sent,
            ),
        )
        .await;
    assert_eq!(receipt["lines"][0]["expires_at"], json!(END_OF_10_08));
}

// 验收用例「收货日期」（domain「收货接口」业务校验）：10-08 02:00 收货（营业日 10-07），produced_on 10-08 照常受理——判定用 occurred_at 的
// 日历日期，不用营业日；produced_on 10-09 时 400 INVALID_LOT_DATES，reason 为 PRODUCED_IN_FUTURE，什么都不写。
#[tokio::test]
async fn produced_on_is_checked_against_the_calendar_date() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    node.clock.set(UnixMillis(AT_10_08_0200));
    let today = |n: u16, produced_on: &str| {
        let mut l = line(&flour, "1", "g", "1");
        l["produced_on"] = json!(produced_on);
        timed_body(&cmd(n), &supplier, vec![l], AT_10_08_0200, AT_10_08_0200)
    };

    let receipt = node.receive_ok(&STAFF, &today(3, "2026-10-08")).await;
    assert_eq!(receipt["business_date"], json!("2026-10-07"));

    let before = node.state();
    let reply = node.receive(&STAFF, &today(4, "2026-10-09")).await;
    assert_eq!(
        assert_error(&reply, 400, "INVALID_LOT_DATES"),
        &json!({ "line": 0, "item_id": flour, "reason": "PRODUCED_IN_FUTURE" })
    );
    assert_eq!(node.state(), before);
}

// domain「收货接口」：日期按 occurred_at（不是 recorded_at）的当地日历日期判定。节点时间 10-08 00:30，lag 1 小时，
// occurred_at 为 10-07 23:30：produced_on 10-08 是未来；expires_on 10-07 不算过期。
#[tokio::test]
async fn lot_dates_follow_occurred_at_not_recorded_at() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    node.clock.set(UnixMillis(AT_10_08_0030));
    let sent = AT_10_08_0030;

    let mut future = line(&flour, "1", "g", "1");
    future["produced_on"] = json!("2026-10-08");
    let reply = node
        .receive(
            &STAFF,
            &timed_body(&cmd(3), &supplier, vec![future], sent - HOUR, sent),
        )
        .await;
    assert_eq!(
        assert_error(&reply, 400, "INVALID_LOT_DATES"),
        &json!({ "line": 0, "item_id": flour, "reason": "PRODUCED_IN_FUTURE" })
    );

    let mut last_day = expiring(line(&flour, "1", "g", "1"), "2026-10-07");
    last_day["produced_on"] = json!("2026-10-07");
    let receipt = node
        .receive_ok(
            &STAFF,
            &timed_body(&cmd(4), &supplier, vec![last_day], sent - HOUR, sent),
        )
        .await;
    assert_eq!(receipt["occurred_at"], json!(AT_10_08_0030 - HOUR));
    assert_eq!(receipt["lines"][0]["expires_at"], json!(END_OF_10_07));
}

// ---------------------------------------------------------------------------------------------
// 单位与引用
// ---------------------------------------------------------------------------------------------

// 验收用例「单位换算」（domain「单位」）：离线录入「2 袋，每袋 25000 g」；提交前换算改为 20000：409 UNIT_CONVERSION_CHANGED，
// details 为 {line, item_id, unit_code, base_qty_per_unit: 当前值}，不入账。按当前系数重新录入后可以用同一个 command_id 重提。
#[tokio::test]
async fn changed_conversion_is_a_conflict() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[("bag", "25000")]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    node.update_item(&cmd(3), &flour, 1, &[("bag", "20000")], true)
        .await;
    let before = node.state();

    let reply = node
        .receive(
            &STAFF,
            &body(
                &cmd(4),
                &supplier,
                vec![line(&flour, "2", "bag", "25000")],
                0,
            ),
        )
        .await;
    assert_eq!(
        assert_error(&reply, 409, "UNIT_CONVERSION_CHANGED"),
        &json!({ "line": 0, "item_id": flour, "unit_code": "bag", "base_qty_per_unit": "20000" })
    );
    assert_eq!(node.state(), before);

    let receipt = node
        .receive_ok(
            &STAFF,
            &body(
                &cmd(4),
                &supplier,
                vec![line(&flour, "2", "bag", "20000")],
                0,
            ),
        )
        .await;
    assert_eq!(receipt["lines"][0]["qty"], json!("40000"));
}

// domain「收货接口」：用基本单位提交时当前系数为 1，系数不是 1 也是 409 UNIT_CONVERSION_CHANGED；
// unit_code 既不是基本单位也不在 units 中（含已删除的单位、大小写不同）：400 UNKNOWN_UNIT。都不写任何内容。
#[tokio::test]
async fn unit_mismatches_are_rejected() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[("bag", "25000")]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    node.update_item(&cmd(3), &flour, 1, &[("box", "10000")], true)
        .await;
    let before = node.state();

    let reply = node
        .receive(
            &STAFF,
            &body(
                &cmd(4),
                &supplier,
                vec![
                    line(&flour, "1", "box", "10000"),
                    line(&flour, "5", "g", "1000"),
                ],
                0,
            ),
        )
        .await;
    assert_eq!(
        assert_error(&reply, 409, "UNIT_CONVERSION_CHANGED"),
        &json!({ "line": 1, "item_id": flour, "unit_code": "g", "base_qty_per_unit": "1" })
    );

    for unit_code in ["bag", "BOX", "kg"] {
        let reply = node
            .receive(
                &STAFF,
                &body(
                    &cmd(4),
                    &supplier,
                    vec![line(&flour, "1", unit_code, "25000")],
                    0,
                ),
            )
            .await;
        assert_eq!(
            assert_error(&reply, 400, "UNKNOWN_UNIT"),
            &json!({ "line": 0, "item_id": flour, "unit_code": unit_code }),
            "{unit_code}"
        );
    }

    assert_eq!(node.state(), before);
}

// domain「收货接口」：供应商或物料不存在时 404 REFERENCE_NOT_FOUND，details 为 {entity, id}；什么都不写。
// 物料 ID 是已存在的供应商 ID（反之亦然）同样不存在。
#[tokio::test]
async fn unknown_references_are_not_found() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    let before = node.state();

    for (supplier_id, item_id, entity, id) in [
        (UNKNOWN_ID, flour.as_str(), "SUPPLIER", UNKNOWN_ID),
        (flour.as_str(), flour.as_str(), "SUPPLIER", flour.as_str()),
        (supplier.as_str(), UNKNOWN_ID, "ITEM", UNKNOWN_ID),
        (
            supplier.as_str(),
            supplier.as_str(),
            "ITEM",
            supplier.as_str(),
        ),
    ] {
        let reply = node
            .receive(
                &STAFF,
                &body(&cmd(3), supplier_id, vec![line(item_id, "1", "g", "1")], 0),
            )
            .await;
        assert_eq!(
            assert_error(&reply, 404, "REFERENCE_NOT_FOUND"),
            &json!({ "entity": entity, "id": id })
        );
    }

    assert_eq!(node.state(), before);
}

// ---------------------------------------------------------------------------------------------
// 取值校验
// ---------------------------------------------------------------------------------------------

// domain「收货接口」取值、「写命令通用规则」取值与 AGENTS「HTTP 约定」：结构或取值错误一律 400 VALIDATION_FAILED，details 为 {}，什么都不写。
// 引用都存在、单位合法，只有被改动的字段不合法。
// - input.qty、base_qty_per_unit 的写法与范围见 domain「数量」「单位」（spec_support 的两张非法写法表）；
//   input.qty 大于 0 而换算结果四舍五入为 0（0.4 × 0.001 = 0.0004；0.001 × 0.499999999 = 0.000499999999 不到中点）也是取值错误，
//   只用请求中的值判定，与单位是否存在无关（这里的 bag 系数与当前值不同，换算为 0 先命中）。
// - line_cost_cents 是 JSON 整数 0～99999999。
#[tokio::test]
async fn invalid_values_are_validation_failed() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[("bag", "25000")]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    let valid = body(
        &cmd(3),
        &supplier,
        vec![line(&flour, "2", "bag", "25000")],
        0,
    );
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
    top("supplier_id missing", &|r| {
        r.as_object_mut().unwrap().remove("supplier_id");
    });
    top("supplier_id not uuid", &|r| r["supplier_id"] = json!("S1"));
    top("supplier_id uppercase", &|r| {
        r["supplier_id"] = json!(r["supplier_id"].as_str().unwrap().to_uppercase())
    });
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
        r["command_id"] = json!("01890a5d-ac96-474b-bcce-b30209a90003")
    });

    let mut row = |name: &'static str, f: &dyn Fn(&mut Value)| {
        let mut request = valid.clone();
        f(&mut request["lines"][0]);
        cases.push((name, request));
    };
    row("item_id not uuid", &|l| l["item_id"] = json!("FLOUR"));
    row("item_id uppercase", &|l| {
        l["item_id"] = json!(l["item_id"].as_str().unwrap().to_uppercase())
    });
    row("line null", &|l| *l = Value::Null);
    row("input null", &|l| l["input"] = Value::Null);
    row("item_id missing", &|l| {
        l.as_object_mut().unwrap().remove("item_id");
    });
    row("input missing", &|l| {
        l.as_object_mut().unwrap().remove("input");
    });
    row("input.qty missing", &|l| {
        l["input"].as_object_mut().unwrap().remove("qty");
    });
    row("factor missing", &|l| {
        l["input"]
            .as_object_mut()
            .unwrap()
            .remove("base_qty_per_unit");
    });
    row("unit_code missing", &|l| {
        l["input"].as_object_mut().unwrap().remove("unit_code");
    });
    row(
        "conversion rounds to 0",
        &|l| l["input"] = json!({ "qty": "0.4", "unit_code": "bag", "base_qty_per_unit": "0.001" }),
    );
    row(
        "conversion with smallest factor",
        &|l| {
            l["input"] =
                json!({ "qty": "9999.999", "unit_code": "bag", "base_qty_per_unit": "0.000000001" })
        },
    );
    row(
        "conversion just below the midpoint",
        &|l| {
            l["input"] =
                json!({ "qty": "0.001", "unit_code": "bag", "base_qty_per_unit": "0.499999999" })
        },
    );
    row("unit_code empty", &|l| l["input"]["unit_code"] = json!(""));
    row("unit_code padded", &|l| {
        l["input"]["unit_code"] = json!(" bag")
    });
    row("input unknown field", &|l| {
        l["input"]["unit"] = json!("bag")
    });
    row("qty field present", &|l| l["qty"] = json!("50000"));
    row("lot_id present", &|l| l["lot_id"] = json!(UNKNOWN_ID));
    row("expires_at present", &|l| {
        l["expires_at"] = json!(END_OF_10_20)
    });
    row("line_cost_cents missing", &|l| {
        l.as_object_mut().unwrap().remove("line_cost_cents");
    });
    row("line_cost_cents negative", &|l| {
        l["line_cost_cents"] = json!(-1)
    });
    row("line_cost_cents above 99999999", &|l| {
        l["line_cost_cents"] = json!(100_000_000)
    });
    row("line_cost_cents string", &|l| {
        l["line_cost_cents"] = json!("1000")
    });
    row("line_cost_cents fraction", &|l| {
        l["line_cost_cents"] = json!(10.5)
    });
    row("line_cost_cents null", &|l| {
        l["line_cost_cents"] = Value::Null
    });
    row("manufacturer_lot_no empty", &|l| {
        l["manufacturer_lot_no"] = json!("")
    });
    row("manufacturer_lot_no padded", &|l| {
        l["manufacturer_lot_no"] = json!("L1 ")
    });
    row("manufacturer_lot_no null", &|l| {
        l["manufacturer_lot_no"] = Value::Null
    });
    row("manufacturer_lot_no 65 chars", &|l| {
        l["manufacturer_lot_no"] = json!(format!("批{}", "x".repeat(64)))
    });
    row("produced_on missing", &|l| {
        l.as_object_mut().unwrap().remove("produced_on");
    });
    row("expires_on missing", &|l| {
        l.as_object_mut().unwrap().remove("expires_on");
    });
    row("expires_on null", &|l| l["expires_on"] = Value::Null);
    row("produced_on not padded", &|l| {
        l["produced_on"] = json!("2026-10-1")
    });
    row("produced_on with time", &|l| {
        l["produced_on"] = json!("2026-10-01T00:00")
    });
    row("produced_on slashes", &|l| {
        l["produced_on"] = json!("2026/10/01")
    });
    row("expires_on not a date", &|l| {
        l["expires_on"] = json!("2026-02-30")
    });
    row("expires_on number", &|l| l["expires_on"] = json!(20261020));
    row("expires_on after 9998-12-31", &|l| {
        l["expires_on"] = json!("9999-01-01")
    });
    row("expires_on 9999-12-31", &|l| {
        l["expires_on"] = json!("9999-12-31")
    });
    row("produced after expires", &|l| {
        l["produced_on"] = json!("2026-10-21")
    });
    row("unknown line field", &|l| l["note"] = json!("x"));
    for text in non_canonical_uuids(&cmd(3)) {
        let mut request = valid.clone();
        request["command_id"] = json!(text);
        cases.push(("command_id non-canonical", request));
    }
    for (name, qty) in spec_support::invalid_input_qty_texts() {
        let mut request = valid.clone();
        request["lines"][0]["input"]["qty"] = qty;
        cases.push((name, request));
    }
    for (name, factor) in spec_support::invalid_factor_texts() {
        let mut request = valid.clone();
        request["lines"][0]["input"]["base_qty_per_unit"] = factor;
        cases.push((name, request));
    }

    for (name, request) in &cases {
        let reply = node.receive(&STAFF, request).await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "{name}"
        );
    }

    assert_eq!(node.state(), before);
}

// interfaces「calibrate」：sent_at − captured_at 溢出（正、负两个方向）时 400 VALIDATION_FAILED（details 为 {}）；
// 时间差恰好是 i64::MAX 时不溢出，先命中「超过 72 小时」，400 CAPTURE_TOO_OLD。都什么都不写。
#[tokio::test]
async fn out_of_range_times_are_validation_failed() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    let before = node.state();

    for (captured_at, sent_at) in [(i64::MIN, i64::MAX), (i64::MAX, i64::MIN)] {
        let reply = node
            .receive(
                &STAFF,
                &timed_body(
                    &cmd(3),
                    &supplier,
                    vec![line(&flour, "1", "g", "1")],
                    captured_at,
                    sent_at,
                ),
            )
            .await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "{captured_at} {sent_at}"
        );
    }
    let reply = node
        .receive(
            &STAFF,
            &timed_body(
                &cmd(3),
                &supplier,
                vec![line(&flour, "1", "g", "1")],
                i64::MIN + 1,
                0,
            ),
        )
        .await;
    assert_eq!(assert_error(&reply, 400, "CAPTURE_TOO_OLD"), &json!({}));

    assert_eq!(node.state(), before);
}

// ---------------------------------------------------------------------------------------------
// 幂等与处理顺序
// ---------------------------------------------------------------------------------------------

// 验收用例「幂等」、场景「同次收货重复提交」：一小时后用原 command_id、原内容重发（sent_at 不同，包括会超过 72 小时的值），
// 原样返回首次响应（含 warnings、原批次号 …-001、…-002），seq 不增加，批次与流水不变；不重新计算到期提醒，
// 也不重新分配流水号。
// 幂等检查先于业务校验：首次成功后换算系数变了，原内容重试仍返回首次响应，不是 409 UNIT_CONVERSION_CHANGED。
#[tokio::test]
async fn retries_return_the_original_response() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[("bag", "25000")]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    let lines = vec![
        expiring(line(&flour, "5", "g", "1"), "2026-10-20"),
        expiring(line(&flour, "1", "bag", "25000"), "2026-10-15"),
    ];
    let captured_at = SENT + MINUTE;
    let first = node
        .receive(
            &STAFF,
            &timed_body(&cmd(3), &supplier, lines.clone(), captured_at, SENT),
        )
        .await;
    let lots = lot_ids(receipt(&first));
    assert_eq!(lots, ["RAW-FLOUR-20261006-001", "RAW-FLOUR-20261006-002"]);
    assert_warnings(
        &first,
        &[
            ("CAPTURE_TIME_ADJUSTED", json!({})),
            (
                "EXPIRES_BEFORE_OLDER_STOCK",
                json!({ "line": 1, "item_id": flour, "lot_id": lots[1] }),
            ),
        ],
    );
    node.update_item(&cmd(4), &flour, 1, &[("bag", "20000")], true)
        .await;
    let before = node.state();
    node.clock.advance(Duration::from_secs(3600));

    for sent_at in [SENT, captured_at + 10 * MINUTE, captured_at + MAX_LAG + 1] {
        let reply = node
            .receive(
                &STAFF,
                &timed_body(&cmd(3), &supplier, lines.clone(), captured_at, sent_at),
            )
            .await;
        assert_eq!(reply.status, 200, "sent_at = {sent_at}");
        assert_eq!(reply.body, first.body, "sent_at = {sent_at}");
    }

    assert_eq!(node.state(), before);
}

// AGENTS「幂等」：同一 command_id、内容不同时 409 IDEMPOTENCY_CONFLICT，details.fields 为不同的顶层字段名（字典序）；
// 行内任何差异（含行序、省略可选字段）都列为 "lines"；command_type 不同时只列 "command_type"。
// 幂等检查先于业务校验：改成不存在的供应商、过期的日期、变化的系数、72 小时以外的 captured_at，仍是 409。
#[tokio::test]
async fn idempotency_conflicts_list_differing_fields() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[("bag", "25000")]).await;
    let sugar = node.item(&cmd(2), "SUGAR", &[]).await;
    let supplier = node.supplier(&cmd(3), "S1").await;
    let other = node.supplier(&cmd(4), "S2").await;
    let mut lot_no = line(&flour, "2", "bag", "25000");
    lot_no["manufacturer_lot_no"] = json!("L1");
    let lines = vec![lot_no.clone(), line(&sugar, "100", "g", "1")];
    let x = cmd(5);
    node.receive_ok(&STAFF, &body(&x, &supplier, lines.clone(), 10 * MINUTE))
        .await;
    let before = node.state();

    let changed_line = |f: &dyn Fn(&mut Value)| {
        let mut changed = lines.clone();
        f(&mut changed[0]);
        changed
    };
    let cases: Vec<(Value, Value)> = vec![
        (
            body(&x, &other, lines.clone(), 10 * MINUTE),
            json!(["supplier_id"]),
        ),
        (
            body(&x, UNKNOWN_ID, lines.clone(), 10 * MINUTE),
            json!(["supplier_id"]),
        ),
        (
            body(
                &x,
                &supplier,
                vec![lines[1].clone(), lines[0].clone()],
                10 * MINUTE,
            ),
            json!(["lines"]),
        ),
        (
            body(&x, &supplier, vec![lines[0].clone()], 10 * MINUTE),
            json!(["lines"]),
        ),
        (
            body(
                &x,
                &supplier,
                changed_line(&|l| {
                    l.as_object_mut().unwrap().remove("manufacturer_lot_no");
                }),
                10 * MINUTE,
            ),
            json!(["lines"]),
        ),
        (
            body(
                &x,
                &supplier,
                changed_line(&|l| l["expires_on"] = json!("2026-10-01")),
                10 * MINUTE,
            ),
            json!(["lines"]),
        ),
        (
            body(
                &x,
                &supplier,
                changed_line(&|l| l["input"]["base_qty_per_unit"] = json!("20000")),
                10 * MINUTE,
            ),
            json!(["lines"]),
        ),
        (
            body(
                &x,
                &supplier,
                changed_line(&|l| l["line_cost_cents"] = json!(999)),
                10 * MINUTE,
            ),
            json!(["lines"]),
        ),
        // captured_at 与 sent_at 同时前移：lag 不变，只有 captured_at 不同。
        (
            timed_body(
                &x,
                &supplier,
                lines.clone(),
                SENT - 11 * MINUTE,
                SENT - MINUTE,
            ),
            json!(["captured_at"]),
        ),
        (
            body(&x, &supplier, lines.clone(), MAX_LAG + 1),
            json!(["captured_at"]),
        ),
        (
            body(&x, &other, vec![lines[0].clone()], 0),
            json!(["captured_at", "lines", "supplier_id"]),
        ),
        // 物料新建命令的 command_id 用于收货。
        (
            body(&cmd(1), &supplier, lines.clone(), 10 * MINUTE),
            json!(["command_type"]),
        ),
    ];
    for (request, fields) in &cases {
        let reply = node.receive(&STAFF, request).await;
        assert_eq!(
            assert_error(&reply, 409, "IDEMPOTENCY_CONFLICT"),
            &json!({ "fields": fields }),
            "{request}"
        );
    }
    // 收货的 command_id 用于供应商新建。
    let reply = spec_support::post(
        &node.router,
        "/api/v1/suppliers",
        Some(&MANAGER),
        &json!({ "command_id": x, "code": "S9", "name": "S9", "active": true }),
    )
    .await
    .unwrap();
    assert_eq!(
        assert_error(&reply, 409, "IDEMPOTENCY_CONFLICT"),
        &json!({ "fields": ["command_type"] })
    );

    assert_eq!(node.state(), before);
}

// AGENTS「HTTP 约定」处理顺序：请求结构与取值校验先于幂等检查——已成功的 command_id 携带非法内容重发时是 400，不是 409。
// 超出录入上限（10000 g）、换算结果为 0、数量写成 JSON 数字、金额超出上限都属于取值（domain「数量」「单位」「收货接口」）。
#[tokio::test]
async fn validation_precedes_the_idempotency_check() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    let original = body(&cmd(3), &supplier, vec![line(&flour, "1", "g", "1")], 0);
    node.receive_ok(&STAFF, &original).await;
    let before = node.state();

    for (key, value) in [
        ("lines", json!([])),
        ("supplier_id", json!("not-a-uuid")),
        ("produced_on", json!("2026-10-21")),
        ("line_cost_cents", json!(-1)),
        ("line_cost_cents", json!(100_000_000)),
        ("expires_on", json!("9999-01-01")),
        (
            "input",
            json!({ "qty": "10000", "unit_code": "g", "base_qty_per_unit": "1" }),
        ),
        (
            "input",
            json!({ "qty": "0.4", "unit_code": "g", "base_qty_per_unit": "0.001" }),
        ),
        (
            "input",
            json!({ "qty": 1, "unit_code": "g", "base_qty_per_unit": "1" }),
        ),
    ] {
        let mut request = original.clone();
        if key == "lines" || key == "supplier_id" {
            request[key] = value;
        } else {
            request["lines"][0][key] = value;
        }
        let reply = node.receive(&STAFF, &request).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }

    assert_eq!(node.state(), before);
}

// auth「开发桩」与 AGENTS「HTTP 约定」：缺少或使用非法身份时 401 UNAUTHENTICATED；身份检查先于请求体——
// 请求体无法解析或取值非法时，无身份仍是 401。有身份时，请求体不是 JSON 对象、缺少 Content-Type 或不是
// application/json 都是 400 VALIDATION_FAILED。只有 POST，其他方法 405 METHOD_NOT_ALLOWED。什么都不写。
#[tokio::test]
async fn identity_is_required_and_checked_first() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    let valid = body(&cmd(3), &supplier, vec![line(&flour, "1", "g", "1")], 0);
    let before = node.state();

    let reply = send(
        &node.router,
        request(Method::POST, URI, None, Some(&valid)).unwrap(),
    )
    .await
    .unwrap();
    assert_error(&reply, 401, "UNAUTHENTICATED");
    let mut invalid = valid.clone();
    invalid["lines"] = json!([]);
    let reply = send(
        &node.router,
        request(Method::POST, URI, None, Some(&invalid)).unwrap(),
    )
    .await
    .unwrap();
    assert_error(&reply, 401, "UNAUTHENTICATED");
    let reply = send(
        &node.router,
        raw_request(Method::POST, URI, None, Some("application/json"), "{").unwrap(),
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
        let req = raw_request(Method::POST, URI, Some(&STAFF), content_type, text).unwrap();
        let reply = send(&node.router, req).await.unwrap();
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }
    for method in [Method::GET, Method::PUT, Method::DELETE] {
        let reply = send(
            &node.router,
            request(method.clone(), URI, Some(&STAFF), None).unwrap(),
        )
        .await
        .unwrap();
        assert_error(&reply, 405, "METHOD_NOT_ALLOWED");
    }

    assert_eq!(node.state(), before);
}

// ---------------------------------------------------------------------------------------------
// 定点数量（domain「数量」「单位」，验收用例「小数系数换算」「换算中点」「录入上限」「收货累计越界」「金额上限」「小数字符串」）
// ---------------------------------------------------------------------------------------------

// 验收用例「小数系数换算」与 domain「单位」单位换算：qty = input.qty × base_qty_per_unit，每行单独换算，
// 四舍五入到 0.001 基本单位，中点远离 0。物料基本单位 g，lb 系数 453.59237、oz 系数 28.349523125、half 系数 0.5。
// - 1.25 oz = 35.43690390625 g → "35.437"；16 oz = 453.59237 g → "453.592"；1 lb → "453.592"（16 oz 与 1 lb 结果相同）。
// - 0.001 half = 0.0005 → "0.001"；0.003 half = 0.0015 → "0.002"；0.005 half = 0.0025 → "0.003"（中点远离 0，不是银行家舍入）。
// - "5.500" g → qty "5.5"；payload、响应、规范化请求的 input 中只有规范形式（"5.5"、系数 "1"）。
// - 投影存千分之一克：35437、453592、453592、1、2、3、5500；账面 948.127 g 存 948127。
#[tokio::test]
async fn decimal_factors_convert_with_half_away_rounding() {
    let node = node();
    let flour = node
        .item(
            &cmd(1),
            "FLOUR",
            &[("half", "0.5"), ("lb", "453.59237"), ("oz", "28.349523125")],
        )
        .await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    let request = body(
        &cmd(3),
        &supplier,
        vec![
            line(&flour, "1.25", "oz", "28.349523125"),
            line(&flour, "16", "oz", "28.349523125"),
            line(&flour, "1", "lb", "453.59237"),
            line(&flour, "0.001", "half", "0.5"),
            line(&flour, "0.003", "half", "0.5"),
            line(&flour, "0.005", "half", "0.50"),
            line(&flour, "5.500", "g", "1.000"),
        ],
        0,
    );

    let receipt = node.receive_ok(&STAFF, &request).await;

    let converted: Vec<(Value, Value)> = receipt["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["qty"].clone(), l["input"].clone()))
        .collect();
    let input = |qty: &str, unit: &str, factor: &str| json!({ "qty": qty, "unit_code": unit, "base_qty_per_unit": factor });
    assert_eq!(
        converted,
        [
            (json!("35.437"), input("1.25", "oz", "28.349523125")),
            (json!("453.592"), input("16", "oz", "28.349523125")),
            (json!("453.592"), input("1", "lb", "453.59237")),
            (json!("0.001"), input("0.001", "half", "0.5")),
            (json!("0.002"), input("0.003", "half", "0.5")),
            (json!("0.003"), input("0.005", "half", "0.5")),
            (json!("5.5"), input("5.5", "g", "1")),
        ]
    );
    assert_eq!(
        node.receipts().unwrap()[0].payload["lines"],
        receipt["lines"]
    );
    let remaining: Vec<i64> = node.lots().unwrap().iter().map(|lot| lot.7).collect();
    assert_eq!(remaining, [35_437, 453_592, 453_592, 1, 2, 3, 5_500]);
    let deltas: Vec<(i64, i64)> = node
        .movements()
        .unwrap()
        .iter()
        .map(|m| (m.6, m.7))
        .collect();
    assert_eq!(
        deltas,
        [
            (35_437, 35_437),
            (453_592, 453_592),
            (453_592, 453_592),
            (1, 1),
            (2, 2),
            (3, 3),
            (5_500, 5_500)
        ]
    );
    assert_eq!(node.on_hand(&flour).unwrap(), 948_127);
    let (_, canonical, _) = node.processed(&cmd(3)).unwrap();
    assert_eq!(
        canonical["lines"][5]["input"],
        input("0.005", "half", "0.5")
    );
    assert_eq!(canonical["lines"][6]["input"], input("5.5", "g", "1"));
}

// 验收用例「小数字符串」与 AGENTS「幂等」：请求中小数部分可以带末尾的 0，规范化请求只用规范形式。
// "5.500" g 照常受理，payload 与响应中为 "5.5"；同一 command_id 再以 "5.5"、"5.50" 重发返回首次响应，seq 不增加；
// 改为 "5.6" 是 409 IDEMPOTENCY_CONFLICT（fields ["lines"]）；数量写成 JSON 数字 5.5 时 400 VALIDATION_FAILED（取值先于幂等）。
#[tokio::test]
async fn trailing_zeros_share_the_canonical_request() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[("kg", "1000")]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;
    let request = |qty: Value, factor: &str| {
        let mut l = line(&flour, "1", "kg", factor);
        l["input"]["qty"] = qty;
        body(&cmd(3), &supplier, vec![l], 0)
    };

    let first = node.receive(&STAFF, &request(json!("5.500"), "1000")).await;
    assert_eq!(receipt(&first)["lines"][0]["qty"], json!("5500"));
    assert_eq!(
        receipt(&first)["lines"][0]["input"],
        json!({ "qty": "5.5", "unit_code": "kg", "base_qty_per_unit": "1000" })
    );
    let before = node.state();

    for (qty, factor) in [
        (json!("5.5"), "1000"),
        (json!("5.50"), "1000.000"),
        (json!("5.500"), "1000.0"),
    ] {
        let reply = node.receive(&STAFF, &request(qty.clone(), factor)).await;
        assert_eq!(
            (reply.status.as_u16(), &reply.body),
            (200, &first.body),
            "{qty} {factor}"
        );
    }
    let reply = node.receive(&STAFF, &request(json!("5.6"), "1000")).await;
    assert_eq!(
        assert_error(&reply, 409, "IDEMPOTENCY_CONFLICT"),
        &json!({ "fields": ["lines"] })
    );
    for qty in [json!(5.5), json!(5)] {
        let reply = node.receive(&STAFF, &request(qty.clone(), "1000")).await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "{qty}"
        );
    }

    assert_eq!(node.state(), before);
}

// 验收用例「录入上限」与 domain「数量」录入上限：只检查录入时所选单位下的数字，不看换算后的值。
// 9999.999 kg 照常受理，qty "9999999"（批次余量存 9999999000）；10000 g、10000 kg 都是 400 VALIDATION_FAILED，不入账。
#[tokio::test]
async fn the_entry_limit_applies_to_the_selected_unit() {
    let node = node();
    let flour = node.item(&cmd(1), "FLOUR", &[("kg", "1000")]).await;
    let supplier = node.supplier(&cmd(2), "S1").await;

    let receipt = node
        .receive_ok(
            &STAFF,
            &body(
                &cmd(3),
                &supplier,
                vec![line(&flour, "9999.999", "kg", "1000")],
                0,
            ),
        )
        .await;
    assert_eq!(receipt["lines"][0]["qty"], json!("9999999"));
    assert_eq!(node.lots().unwrap()[0].7, 9_999_999_000);
    let before = node.state();

    for l in [
        line(&flour, "10000", "g", "1"),
        line(&flour, "10000", "kg", "1000"),
    ] {
        let reply = node
            .receive(&STAFF, &body(&cmd(4), &supplier, vec![l.clone()], 0))
            .await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "{l}"
        );
    }

    assert_eq!(node.state(), before);
}

// 验收用例「收货累计越界」与 domain「数量」库存状态上限、「收货接口」业务校验：L = 1000000000 g（存储值 10^12）。
// 每行处理之后（看到同一命令中前面各行新建的批次），该行物料的净账面、新批次余量都不超过 L；超出时整条命令不入账，
// 409 STOCK_LIMIT_EXCEEDED，details 为 {line, item_id}（收货只新建批次，不带 lot_id），line 是第一个越界的行。
// - 面粉先收 999.99 t = 999990000 g；一次收货两行各 5 kg：第二行之后账面恰好 1000000000，照常受理。
// - 再收 0.001 g：{line 0, item_id 面粉}。[糖 1 g, 面粉 0.001 g]：{line 1}，糖也不入账。
// - 多行越界时报第一个：[1.001 max, 糖 1 g, 1.001 max]：{line 0, item_id big}。
// - 单行本身超过 L：1.001 个 max（系数 10^9）= 1001000000 g；9999.999 个 max、9999.999 个 odd（系数 999999999.999999999）
//   的换算结果约 10^13 g，也是 409，不溢出、不当作取值错误。
// - 被拒绝的命令不落库，同一 command_id 改为恰好 L（1 个 max）后照常受理。
// - 幂等检查先于库存状态上限：面粉到达 L 之后，原内容重试两行 5 kg 的收货仍返回首次响应。
#[tokio::test]
async fn the_stock_limit_is_checked_after_each_line() {
    let node = node();
    let flour = node
        .item(&cmd(1), "FLOUR", &[("kg", "1000"), ("t", "1000000")])
        .await;
    let sugar = node.item(&cmd(2), "SUGAR", &[]).await;
    let big = node
        .item(
            &cmd(3),
            "BIG",
            &[("max", "1000000000"), ("odd", "999999999.999999999")],
        )
        .await;
    let supplier = node.supplier(&cmd(4), "S1").await;
    node.receive_ok(
        &STAFF,
        &body(
            &cmd(5),
            &supplier,
            vec![line(&flour, "999.99", "t", "1000000")],
            0,
        ),
    )
    .await;
    let at_limit = body(
        &cmd(6),
        &supplier,
        vec![
            line(&flour, "5", "kg", "1000"),
            line(&flour, "5", "kg", "1000"),
        ],
        0,
    );
    let first = node.receive(&STAFF, &at_limit).await;
    receipt(&first);
    assert_eq!(node.on_hand(&flour).unwrap(), 1_000_000_000_000);
    let before = node.state();

    let limit = |line: usize, item_id: &str| json!({ "line": line, "item_id": item_id });
    for (lines, details) in [
        (vec![line(&flour, "0.001", "g", "1")], limit(0, &flour)),
        (
            vec![line(&sugar, "1", "g", "1"), line(&flour, "0.001", "g", "1")],
            limit(1, &flour),
        ),
        (
            vec![line(&big, "1.001", "max", "1000000000")],
            limit(0, &big),
        ),
        (
            vec![line(&big, "9999.999", "max", "1000000000")],
            limit(0, &big),
        ),
        (
            vec![line(&big, "9999.999", "odd", "999999999.999999999")],
            limit(0, &big),
        ),
        (
            vec![
                line(&big, "0.5", "max", "1000000000"),
                line(&big, "0.501", "max", "1000000000"),
            ],
            limit(1, &big),
        ),
        (
            vec![
                line(&big, "1.001", "max", "1000000000"),
                line(&sugar, "1", "g", "1"),
                line(&big, "1.001", "max", "1000000000"),
            ],
            limit(0, &big),
        ),
    ] {
        let reply = node
            .receive(&STAFF, &body(&cmd(7), &supplier, lines.clone(), 0))
            .await;
        assert_eq!(
            assert_error(&reply, 409, "STOCK_LIMIT_EXCEEDED"),
            &details,
            "{lines:?}"
        );
        assert_eq!(node.state(), before);
    }

    node.clock.advance(Duration::from_secs(60));
    let reply = node.receive(&STAFF, &at_limit).await;
    assert_eq!((reply.status.as_u16(), &reply.body), (200, &first.body));
    assert_eq!(node.state(), before);

    let receipt = node
        .receive_ok(
            &STAFF,
            &body(
                &cmd(7),
                &supplier,
                vec![
                    line(&big, "0.5", "max", "1000000000"),
                    line(&big, "0.5", "max", "1000000000"),
                ],
                0,
            ),
        )
        .await;
    assert_eq!(receipt["lines"][1]["qty"], json!("500000000"));
    assert_eq!(node.on_hand(&big).unwrap(), 1_000_000_000_000);
}
