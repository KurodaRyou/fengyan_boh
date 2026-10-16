//! 锁定测试：节点校时信号对写命令的拦截、前端发生时间的校验、`GET /api/v1/clock`、`/health` 的校时字段。
//! 规则见 docs/domain.md「时间」「写命令通用规则」「节点时钟接口」与 AGENTS.md「HTTP 约定」；
//! 经 `test_router` 注入节点的上次校时（docs/interfaces.md「Router 测试入口」）。状态文件与 NTP 核对见 spec_clock_sync.rs。

mod spec_support;

use std::path::PathBuf;

use axum::Router;
use axum::http::{Method, header};
use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use boh_storage::rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use spec_support::{
    Actor, JsonReply, MANAGER, STAFF, assert_error, assert_success, raw_request, request,
};

const MS: i64 = 1;
const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;
/// 「太早」的界限：72 小时。
const TOO_OLD: i64 = 72 * HOUR;
const NOW: i64 = 1_791_248_400_000; // 2026-10-06 09:00 +08:00
/// 节点上次校时：NOW 前 1 天（10-05 09:00）。节点信号在 [SYNCED, SYNCED + 7 天] 内有效。
const SYNCED: i64 = NOW - DAY;
/// 前端上次校时：NOW 前 2 小时。
const CLIENT_SYNCED: i64 = NOW - 2 * HOUR;
const AT_10_06_0400: i64 = 1_791_230_400_000; // 2026-10-06 04:00 +08:00，营业日切点

fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b3020c10{n:04x}")
}

struct Fixture {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
    clock: ManualClock,
    router: Router,
}

impl Fixture {
    /// 时钟停在 NOW；节点上次校时为 `synced_at`。
    #[allow(clippy::unwrap_used)] // 测试夹具：建库失败时测试无法开始，直接终止。
    fn new(synced_at: Option<i64>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("boh.db");
        let clock = ManualClock::new(UnixMillis(NOW));
        let router =
            spec_support::router_synced(&db_path, clock.clock(), synced_at.map(UnixMillis))
                .unwrap();
        Self {
            _dir: dir,
            db_path,
            clock,
            router,
        }
    }

    fn set(&self, now: i64) {
        self.clock.set(UnixMillis(now));
    }

    #[allow(clippy::unwrap_used)] // 测试辅助：响应体不是 JSON 已违反信封约定，直接终止测试；状态码由调用方断言。
    async fn send(
        &self,
        method: Method,
        uri: &str,
        actor: Option<&Actor>,
        body: &Value,
    ) -> JsonReply {
        spec_support::send(
            &self.router,
            request(method, uri, actor, Some(body)).unwrap(),
        )
        .await
        .unwrap()
    }

    async fn post(&self, uri: &str, actor: &Actor, body: &Value) -> JsonReply {
        self.send(Method::POST, uri, Some(actor), body).await
    }

    #[allow(clippy::unwrap_used)] // 同上。
    async fn get(&self, uri: &str, actor: Option<&Actor>) -> JsonReply {
        spec_support::get(&self.router, uri, actor).await.unwrap()
    }

    async fn ok(&self, uri: &str, actor: &Actor, body: &Value) -> Value {
        let reply = self.post(uri, actor, body).await;
        assert_success(&reply).clone()
    }

    /// (store_events 行数, processed_commands 行数)。
    #[allow(clippy::unwrap_used)] // 测试辅助：只读查询失败说明夹具有误。
    fn counts(&self) -> (i64, i64) {
        let conn = spec_support::reader(&self.db_path).unwrap();
        (
            spec_support::count(&conn, "store_events").unwrap(),
            spec_support::count(&conn, "processed_commands").unwrap(),
        )
    }

    /// 命令写入的全部事件的 (occurred_at, recorded_at, business_date, clock_synced_at)，按 seq。
    #[allow(clippy::unwrap_used)] // 同上。
    fn event_times(&self, command_id: &str) -> Vec<(i64, i64, String, Option<i64>)> {
        let conn = spec_support::reader(&self.db_path).unwrap();
        let rows = spec_support::rows(
            &conn,
            &format!(
                "SELECT occurred_at, recorded_at, business_date, clock_synced_at FROM store_events
                 WHERE command_id = '{command_id}' ORDER BY seq"
            ),
        )
        .unwrap();
        rows.into_iter()
            .map(|row| match row.as_slice() {
                [
                    SqlValue::Integer(occurred),
                    SqlValue::Integer(recorded),
                    SqlValue::Text(date),
                    synced,
                ] => (
                    *occurred,
                    *recorded,
                    date.clone(),
                    match synced {
                        SqlValue::Integer(v) => Some(*v),
                        SqlValue::Null => None,
                        other => panic!("clock_synced_at: {other:?}"),
                    },
                ),
                other => panic!("unexpected row {other:?}"),
            })
            .collect()
    }

    /// processed_commands 中保存的规范化请求。
    #[allow(clippy::unwrap_used)] // 同上。
    fn canonical_request(&self, command_id: &str) -> Value {
        let conn = spec_support::reader(&self.db_path).unwrap();
        let text: String = conn
            .query_row(
                "SELECT request FROM processed_commands WHERE command_id = ?1",
                [command_id],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }
}

/// 业务数据：设备 F1；物料 FLOUR（RAW，单位 g）、TOAST（FINISHED）；供应商 S1；报损原因 EXPIRED；配方 R-TOAST；
/// 收货 FLOUR 1000 g（一个批次）。全部在 NOW 写入（cmd 1～8）。
struct Refs {
    equipment: String,
    flour: String,
    toast: String,
    supplier: String,
    reason: String,
    recipe: String,
    lot: String,
}

#[allow(clippy::unwrap_used)] // 测试夹具：前置写入失败时后续断言没有意义，直接终止测试。
async fn seed(f: &Fixture) -> Refs {
    let equipment = f
        .ok(
            "/api/v1/equipment",
            &MANAGER,
            &json!({
                "command_id": cmd(1), "code": "F1", "name": "Walk-in",
                "equipment_type": "FRIDGE", "active": true,
            }),
        )
        .await["equipment"]["equipment_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let item = |n: u16, code: &str, category: &str| {
        json!({
            "command_id": cmd(n), "code": code, "name": code, "base_unit": "g",
            "category": category, "units": [], "active": true,
        })
    };
    let flour = f
        .ok("/api/v1/items", &MANAGER, &item(2, "FLOUR", "RAW"))
        .await["item"]["item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let toast = f
        .ok("/api/v1/items", &MANAGER, &item(3, "TOAST", "FINISHED"))
        .await["item"]["item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let supplier = f
        .ok(
            "/api/v1/suppliers",
            &MANAGER,
            &json!({ "command_id": cmd(4), "code": "S1", "name": "S1", "active": true }),
        )
        .await["supplier"]["supplier_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let reason = f
        .ok(
            "/api/v1/waste-reasons",
            &MANAGER,
            &json!({ "command_id": cmd(5), "code": "EXPIRED", "name": "过期", "active": true }),
        )
        .await["waste_reason"]["waste_reason_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let recipe = f
        .ok(
            "/api/v1/recipes",
            &MANAGER,
            &json!({
                "command_id": cmd(6), "code": "R-TOAST", "name": "吐司",
                "output_item_id": toast, "active": true,
                "output_input": { "qty": "12", "unit_code": "g", "base_qty_per_unit": "1" },
                "lines": [{
                    "item_id": flour,
                    "input": { "qty": "3000", "unit_code": "g", "base_qty_per_unit": "1" },
                }],
            }),
        )
        .await["recipe"]["recipe_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let receipt = f
        .ok(
            "/api/v1/receipts",
            &STAFF,
            &receipt_body(7, &supplier, &flour, "1000", NOW, CLIENT_SYNCED),
        )
        .await;
    let lot = receipt["receipt"]["lines"][0]["lot_id"]
        .as_str()
        .unwrap()
        .to_owned();
    Refs {
        equipment,
        flour,
        toast,
        supplier,
        reason,
        recipe,
        lot,
    }
}

fn temperature_body(n: u16, equipment: &str, occurred_at: i64, clock_synced_at: i64) -> Value {
    json!({
        "command_id": cmd(n), "equipment_id": equipment, "celsius_x10": 38,
        "occurred_at": occurred_at, "clock_synced_at": clock_synced_at,
    })
}

fn receipt_body(
    n: u16,
    supplier: &str,
    item: &str,
    qty: &str,
    occurred_at: i64,
    clock_synced_at: i64,
) -> Value {
    json!({
        "command_id": cmd(n), "supplier_id": supplier,
        "lines": [{
            "item_id": item,
            "input": { "qty": qty, "unit_code": "g", "base_qty_per_unit": "1" },
            "produced_on": "2026-10-01", "expires_on": "2026-10-20", "line_cost_cents": 100,
        }],
        "occurred_at": occurred_at, "clock_synced_at": clock_synced_at,
    })
}

fn waste_body(n: u16, item: &str, qty: &str, occurred_at: i64, clock_synced_at: i64) -> Value {
    json!({
        "command_id": cmd(n),
        "lines": [{
            "item_id": item,
            "input": { "qty": qty, "unit_code": "g", "base_qty_per_unit": "1" },
            "reason_code": "EXPIRED",
        }],
        "occurred_at": occurred_at, "clock_synced_at": clock_synced_at,
    })
}

/// 实时录入的三种写命令（温度记录、收货、报损）：路径与按 (command_id, occurred_at, clock_synced_at) 构造的请求体。
#[derive(Clone, Copy)]
enum Live {
    Temperature,
    Receipt,
    Waste,
}

const LIVE: [Live; 3] = [Live::Temperature, Live::Receipt, Live::Waste];

impl Live {
    fn uri(self) -> &'static str {
        match self {
            Live::Temperature => "/api/v1/temperature-readings",
            Live::Receipt => "/api/v1/receipts",
            Live::Waste => "/api/v1/waste-records",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Live::Temperature => "temperature",
            Live::Receipt => "receipt",
            Live::Waste => "waste",
        }
    }

    fn body(self, refs: &Refs, n: u16, occurred_at: i64, clock_synced_at: i64) -> Value {
        match self {
            Live::Temperature => temperature_body(n, &refs.equipment, occurred_at, clock_synced_at),
            Live::Receipt => receipt_body(
                n,
                &refs.supplier,
                &refs.flour,
                "10",
                occurred_at,
                clock_synced_at,
            ),
            Live::Waste => waste_body(n, &refs.flour, "1", occurred_at, clock_synced_at),
        }
    }
}

/// 14 个写命令（主数据、温度记录、收货、报损），各用新的 command_id（从 `first` 起编号）。
fn write_commands(refs: &Refs, first: u16) -> Vec<(Method, String, &'static Actor, Value)> {
    let n = |i: u16| cmd(first + i);
    let update = |i: u16, fields: Value| {
        let mut body = json!({ "command_id": n(i), "base_revision": 1 });
        for (key, value) in fields.as_object().into_iter().flatten() {
            body[key] = value.clone();
        }
        body
    };
    vec![
        (
            Method::POST,
            "/api/v1/equipment".into(),
            &MANAGER,
            json!({
                "command_id": n(0), "code": "F2", "name": "Freezer",
                "equipment_type": "FREEZER", "active": true,
            }),
        ),
        (
            Method::PUT,
            format!("/api/v1/equipment/{}", refs.equipment),
            &MANAGER,
            update(
                1,
                json!({ "name": "Walk-in 2", "equipment_type": "FRIDGE", "active": true }),
            ),
        ),
        (
            Method::POST,
            "/api/v1/items".into(),
            &MANAGER,
            json!({
                "command_id": n(2), "code": "SUGAR", "name": "SUGAR", "base_unit": "g",
                "category": "RAW", "units": [], "active": true,
            }),
        ),
        (
            Method::PUT,
            format!("/api/v1/items/{}", refs.flour),
            &MANAGER,
            update(
                3,
                json!({ "name": "FLOUR T65", "category": "RAW", "units": [], "active": true }),
            ),
        ),
        (
            Method::POST,
            "/api/v1/recipes".into(),
            &MANAGER,
            json!({
                "command_id": n(4), "code": "R-TOAST-2", "name": "吐司 2",
                "output_item_id": refs.toast, "active": true,
                "output_input": { "qty": "10", "unit_code": "g", "base_qty_per_unit": "1" },
                "lines": [{
                    "item_id": refs.flour,
                    "input": { "qty": "2000", "unit_code": "g", "base_qty_per_unit": "1" },
                }],
            }),
        ),
        (
            Method::PUT,
            format!("/api/v1/recipes/{}", refs.recipe),
            &MANAGER,
            update(5, json!({ "name": "白吐司", "active": true })),
        ),
        (
            Method::POST,
            format!("/api/v1/recipes/{}/versions", refs.recipe),
            &MANAGER,
            json!({
                "command_id": n(6), "base_revision": 2,
                "output_input": { "qty": "10", "unit_code": "g", "base_qty_per_unit": "1" },
                "lines": [{
                    "item_id": refs.flour,
                    "input": { "qty": "2500", "unit_code": "g", "base_qty_per_unit": "1" },
                }],
            }),
        ),
        (
            Method::POST,
            "/api/v1/suppliers".into(),
            &MANAGER,
            json!({ "command_id": n(7), "code": "S2", "name": "S2", "active": true }),
        ),
        (
            Method::PUT,
            format!("/api/v1/suppliers/{}", refs.supplier),
            &MANAGER,
            update(8, json!({ "name": "S1 B", "active": true })),
        ),
        (
            Method::POST,
            "/api/v1/waste-reasons".into(),
            &MANAGER,
            json!({ "command_id": n(9), "code": "DAMAGED", "name": "损坏", "active": true }),
        ),
        (
            Method::PUT,
            format!("/api/v1/waste-reasons/{}", refs.reason),
            &MANAGER,
            update(10, json!({ "name": "超过保质期", "active": true })),
        ),
        (
            Method::POST,
            Live::Temperature.uri().into(),
            &STAFF,
            Live::Temperature.body(refs, first + 11, NOW, CLIENT_SYNCED),
        ),
        (
            Method::POST,
            Live::Receipt.uri().into(),
            &STAFF,
            Live::Receipt.body(refs, first + 12, NOW, CLIENT_SYNCED),
        ),
        (
            Method::POST,
            Live::Waste.uri().into(),
            &STAFF,
            Live::Waste.body(refs, first + 13, NOW, CLIENT_SYNCED),
        ),
    ]
}

fn assert_no_store(reply: &JsonReply) {
    assert_eq!(
        reply
            .headers
            .get(header::CACHE_CONTROL)
            .map(|v| v.to_str().unwrap_or_default()),
        Some("no-store"),
        "{}",
        reply.body
    );
}

// ---------------------------------------------------------------------------
// GET /api/v1/clock
// ---------------------------------------------------------------------------

// domain「节点时钟接口」：不需要身份；信号有效时 data 为 {"now": 节点时钟的当前时间}，warnings 为空，
// 带 Cache-Control: no-store。信号边界两端（now = 上次校时、now = 上次校时 + 7 天）都有效。带开发桩身份也照常返回。
#[tokio::test]
async fn clock_endpoint_returns_node_time_without_identity() {
    let f = Fixture::new(Some(SYNCED));
    for now in [NOW, SYNCED, SYNCED + WEEK] {
        f.set(now);
        let reply = f.get("/api/v1/clock", None).await;
        assert_eq!(assert_success(&reply), &json!({ "now": now }), "{now}");
        assert_eq!(reply.body["warnings"], json!([]));
        assert_no_store(&reply);
    }
    f.set(NOW);
    let reply = f.get("/api/v1/clock", Some(&STAFF)).await;
    assert_eq!(assert_success(&reply), &json!({ "now": NOW }));
    assert_no_store(&reply);
}

// domain「节点时钟接口」：信号无效时 503 CLOCK_NOT_SYNCED，details 为 {}，同样带 Cache-Control: no-store。
// 无效的三种情况：没有上次校时；现在早于上次校时 1 ms（时钟复位到过去）；超过 7 天 1 ms。
#[tokio::test]
async fn clock_endpoint_is_unavailable_without_a_valid_node_signal() {
    let f = Fixture::new(Some(SYNCED));
    for now in [SYNCED - MS, SYNCED + WEEK + MS] {
        f.set(now);
        let reply = f.get("/api/v1/clock", None).await;
        assert_eq!(
            assert_error(&reply, 503, "CLOCK_NOT_SYNCED"),
            &json!({}),
            "{now}"
        );
        assert_no_store(&reply);
    }
    let f = Fixture::new(None);
    let reply = f.get("/api/v1/clock", None).await;
    assert_eq!(assert_error(&reply, 503, "CLOCK_NOT_SYNCED"), &json!({}));
    assert_no_store(&reply);
}

// AGENTS「HTTP 约定」：/api/v1/clock 只有 GET，其他方法 405 METHOD_NOT_ALLOWED（信封）。
#[tokio::test]
async fn clock_endpoint_only_accepts_get() {
    let f = Fixture::new(Some(SYNCED));
    for method in [Method::POST, Method::PUT, Method::DELETE] {
        let reply = f
            .send(method.clone(), "/api/v1/clock", None, &json!({}))
            .await;
        assert_error(&reply, 405, "METHOD_NOT_ALLOWED");
    }
}

// ---------------------------------------------------------------------------
// /health
// ---------------------------------------------------------------------------

// AGENTS「HTTP 约定」/health：clock_synced 是节点信号是否有效，clock_synced_at 是上次校时（没有时 null）；
// clock_synced 为 false 时 status 为 degraded。过期或现在早于上次校时，上次校时照常报告。
#[tokio::test]
async fn health_reports_the_node_clock_signal() {
    let f = Fixture::new(Some(SYNCED));
    for (now, synced, status) in [
        (NOW, true, "ok"),
        (SYNCED, true, "ok"),
        (SYNCED + WEEK, true, "ok"),
        (SYNCED + WEEK + MS, false, "degraded"),
        (SYNCED - MS, false, "degraded"),
    ] {
        f.set(now);
        let data = spec_support::health(&f.router).await;
        assert_eq!(data["clock_synced"], json!(synced), "{now}");
        assert_eq!(data["clock_synced_at"], json!(SYNCED), "{now}");
        assert_eq!(data["status"], json!(status), "{now}");
    }
    let f = Fixture::new(None);
    let data = spec_support::health(&f.router).await;
    assert_eq!(data["clock_synced"], json!(false));
    assert_eq!(data["clock_synced_at"], Value::Null);
    assert_eq!(data["status"], json!("degraded"));
}

// ---------------------------------------------------------------------------
// 节点信号无效时拒绝写入
// ---------------------------------------------------------------------------

// domain「时间」门店节点校时：节点信号无效时，业务写命令和主数据命令全部 503 CLOCK_NOT_SYNCED，details 为 {}，
// 不写事件或 processed_commands。三种无效情况逐一检查。之后时钟回到有效期内（校时后自动恢复，不需要人工确认），
// 用同一个 command_id、同一内容重提，全部 200。
#[tokio::test]
async fn every_write_command_is_rejected_while_the_node_signal_is_invalid() {
    let f = Fixture::new(Some(SYNCED));
    let refs = seed(&f).await;
    let before = f.counts();
    let commands = write_commands(&refs, 100);
    for now in [SYNCED - MS, SYNCED + WEEK + MS] {
        f.set(now);
        for (method, uri, actor, body) in &commands {
            let reply = f.send(method.clone(), uri, Some(actor), body).await;
            assert_eq!(
                assert_error(&reply, 503, "CLOCK_NOT_SYNCED"),
                &json!({}),
                "{method} {uri} at {now}"
            );
        }
        assert_eq!(f.counts(), before, "at {now}");
    }

    f.set(NOW);
    for (method, uri, actor, body) in &commands {
        let reply = f.send(method.clone(), uri, Some(actor), body).await;
        assert_success(&reply);
    }
    assert_eq!(f.counts(), (before.0 + 14, before.1 + 14));
}

// domain「时间」门店节点校时：从未校时（test_router 注入 None）的节点同样拒绝写入；查询照常。
#[tokio::test]
async fn a_node_that_never_synced_rejects_writes() {
    let f = Fixture::new(None);
    let reply = f
        .post(
            "/api/v1/equipment",
            &MANAGER,
            &json!({
                "command_id": cmd(1), "code": "F1", "name": "Walk-in",
                "equipment_type": "FRIDGE", "active": true,
            }),
        )
        .await;
    assert_eq!(assert_error(&reply, 503, "CLOCK_NOT_SYNCED"), &json!({}));
    assert_eq!(f.counts(), (0, 0));
    let reply = f.get("/api/v1/equipment", Some(&MANAGER)).await;
    assert_eq!(assert_success(&reply)["equipment"], json!([]));
}

// AGENTS「HTTP 约定」写命令处理顺序：身份与权限 → 请求结构与取值 → 幂等检查 → 节点校时信号 → 业务校验。
// 节点信号无效时：缺少身份 401、角色不够 403；请求体不合法（非法 JSON、缺字段、相对校准时代的旧字段）400 VALIDATION_FAILED；
// 业务上会被拒绝的命令（引用不存在、前端信号无效、发生时间太早、报损需要确认）一律 503，不先做业务校验。
#[tokio::test]
async fn node_signal_is_checked_after_request_shape_and_before_business_rules() {
    let f = Fixture::new(Some(SYNCED));
    let refs = seed(&f).await;
    let before = f.counts();
    f.set(SYNCED + WEEK + MS);

    let equipment = json!({
        "command_id": cmd(20), "code": "F9", "name": "X", "equipment_type": "FRIDGE", "active": true,
    });
    let reply = f
        .send(Method::POST, "/api/v1/equipment", None, &equipment)
        .await;
    assert_error(&reply, 401, "UNAUTHENTICATED");
    let reply = f.post("/api/v1/equipment", &STAFF, &equipment).await;
    assert_error(&reply, 403, "FORBIDDEN");

    let reply = spec_support::send(
        &f.router,
        raw_request(
            Method::POST,
            Live::Temperature.uri(),
            Some(&STAFF),
            Some("application/json"),
            "{not json",
        )
        .unwrap(),
    )
    .await
    .unwrap();
    assert_error(&reply, 400, "VALIDATION_FAILED");
    for live in LIVE {
        let good = live.body(&refs, 21, NOW, CLIENT_SYNCED);
        let mut missing = good.clone();
        missing.as_object_mut().unwrap().remove("clock_synced_at");
        let mut legacy = good.clone();
        legacy["captured_at"] = json!(NOW);
        legacy["sent_at"] = json!(NOW);
        for body in [missing, legacy] {
            let reply = f.post(live.uri(), &STAFF, &body).await;
            assert_error(&reply, 400, "VALIDATION_FAILED");
        }
    }

    let unknown_equipment = temperature_body(22, &cmd(999), NOW, CLIENT_SYNCED);
    let expired_client = temperature_body(23, &refs.equipment, NOW, NOW - WEEK - MS);
    let too_old = temperature_body(24, &refs.equipment, NOW - TOO_OLD - MS, CLIENT_SYNCED);
    let mut shortage = waste_body(25, &refs.flour, "5000", NOW, CLIENT_SYNCED);
    shortage["lines"][0]["lot_id"] = json!(refs.lot);
    for (uri, body) in [
        (Live::Temperature.uri(), unknown_equipment),
        (Live::Temperature.uri(), expired_client),
        (Live::Temperature.uri(), too_old),
        (Live::Waste.uri(), shortage),
    ] {
        let reply = f.post(uri, &STAFF, &body).await;
        assert_eq!(
            assert_error(&reply, 503, "CLOCK_NOT_SYNCED"),
            &json!({}),
            "{body}"
        );
    }
    assert_eq!(f.counts(), before);
}

// domain「时间」门店节点校时：节点信号检查在幂等检查之后。信号有效时成功的命令，信号失效后用原 command_id、原内容
// 重试，返回首次响应（含 warnings），不新增事件；clock_synced_at 不同（不进入规范化请求）也一样。
// 内容不同时 409 IDEMPOTENCY_CONFLICT，details 列出不同的字段，而不是 503。用同一 command_id 提交另一种命令（另两种实时命令、
// 新建设备，请求结构合法）时同样是 409，details 只列 "command_type"（AGENTS「HTTP 约定」）。
#[tokio::test]
async fn idempotent_retries_are_answered_while_the_node_signal_is_invalid() {
    let f = Fixture::new(Some(SYNCED));
    let refs = seed(&f).await;
    let mut firsts = Vec::new();
    for (i, live) in LIVE.into_iter().enumerate() {
        let n = 30 + u16::try_from(i).unwrap_or_default();
        let body = live.body(&refs, n, NOW - 5 * MINUTE, CLIENT_SYNCED);
        let first = f.post(live.uri(), &STAFF, &body).await;
        assert_success(&first);
        firsts.push((live, body, first.body));
    }
    let after = f.counts();

    for now in [SYNCED - MS, SYNCED + WEEK + MS] {
        f.set(now);
        for (live, body, first) in &firsts {
            let reply = f.post(live.uri(), &STAFF, body).await;
            assert_eq!(reply.status, 200, "{} at {now}", live.name());
            assert_eq!(&reply.body, first, "{} at {now}", live.name());

            let mut resynced = body.clone();
            resynced["clock_synced_at"] = json!(now - 30 * MINUTE);
            let reply = f.post(live.uri(), &STAFF, &resynced).await;
            assert_eq!(&reply.body, first, "{} resynced at {now}", live.name());

            let mut changed = body.clone();
            changed["occurred_at"] = json!(NOW - 4 * MINUTE);
            let reply = f.post(live.uri(), &STAFF, &changed).await;
            assert_eq!(
                assert_error(&reply, 409, "IDEMPOTENCY_CONFLICT"),
                &json!({ "fields": ["occurred_at"] }),
                "{} at {now}",
                live.name()
            );

            let mut others: Vec<(&str, &Actor, Value)> = LIVE
                .into_iter()
                .filter(|other| other.name() != live.name())
                .map(|other| {
                    (
                        other.uri(),
                        &STAFF,
                        other.body(&refs, 0, NOW - 5 * MINUTE, CLIENT_SYNCED),
                    )
                })
                .collect();
            others.push((
                "/api/v1/equipment",
                &MANAGER,
                json!({
                    "code": "F8", "name": "Freezer", "equipment_type": "FREEZER", "active": true,
                }),
            ));
            for (uri, actor, mut other) in others {
                other["command_id"] = body["command_id"].clone();
                let reply = f.post(uri, actor, &other).await;
                assert_eq!(
                    assert_error(&reply, 409, "IDEMPOTENCY_CONFLICT"),
                    &json!({ "fields": ["command_type"] }),
                    "{} as {uri} at {now}",
                    live.name()
                );
            }
        }
    }
    assert_eq!(f.counts(), after);
}

// domain「时间」门店节点校时：「其他查询、报损预检和 /health 照常」。
#[tokio::test]
async fn queries_and_waste_precheck_work_while_the_node_signal_is_invalid() {
    let f = Fixture::new(Some(SYNCED));
    let refs = seed(&f).await;
    f.set(SYNCED + WEEK + MS);
    for uri in [
        "/api/v1/equipment".to_owned(),
        "/api/v1/items".to_owned(),
        "/api/v1/recipes".to_owned(),
        "/api/v1/suppliers".to_owned(),
        "/api/v1/waste-reasons".to_owned(),
        "/api/v1/inventory".to_owned(),
        format!("/api/v1/lots/{}", refs.lot),
        "/api/v1/temperature-readings?business_date=2026-10-06".to_owned(),
    ] {
        let reply = f.get(&uri, Some(&STAFF)).await;
        assert_eq!(reply.status, 200, "{uri}: {}", reply.body);
        assert_success(&reply);
    }
    let reply = f
        .post(
            "/api/v1/waste-records/precheck",
            &STAFF,
            &json!({ "lines": [{
                "item_id": refs.flour,
                "input": { "qty": "1", "unit_code": "g", "base_qty_per_unit": "1" },
                "reason_code": "EXPIRED",
            }] }),
        )
        .await;
    assert_success(&reply);
    let data = spec_support::health(&f.router).await;
    assert_eq!(data["clock_synced"], json!(false));
}

// ---------------------------------------------------------------------------
// 发生时间的校验（实时录入：温度记录、收货、报损）
// ---------------------------------------------------------------------------

// domain「时间」发生时间的校验：边界值照常受理，occurred_at 原样记录（不按 recorded_at 推算），
// clock_synced_at 原样写进事件的 store_events.clock_synced_at，recorded_at 是节点时钟。
// - clock_synced_at = recorded_at + 1 分钟（前端校时比节点快，允许）；
// - clock_synced_at = recorded_at − 7 天；
// - occurred_at = recorded_at + 1 分钟（领先不超过 1 分钟，照常受理）；
// - occurred_at = recorded_at − 72 小时。
#[tokio::test]
async fn boundary_capture_times_are_accepted_and_recorded_as_submitted() {
    let f = Fixture::new(Some(SYNCED));
    let refs = seed(&f).await;
    let mut n = 40;
    for live in LIVE {
        for (occurred_at, clock_synced_at, date) in [
            (NOW, NOW + MINUTE, "2026-10-06"),
            (NOW, NOW - WEEK, "2026-10-06"),
            (NOW + MINUTE, CLIENT_SYNCED, "2026-10-06"),
            (NOW - TOO_OLD, CLIENT_SYNCED, "2026-10-03"),
        ] {
            n += 1;
            let body = live.body(&refs, n, occurred_at, clock_synced_at);
            let reply = f.post(live.uri(), &STAFF, &body).await;
            assert_success(&reply);
            assert_eq!(
                f.event_times(&cmd(n)),
                [(occurred_at, NOW, date.to_owned(), Some(clock_synced_at))],
                "{} {occurred_at} {clock_synced_at}",
                live.name()
            );
        }
    }
}

// domain「时间」营业日：business_date 由提交的 occurred_at 计算，不由 recorded_at 计算。
// 节点 04:00:00 收到 03:59:59.999 录入的记录，归前一营业日 10-05；节点 03:59:30 收到 04:00:00 录入的记录（领先 30 秒），
// 归 10-06。
#[tokio::test]
async fn business_date_follows_the_submitted_occurred_at() {
    let f = Fixture::new(Some(SYNCED));
    let refs = seed(&f).await;
    let mut n = 60;
    for live in LIVE {
        for (recorded_at, occurred_at, date) in [
            (AT_10_06_0400, AT_10_06_0400 - MS, "2026-10-05"),
            (AT_10_06_0400 - 30_000, AT_10_06_0400, "2026-10-06"),
        ] {
            n += 1;
            f.set(recorded_at);
            let body = live.body(&refs, n, occurred_at, CLIENT_SYNCED - 6 * HOUR);
            assert_success(&f.post(live.uri(), &STAFF, &body).await);
            assert_eq!(
                f.event_times(&cmd(n)),
                [(
                    occurred_at,
                    recorded_at,
                    date.to_owned(),
                    Some(CLIENT_SYNCED - 6 * HOUR)
                )],
                "{}",
                live.name()
            );
        }
    }
}

// domain「时间」发生时间的校验：超出边界 1 ms 即拒绝，details 为 {}，不写事件或 processed_commands：
// - 前端信号无效（clock_synced_at 领先 1 分钟零 1 ms，或 7 天零 1 ms 之前）→ 400 CLIENT_CLOCK_NOT_SYNCED；
// - occurred_at 晚于 recorded_at 1 分钟零 1 ms → 400 CLIENT_CLOCK_NOT_SYNCED；
// - occurred_at 早于 recorded_at 72 小时零 1 ms → 400 CAPTURE_TOO_OLD；
// - 极端值（i64 的最小、最大值）按差值的真实大小判定，不溢出成 500 或 VALIDATION_FAILED。
// 被拒的命令不落库：App 按「前端错误处理」修改后用同一个 command_id 重提，照常成功。
#[tokio::test]
async fn capture_times_outside_the_limits_are_rejected_and_not_recorded() {
    let f = Fixture::new(Some(SYNCED));
    let refs = seed(&f).await;
    let cases = [
        (NOW, NOW + MINUTE + MS, "CLIENT_CLOCK_NOT_SYNCED"),
        (NOW, NOW - WEEK - MS, "CLIENT_CLOCK_NOT_SYNCED"),
        (NOW + MINUTE + MS, CLIENT_SYNCED, "CLIENT_CLOCK_NOT_SYNCED"),
        (NOW - TOO_OLD - MS, CLIENT_SYNCED, "CAPTURE_TOO_OLD"),
        (i64::MIN, CLIENT_SYNCED, "CAPTURE_TOO_OLD"),
        (i64::MAX, CLIENT_SYNCED, "CLIENT_CLOCK_NOT_SYNCED"),
        (NOW, i64::MIN, "CLIENT_CLOCK_NOT_SYNCED"),
        (NOW, i64::MAX, "CLIENT_CLOCK_NOT_SYNCED"),
    ];
    let mut n = 80;
    for live in LIVE {
        for (occurred_at, clock_synced_at, code) in cases {
            n += 1;
            let before = f.counts();
            let reply = f
                .post(
                    live.uri(),
                    &STAFF,
                    &live.body(&refs, n, occurred_at, clock_synced_at),
                )
                .await;
            assert_eq!(
                assert_error(&reply, 400, code),
                &json!({}),
                "{} {occurred_at} {clock_synced_at}",
                live.name()
            );
            assert_eq!(f.counts(), before);
        }
        // 最后一个被拒的 command_id：重新校时后按新的偏差重算 occurred_at，更新 clock_synced_at，再重提。
        let reply = f
            .post(
                live.uri(),
                &STAFF,
                &live.body(&refs, n, NOW - MINUTE, NOW - MINUTE),
            )
            .await;
        assert_success(&reply);
        assert_eq!(
            f.event_times(&cmd(n)),
            [(
                NOW - MINUTE,
                NOW,
                "2026-10-06".to_owned(),
                Some(NOW - MINUTE)
            )]
        );
    }
}

// AGENTS「幂等」、domain「时间」前端校时：规范化请求剥离 command_id 和 clock_synced_at，保留 occurred_at。
// 验收用例「幂等」：同一命令发送 2 次、clock_synced_at 不同，第二次原样返回首次响应，seq 不增加；
// 事件中仍是首次的 clock_synced_at。重试即使带着已过期的 clock_synced_at、或节点时间已超过 occurred_at 72 小时，
// 也返回首次响应，不重新校验。
#[tokio::test]
async fn clock_synced_at_is_not_part_of_the_canonical_request() {
    let f = Fixture::new(Some(SYNCED));
    let refs = seed(&f).await;
    let mut n = 110;
    for live in LIVE {
        n += 1;
        f.set(NOW);
        let body = live.body(&refs, n, NOW - 10 * MINUTE, CLIENT_SYNCED);
        let first = f.post(live.uri(), &STAFF, &body).await;
        assert_success(&first);
        let canonical = f.canonical_request(&cmd(n));
        assert!(canonical.get("clock_synced_at").is_none(), "{canonical}");
        assert!(canonical.get("command_id").is_none(), "{canonical}");
        assert_eq!(canonical["occurred_at"], json!(NOW - 10 * MINUTE));
        let after = f.counts();

        for (now, clock_synced_at) in [
            (NOW + HOUR, NOW + 50 * MINUTE),
            (NOW + HOUR, NOW - WEEK - DAY),
            (NOW + TOO_OLD, NOW + TOO_OLD - HOUR),
        ] {
            f.set(now);
            let mut retry = body.clone();
            retry["clock_synced_at"] = json!(clock_synced_at);
            let reply = f.post(live.uri(), &STAFF, &retry).await;
            assert_eq!(reply.status, 200, "{} {now}", live.name());
            assert_eq!(reply.body, first.body, "{} {now}", live.name());
        }
        assert_eq!(f.counts(), after);
        assert_eq!(
            f.event_times(&cmd(n)),
            [(
                NOW - 10 * MINUTE,
                NOW,
                "2026-10-06".to_owned(),
                Some(CLIENT_SYNCED)
            )]
        );
    }
}

// domain「时间」：主数据命令不带 occurred_at / clock_synced_at（occurred_at = recorded_at，store_events.clock_synced_at
// 为 NULL）；带这两个字段之一时是未知字段，400 VALIDATION_FAILED。
#[tokio::test]
async fn master_data_events_have_no_client_clock() {
    let f = Fixture::new(Some(SYNCED));
    let refs = seed(&f).await;
    for n in 1..=6 {
        assert_eq!(
            f.event_times(&cmd(n)),
            [(NOW, NOW, "2026-10-06".to_owned(), None)],
            "cmd {n}"
        );
    }
    assert_eq!(
        f.event_times(&cmd(7)),
        [(NOW, NOW, "2026-10-06".to_owned(), Some(CLIENT_SYNCED))]
    );
    for (field, value) in [("clock_synced_at", CLIENT_SYNCED), ("occurred_at", NOW)] {
        for (method, uri, actor, mut body) in write_commands(&refs, 200).into_iter().take(11) {
            body[field] = json!(value);
            let reply = f.send(method.clone(), &uri, Some(actor), &body).await;
            assert_error(&reply, 400, "VALIDATION_FAILED");
        }
    }
}

// domain「时间」前端校时：实时录入的写命令必须带 occurred_at 和 clock_synced_at，都是 JSON 整数（UTC 毫秒，i64 范围内）；
// 缺少、写成字符串、小数、null 或超出 i64 都是 400 VALIDATION_FAILED（请求结构，先于幂等检查和节点信号）。
// 相对校准时代的 captured_at、sent_at、started_captured_at，以及温度记录、收货、报损不使用的 started_at，都是未知字段。
#[tokio::test]
async fn live_commands_require_integer_occurred_at_and_clock_synced_at() {
    let f = Fixture::new(Some(SYNCED));
    let refs = seed(&f).await;
    let before = f.counts();
    for live in LIVE {
        let good = live.body(&refs, 140, NOW, CLIENT_SYNCED);
        let mut bodies = Vec::new();
        for field in ["occurred_at", "clock_synced_at"] {
            let mut missing = good.clone();
            missing.as_object_mut().unwrap().remove(field);
            bodies.push((format!("{field} missing"), missing));
            for (label, value) in [
                ("string", json!(NOW.to_string())),
                ("fraction", json!(1.5)),
                ("null", Value::Null),
                ("beyond i64", json!(9_223_372_036_854_775_808_u64)),
                ("bool", json!(true)),
            ] {
                let mut body = good.clone();
                body[field] = value;
                bodies.push((format!("{field} {label}"), body));
            }
        }
        for field in [
            "captured_at",
            "sent_at",
            "started_captured_at",
            "started_at",
        ] {
            let mut body = good.clone();
            body[field] = json!(NOW);
            bodies.push((format!("{field} present"), body));
        }
        for (label, body) in bodies {
            let reply = f.post(live.uri(), &STAFF, &body).await;
            assert_eq!(reply.status, 400, "{} {label}: {}", live.name(), reply.body);
            assert_error(&reply, 400, "VALIDATION_FAILED");
        }
    }
    assert_eq!(f.counts(), before);
}
