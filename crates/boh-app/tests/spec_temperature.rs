//! 锁定测试：温度记录接口。规则见 docs/domain.md「温度记录接口」「时间」「事件目录」「主数据」，
//! AGENTS.md「幂等」「HTTP 约定」，接口见 docs/interfaces.md。

mod spec_support;

use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use axum::Router;
use axum::http::Method;
use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use serde_json::{Value, json};
use spec_support::{
    Actor, JsonReply, MANAGER, STAFF, SYSTEM_ACTOR_ID, SYSTEM_DEVICE_ID, assert_error,
    assert_success, is_uuid_v7, non_canonical_uuids, raw_request, request, send,
};
use tempfile::TempDir;

const NOW: i64 = 1_791_248_400_000; // 2026-10-06 09:00 +08:00
const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;
/// 相对校准的上限：72 小时。
const MAX_LAG: i64 = 259_200_000;
/// 平板发送时刻的平板本地时钟：比节点快 7 分钟，相对校准应抵消这个偏差。
const SENT: i64 = NOW + 7 * MINUTE;
const UNKNOWN_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8399";
const URI: &str = "/api/v1/temperature-readings";

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
/// `temperature_readings` 的一行：(id, event_seq, equipment_id, celsius_x10, note, actor_id, device_id,
/// business_date, occurred_at, recorded_at)。
type ReadingRow = (
    String,
    i64,
    String,
    i64,
    Option<String>,
    String,
    String,
    String,
    i64,
    i64,
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

impl Node {
    #[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试；状态码和错误码由调用方断言。
    async fn log(&self, actor: &Actor, body: &Value) -> JsonReply {
        spec_support::post(&self.router, URI, Some(actor), body)
            .await
            .unwrap()
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试；状态码和错误码由调用方断言。
    async fn query(&self, actor: &Actor, query: &str) -> JsonReply {
        spec_support::get(&self.router, &format!("{URI}?{query}"), Some(actor))
            .await
            .unwrap()
    }

    /// 查询成功时的 `data.temperature_readings`。
    async fn list(&self, actor: &Actor, query: &str) -> Value {
        let reply = self.query(actor, query).await;
        let data = assert_success(&reply);
        assert_eq!(reply.body["warnings"], json!([]));
        data["temperature_readings"].clone()
    }

    /// 由店长新建一台设备，返回设备 ID。
    #[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态的请求失败时，后续步骤没有意义，直接终止测试。
    async fn equipment(&self, command_id: &str, code: &str) -> String {
        let reply = spec_support::post(
            &self.router,
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

    /// 停用一台 revision 为 1 的设备。
    #[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态的请求失败时，后续步骤没有意义，直接终止测试。
    async fn deactivate(&self, command_id: &str, equipment_id: &str) {
        let reply = spec_support::put(
            &self.router,
            &format!("/api/v1/equipment/{equipment_id}"),
            Some(&MANAGER),
            &json!({
                "command_id": command_id, "base_revision": 1, "name": "Walk-in",
                "equipment_type": "FRIDGE", "active": false,
            }),
        )
        .await
        .unwrap();
        assert_success(&reply);
    }

    /// 记录一条读数并返回读数 ID。
    async fn log_ok(&self, actor: &Actor, body: &Value) -> String {
        reading_id(&self.log(actor, body).await)
    }

    /// (`store_events` 行数, `processed_commands` 行数)。
    fn counts(&self) -> Rows<(i64, i64)> {
        let reader = spec_support::reader(&self.db_path)?;
        Ok((
            spec_support::count(&reader, "store_events")?,
            spec_support::count(&reader, "processed_commands")?,
        ))
    }

    fn events(&self) -> Rows<Vec<Event>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT seq, event_type, schema_version, aggregate_type, aggregate_id, aggregate_version,
                    command_id, actor_id, device_id, business_date, occurred_at, recorded_at, payload
             FROM store_events ORDER BY seq",
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

    fn readings(&self) -> Rows<Vec<ReadingRow>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT id, event_seq, equipment_id, celsius_x10, note, actor_id, device_id,
                    business_date, occurred_at, recorded_at
             FROM temperature_readings ORDER BY event_seq",
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

    /// 该命令在 `processed_commands` 中的 (`command_type`, 规范化请求, `recorded_at`)。
    fn processed(&self, command_id: &str) -> Rows<(String, Value, i64)> {
        let (command_type, request, recorded_at): (String, String, i64) =
            spec_support::reader(&self.db_path)?.query_row(
                "SELECT command_type, request, recorded_at FROM processed_commands WHERE command_id = ?1",
                [command_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
        Ok((command_type, serde_json::from_str(&request)?, recorded_at))
    }

    /// 账本、读数投影和命令数的快照，用来断言「什么都没写」。
    #[allow(clippy::unwrap_used)] // 测试夹具：只读查询失败时无法比较，直接终止测试。
    fn state(&self) -> (Vec<Event>, Vec<ReadingRow>, (i64, i64)) {
        (
            self.events().unwrap(),
            self.readings().unwrap(),
            self.counts().unwrap(),
        )
    }
}

/// 第 `n` 个命令 ID（UUIDv7）。
fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209a9{n:04x}")
}

/// 实时录入的请求体：平板在 `SENT − lag` 录入、在 `SENT` 发送。
fn body(
    command_id: &str,
    equipment_id: &str,
    celsius_x10: i64,
    note: Option<&str>,
    lag: i64,
) -> Value {
    timed_body(
        command_id,
        equipment_id,
        celsius_x10,
        note,
        SENT - lag,
        SENT,
    )
}

fn timed_body(
    command_id: &str,
    equipment_id: &str,
    celsius_x10: i64,
    note: Option<&str>,
    captured_at: i64,
    sent_at: i64,
) -> Value {
    let mut body = json!({
        "command_id": command_id,
        "equipment_id": equipment_id,
        "celsius_x10": celsius_x10,
        "captured_at": captured_at,
        "sent_at": sent_at,
    });
    if let Some(note) = note {
        body["note"] = json!(note);
    }
    body
}

/// 读数在响应中的行（写命令的 `data.temperature_reading` 和查询的数组元素相同）。
#[allow(clippy::too_many_arguments)]
fn row(
    id: &str,
    equipment_id: &str,
    celsius_x10: i64,
    note: Option<&str>,
    business_date: &str,
    occurred_at: i64,
    recorded_at: i64,
    actor: &Actor,
) -> Value {
    let mut row = json!({
        "temperature_reading_id": id,
        "equipment_id": equipment_id,
        "celsius_x10": celsius_x10,
        "business_date": business_date,
        "occurred_at": occurred_at,
        "recorded_at": recorded_at,
        "actor_id": actor.employee_id,
        "device_id": actor.device_id,
    });
    if let Some(note) = note {
        row["note"] = json!(note);
    }
    row
}

#[allow(clippy::unwrap_used)] // 测试夹具：成功响应缺少读数 ID 已违反接口约定，直接终止测试。
fn reading_id(reply: &JsonReply) -> String {
    let data = assert_success(reply);
    let id = data["temperature_reading"]["temperature_reading_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(is_uuid_v7(&id), "{id}");
    id
}

/// 断言警告列表恰好是给定的警告码，`details` 为 `{}`；`message` 只要求是字符串。
fn assert_warnings(reply: &JsonReply, codes: &[&str]) {
    let warnings = reply.body["warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("warnings must be an array: {}", reply.body));
    let actual: Vec<(&Value, &Value)> = warnings
        .iter()
        .map(|w| {
            assert!(w["message"].is_string(), "{w}");
            let mut keys: Vec<&str> = w
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            keys.sort_unstable();
            assert_eq!(keys, ["code", "details", "message"], "{w}");
            (&w["code"], &w["details"])
        })
        .collect();
    let expected: Vec<(Value, Value)> = codes.iter().map(|c| (json!(c), json!({}))).collect();
    let expected: Vec<(&Value, &Value)> = expected.iter().map(|(c, d)| (c, d)).collect();
    assert_eq!(actual, expected, "{}", reply.body);
}

// ---------------------------------------------------------------------------------------------
// 写入成功
// ---------------------------------------------------------------------------------------------

// 温度记录接口「新建」：普通员工即可记录；一个命令写一条 TEMPERATURE_LOGGED（新的 TEMPERATURE_READING
// 聚合，version 1）、一行 temperature_readings 投影和一行 processed_commands。
// 相对校准抵消平板时钟偏差：lag = sent_at − captured_at = 10 分钟，occurred_at = recorded_at − 10 分钟。
// 省略 note 时 payload、投影和响应都没有 note；规范化请求保留 captured_at，剥离 command_id、sent_at。
#[tokio::test]
async fn log_writes_event_projection_and_command() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;

    let reply = node
        .log(&STAFF, &body(&cmd(2), &f1, 38, None, 10 * MINUTE))
        .await;

    let id = reading_id(&reply);
    assert_ne!(id, f1);
    let occurred_at = NOW - 10 * MINUTE;
    assert_eq!(
        reply.body,
        json!({
            "success": true,
            "data": { "temperature_reading": row(&id, &f1, 38, None, "2026-10-06", occurred_at, NOW, &STAFF) },
            "warnings": [],
            "error": null,
        })
    );
    let events = node.events().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[1],
        Event {
            seq: 2,
            event_type: "TEMPERATURE_LOGGED".into(),
            schema_version: 1,
            aggregate_type: "TEMPERATURE_READING".into(),
            aggregate_id: id.clone(),
            aggregate_version: 1,
            command_id: cmd(2),
            actor_id: STAFF.employee_id.into(),
            device_id: STAFF.device_id.into(),
            business_date: "2026-10-06".into(),
            occurred_at,
            recorded_at: NOW,
            payload: json!({ "equipment_id": f1, "celsius_x10": 38 }),
        }
    );
    assert_eq!(
        node.readings().unwrap(),
        [(
            id,
            2,
            f1.clone(),
            38,
            None,
            STAFF.employee_id.into(),
            STAFF.device_id.into(),
            "2026-10-06".into(),
            occurred_at,
            NOW,
        )]
    );
    assert_eq!(
        node.processed(&cmd(2)).unwrap(),
        (
            "temperature.log".into(),
            json!({ "equipment_id": f1, "celsius_x10": 38, "captured_at": SENT - 10 * MINUTE }),
            NOW,
        )
    );
}

// 事件目录 TEMPERATURE_LOGGED：note 出现时原样写入 payload、投影、响应和规范化请求；
// 首尾以外的空白、换行和非 ASCII 文本原样保存。每条读数是一个新聚合。
#[tokio::test]
async fn note_is_kept_verbatim_and_each_reading_is_a_new_aggregate() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    let note = "门封条 结霜\n已通知店长";

    let first = node
        .log_ok(&MANAGER, &body(&cmd(2), &f1, -185, Some(note), 0))
        .await;
    let reply = node
        .log(&MANAGER, &body(&cmd(3), &f1, -185, Some(note), 0))
        .await;

    let second = reading_id(&reply);
    assert_ne!(first, second);
    assert_eq!(
        assert_success(&reply),
        &json!({ "temperature_reading": row(&second, &f1, -185, Some(note), "2026-10-06", NOW, NOW, &MANAGER) })
    );
    let events = node.events().unwrap();
    assert_eq!(
        events[2].payload,
        json!({ "equipment_id": f1, "celsius_x10": -185, "note": note })
    );
    let aggregates: Vec<(&str, i64)> = events[1..]
        .iter()
        .map(|e| (e.aggregate_id.as_str(), e.aggregate_version))
        .collect();
    assert_eq!(aggregates, [(first.as_str(), 1), (second.as_str(), 1)]);
    assert_eq!(node.readings().unwrap()[1].4.as_deref(), Some(note));
    assert_eq!(
        node.processed(&cmd(3)).unwrap().1,
        json!({ "equipment_id": f1, "celsius_x10": -185, "note": note, "captured_at": SENT })
    );
}

// 取值边界：celsius_x10 取 -500 和 5000、note 恰好 200 个字符（按字符计）时照常受理。
#[tokio::test]
async fn boundary_values_are_accepted() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    let note = "冷".repeat(200);

    node.log_ok(&STAFF, &body(&cmd(2), &f1, -500, None, 0))
        .await;
    node.log_ok(&STAFF, &body(&cmd(3), &f1, 5000, None, 0))
        .await;
    node.log_ok(&STAFF, &body(&cmd(4), &f1, 0, Some(&note), 0))
        .await;

    let stored: Vec<(i64, Option<String>)> = node
        .readings()
        .unwrap()
        .into_iter()
        .map(|r| (r.3, r.4))
        .collect();
    assert_eq!(stored, [(-500, None), (5000, None), (0, Some(note))]);
}

// domain「主数据」：命令引用已停用的主数据照常受理，停用只在界面上隐藏。
#[tokio::test]
async fn inactive_equipment_is_accepted() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    node.deactivate(&cmd(2), &f1).await;

    let id = node.log_ok(&STAFF, &body(&cmd(3), &f1, 45, None, 0)).await;

    assert_eq!(node.readings().unwrap()[0].0, id);
    assert_eq!(node.counts().unwrap(), (3, 3));
}

// 温度记录接口「新建」：不限制 equipment_type——烤箱、醒发箱等非冷藏设备上的读数照常受理。
#[tokio::test]
async fn any_equipment_type_is_accepted() {
    let node = node();
    let mut ids = Vec::new();
    for (n, kind) in [
        "FREEZER",
        "BLAST_FREEZER",
        "OVEN",
        "PROOFER",
        "MIXER",
        "OTHER",
    ]
    .into_iter()
    .enumerate()
    {
        let reply = spec_support::post(
            &node.router,
            "/api/v1/equipment",
            Some(&MANAGER),
            &json!({
                "command_id": cmd(10 + n as u16), "code": kind, "name": kind,
                "equipment_type": kind, "active": true,
            }),
        )
        .await
        .unwrap();
        ids.push(
            assert_success(&reply)["equipment"]["equipment_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }

    for (n, id) in ids.iter().enumerate() {
        node.log_ok(&STAFF, &body(&cmd(20 + n as u16), id, 1800, None, 0))
            .await;
    }

    assert_eq!(node.readings().unwrap().len(), 6);
    assert_eq!(node.counts().unwrap(), (12, 12));
}

// ---------------------------------------------------------------------------------------------
// 业务校验
// ---------------------------------------------------------------------------------------------

// 温度记录接口：设备不存在时 404 REFERENCE_NOT_FOUND，details 给 entity 和 id，什么都不写；
// 被拒绝的命令不落库，修正后可以用同一个 command_id 重提。
#[tokio::test]
async fn unknown_equipment_is_not_found() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    let before = node.state();

    let reply = node
        .log(&STAFF, &body(&cmd(2), UNKNOWN_ID, 38, None, 0))
        .await;

    assert_eq!(
        assert_error(&reply, 404, "REFERENCE_NOT_FOUND"),
        &json!({ "entity": "EQUIPMENT", "id": UNKNOWN_ID })
    );
    assert_eq!(node.state(), before);
    node.log_ok(&STAFF, &body(&cmd(2), &f1, 38, None, 0)).await;
    assert_eq!(node.counts().unwrap(), (2, 2));
}

// AGENTS「幂等」：被请求结构与取值校验拒绝（400 VALIDATION_FAILED）的命令不落库；修正内容后用同一个 command_id 重提，
// 按新命令处理并成功。
#[tokio::test]
async fn a_command_rejected_as_invalid_can_be_corrected_with_the_same_id() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    let before = node.state();

    let reply = node
        .log(&STAFF, &body(&cmd(2), &f1, 5001, Some("录错"), 0))
        .await;
    assert_error(&reply, 400, "VALIDATION_FAILED");
    assert_eq!(node.state(), before);

    let id = node
        .log_ok(&STAFF, &body(&cmd(2), &f1, 501, Some("录错"), 0))
        .await;
    let readings = node.readings().unwrap();
    assert_eq!((readings.len(), readings[0].0.as_str()), (1, id.as_str()));
    assert_eq!(node.counts().unwrap(), (2, 2));
}

// domain「时间」相对校准：lag < 0 按 0 处理，occurred_at = recorded_at，返回警告 CAPTURE_TIME_ADJUSTED；
// 极端的负 lag（captured_at = i64::MAX）同样归零，不是溢出。
#[tokio::test]
async fn negative_lag_is_clamped_with_a_warning() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;

    let reply = node
        .log(&STAFF, &body(&cmd(2), &f1, 38, None, -2 * MINUTE))
        .await;
    let id = reading_id(&reply);
    assert_eq!(
        reply.body["data"],
        json!({ "temperature_reading": row(&id, &f1, 38, None, "2026-10-06", NOW, NOW, &STAFF) })
    );
    assert_warnings(&reply, &["CAPTURE_TIME_ADJUSTED"]);

    let reply = node
        .log(&STAFF, &timed_body(&cmd(3), &f1, 38, None, i64::MAX, SENT))
        .await;
    assert_eq!(
        assert_success(&reply)["temperature_reading"]["occurred_at"],
        json!(NOW)
    );
    assert_warnings(&reply, &["CAPTURE_TIME_ADJUSTED"]);

    let occurred: Vec<i64> = node.events().unwrap()[1..]
        .iter()
        .map(|e| e.occurred_at)
        .collect();
    assert_eq!(occurred, [NOW, NOW]);
}

// domain「时间」：lag 恰好 72 小时照常受理，营业日按 occurred_at（三天前 09:00）计算，没有警告。
#[tokio::test]
async fn lag_of_exactly_72_hours_is_accepted() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;

    let reply = node
        .log(&STAFF, &body(&cmd(2), &f1, 38, None, MAX_LAG))
        .await;

    let id = reading_id(&reply);
    assert_eq!(
        reply.body["data"],
        json!({ "temperature_reading": row(&id, &f1, 38, None, "2026-10-03", NOW - MAX_LAG, NOW, &STAFF) })
    );
    assert_warnings(&reply, &[]);
}

// domain「时间」：lag 超过 72 小时 400 CAPTURE_TOO_OLD（details 为 {}），什么都不写；
// 修正后可以用同一个 command_id 重提。
#[tokio::test]
async fn lag_over_72_hours_is_capture_too_old() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    let before = node.state();

    let reply = node
        .log(&STAFF, &body(&cmd(2), &f1, 38, None, MAX_LAG + 1))
        .await;

    assert_eq!(assert_error(&reply, 400, "CAPTURE_TOO_OLD"), &json!({}));
    assert_eq!(node.state(), before);
    node.log_ok(&STAFF, &body(&cmd(2), &f1, 38, None, MAX_LAG))
        .await;
    assert_eq!(node.counts().unwrap(), (2, 2));
}

// interfaces「calibrate」与「business_date」：差值溢出、occurred_at 换算溢出、营业日无法换算，
// 都是 400 VALIDATION_FAILED（details 为 {}），什么都不写。设备存在，只有时间不合法。
#[tokio::test]
async fn out_of_range_times_are_validation_failed() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    let before = node.state();

    // sent_at − captured_at 溢出。
    for (captured_at, sent_at) in [(i64::MIN, 1), (-1, i64::MAX)] {
        let reply = node
            .log(
                &STAFF,
                &timed_body(&cmd(2), &f1, 38, None, captured_at, sent_at),
            )
            .await;
        assert_eq!(assert_error(&reply, 400, "VALIDATION_FAILED"), &json!({}));
    }
    // 节点时钟异常：recorded_at − lag 溢出；lag 为 0 时 occurred_at 可表示但营业日无法换算。
    node.clock.set(UnixMillis(i64::MIN + 1_000));
    for lag in [2_000, 0] {
        let reply = node.log(&STAFF, &body(&cmd(2), &f1, 38, None, lag)).await;
        assert_eq!(assert_error(&reply, 400, "VALIDATION_FAILED"), &json!({}));
    }

    assert_eq!(node.state(), before);
}

// domain「时间」营业日：由 occurred_at（不是 recorded_at）按 Asia/Shanghai 与日切 04:00 计算。
// recorded_at 为 09:00；lag 5 小时得 04:00 归当日，再早 1ms 归前一营业日。
#[tokio::test]
async fn business_date_follows_occurred_at() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;

    node.log_ok(&STAFF, &body(&cmd(2), &f1, 38, None, 5 * HOUR))
        .await;
    node.log_ok(&STAFF, &body(&cmd(3), &f1, 38, None, 5 * HOUR + 1))
        .await;

    let dates: Vec<(String, i64, i64)> = node
        .readings()
        .unwrap()
        .into_iter()
        .map(|r| (r.7, r.8, r.9))
        .collect();
    assert_eq!(
        dates,
        [
            ("2026-10-06".into(), NOW - 5 * HOUR, NOW),
            ("2026-10-05".into(), NOW - 5 * HOUR - 1, NOW),
        ]
    );
}

// AGENTS「ID 与时间」：recorded_at 取系统时钟原值，不做单调钳制；时钟回拨后照常受理，
// 不返回警告，seq 照常递增，营业日按回拨后的 occurred_at 计算，可按该营业日查到。
#[tokio::test]
async fn clock_regression_is_recorded_as_is() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    node.log_ok(&STAFF, &body(&cmd(2), &f1, 38, None, 0)).await;
    node.clock.set(UnixMillis(NOW - 2 * DAY));

    let reply = node
        .log(&STAFF, &body(&cmd(3), &f1, 40, None, MINUTE))
        .await;

    let id = reading_id(&reply);
    assert_warnings(&reply, &[]);
    let expected = row(
        &id,
        &f1,
        40,
        None,
        "2026-10-04",
        NOW - 2 * DAY - MINUTE,
        NOW - 2 * DAY,
        &STAFF,
    );
    assert_eq!(
        reply.body["data"],
        json!({ "temperature_reading": expected })
    );
    let times: Vec<(i64, i64, i64)> = node.events().unwrap()[1..]
        .iter()
        .map(|e| (e.seq, e.occurred_at, e.recorded_at))
        .collect();
    assert_eq!(
        times,
        [(2, NOW, NOW), (3, NOW - 2 * DAY - MINUTE, NOW - 2 * DAY),]
    );
    assert_eq!(
        node.list(&STAFF, "business_date=2026-10-04").await,
        json!([expected])
    );
}

// ---------------------------------------------------------------------------------------------
// 幂等与处理顺序
// ---------------------------------------------------------------------------------------------

// AGENTS「幂等」与 domain「时间」：重试返回首次的响应（含 warnings），不重新校准时间。
// 首次 lag 为负（归零并警告）；一小时后用原 command_id、原内容重发，只有 sent_at 不同——
// 包括按新的 sent_at 计算会超过 72 小时或溢出的情况——都原样返回首次响应，不新增事件或命令记录。
#[tokio::test]
async fn retries_return_the_original_response_without_recalibrating() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    let captured_at = SENT + MINUTE;
    let first = node
        .log(
            &STAFF,
            &timed_body(&cmd(2), &f1, 38, None, captured_at, SENT),
        )
        .await;
    reading_id(&first);
    assert_warnings(&first, &["CAPTURE_TIME_ADJUSTED"]);
    let before = node.state();
    node.clock.advance(Duration::from_secs(3600));

    for sent_at in [
        SENT,
        captured_at + 10 * MINUTE,
        captured_at + MAX_LAG + 1,
        i64::MIN,
    ] {
        let reply = node
            .log(
                &STAFF,
                &timed_body(&cmd(2), &f1, 38, None, captured_at, sent_at),
            )
            .await;
        assert_eq!(reply.status, 200, "sent_at = {sent_at}");
        assert_eq!(reply.body, first.body, "sent_at = {sent_at}");
    }

    assert_eq!(node.state(), before);
}

// AGENTS「幂等」：同一 command_id、内容不同时 409 IDEMPOTENCY_CONFLICT，details.fields 为不同的顶层字段名
// （字典序，只在一方出现的字段也算）；command_type 不同时只列 "command_type"。
// 幂等检查先于业务校验：改成不存在的设备、或 captured_at 改到 72 小时以外，仍是 409，不是 404 / CAPTURE_TOO_OLD。
#[tokio::test]
async fn idempotency_conflicts_list_differing_fields() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    let f2 = node.equipment(&cmd(2), "F2").await;
    let x = cmd(3);
    let y = cmd(4);
    node.log_ok(&STAFF, &body(&x, &f1, 38, None, 10 * MINUTE))
        .await;
    node.log_ok(&STAFF, &body(&y, &f1, 40, Some("a"), 0)).await;
    let before = node.state();

    let cases: Vec<(Value, Value)> = vec![
        (body(&x, &f1, 39, None, 10 * MINUTE), json!(["celsius_x10"])),
        (body(&x, &f1, 38, Some("b"), 10 * MINUTE), json!(["note"])),
        (
            body(&x, &f2, 38, None, 10 * MINUTE),
            json!(["equipment_id"]),
        ),
        (
            body(&x, UNKNOWN_ID, 38, None, 10 * MINUTE),
            json!(["equipment_id"]),
        ),
        // captured_at 与 sent_at 同时前移：lag 不变，规范化请求中只有 captured_at 不同。
        (
            timed_body(&x, &f1, 38, None, SENT - 11 * MINUTE, SENT - MINUTE),
            json!(["captured_at"]),
        ),
        (body(&x, &f1, 38, None, MAX_LAG + 1), json!(["captured_at"])),
        (
            body(&x, &f2, 39, Some("b"), 10 * MINUTE),
            json!(["celsius_x10", "equipment_id", "note"]),
        ),
        (body(&y, &f1, 40, None, 0), json!(["note"])),
        // 设备新建命令的 command_id 用于温度记录。
        (body(&cmd(1), &f1, 38, None, 0), json!(["command_type"])),
    ];
    for (request, fields) in &cases {
        let reply = node.log(&STAFF, request).await;
        assert_eq!(
            assert_error(&reply, 409, "IDEMPOTENCY_CONFLICT"),
            &json!({ "fields": fields }),
            "{request}"
        );
    }
    // 温度记录的 command_id 用于设备新建。
    let reply = spec_support::post(
        &node.router,
        "/api/v1/equipment",
        Some(&MANAGER),
        &json!({
            "command_id": x, "code": "F9", "name": "Walk-in",
            "equipment_type": "FRIDGE", "active": true,
        }),
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
#[tokio::test]
async fn validation_precedes_the_idempotency_check() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    let original = body(&cmd(2), &f1, 38, None, 0);
    node.log_ok(&STAFF, &original).await;
    let before = node.state();

    for (key, value) in [
        ("celsius_x10", json!("38")),
        ("note", json!("")),
        ("equipment_id", json!("not-a-uuid")),
    ] {
        let mut request = original.clone();
        request[key] = value;
        let reply = node.log(&STAFF, &request).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }

    assert_eq!(node.state(), before);
}

// domain「员工认证」开发桩与 AGENTS「HTTP 约定」：写入和查询缺少或使用非法身份时 401 UNAUTHENTICATED；
// 身份检查先于请求体和查询参数——请求体无法解析、取值非法或查询参数非法时，无身份仍是 401。什么都不写。
#[tokio::test]
async fn identity_is_required_and_checked_first() {
    let node = node();
    let valid_body = body(&cmd(1), UNKNOWN_ID, 38, None, 0);
    let valid_query = format!("{URI}?business_date=2026-10-06");

    let headers = [
        ("X-Dev-Employee-Id", MANAGER.employee_id),
        ("X-Dev-Device-Id", MANAGER.device_id),
        ("X-Dev-Role", MANAGER.role),
    ];
    let replacements: [Option<(&str, &str)>; 5] = [
        None, // 不带任何身份请求头
        Some(("X-Dev-Role", "manager")),
        Some(("X-Dev-Employee-Id", SYSTEM_ACTOR_ID)),
        Some(("X-Dev-Device-Id", SYSTEM_DEVICE_ID)),
        Some(("X-Dev-Employee-Id", "01890a5d-ac96-474b-bcce-b302099a8101")), // v4
    ];
    for (method, uri, body) in [
        (Method::POST, URI.to_owned(), Some(&valid_body)),
        (Method::GET, valid_query.clone(), None),
    ] {
        for replacement in replacements {
            let mut req = request(method.clone(), &uri, None, body).unwrap();
            if let Some((replaced, new_value)) = replacement {
                for (name, value) in headers {
                    let value = if name == replaced { new_value } else { value };
                    req.headers_mut().insert(name, value.parse().unwrap());
                }
            }
            let reply = send(&node.router, req).await.unwrap();
            assert_error(&reply, 401, "UNAUTHENTICATED");
        }
    }

    let json = Some("application/json");
    let valid_text = valid_body.to_string();
    let invalid_text = json!({ "command_id": "not-a-uuid", "celsius_x10": 1.5 }).to_string();
    for (text, content_type) in [
        ("{", json),
        ("", json),
        (invalid_text.as_str(), json),
        (valid_text.as_str(), None),
        (valid_text.as_str(), Some("text/plain")),
    ] {
        let req = raw_request(Method::POST, URI, None, content_type, text).unwrap();
        let reply = send(&node.router, req).await.unwrap();
        assert_error(&reply, 401, "UNAUTHENTICATED");
    }
    for query in [
        "",
        "?business_date=2026-02-30",
        "?business_date=2026-10-06&foo=1",
    ] {
        let reply = spec_support::get(&node.router, &format!("{URI}{query}"), None)
            .await
            .unwrap();
        assert_error(&reply, 401, "UNAUTHENTICATED");
    }

    assert_eq!(node.counts().unwrap(), (0, 0));
}

// 温度记录接口「取值」与 AGENTS「HTTP 约定」：字段缺失、未知字段（含补录用的 occurred_at）、类型或取值不对、
// 请求体无法解析，一律 400 VALIDATION_FAILED，什么都不写。
#[tokio::test]
async fn invalid_commands_are_rejected_with_validation_failed() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    let base = body(&cmd(2), &f1, 38, Some("ok"), 0);
    let before = node.state();

    let with = |key: &str, value: Value| {
        let mut body = base.clone();
        body[key] = value;
        body
    };
    let without = |key: &str| {
        let mut body = base.clone();
        body.as_object_mut().unwrap().remove(key);
        body
    };

    let too_big = json!(9_223_372_036_854_775_808_u64); // i64::MAX + 1
    let mut bodies = vec![
        with("celsius_x10", json!(38.5)),
        with("celsius_x10", json!(38.0)),
        with("celsius_x10", json!("38")),
        with("celsius_x10", json!(true)),
        with("celsius_x10", Value::Null),
        with("celsius_x10", json!(-501)),
        with("celsius_x10", json!(5001)),
        with("celsius_x10", too_big.clone()),
        with("note", json!("")),
        with("note", json!(" 结霜")),
        with("note", json!("结霜 ")),
        with("note", json!("\u{3000}结霜")),
        with("note", json!("结霜\n")),
        with("note", Value::Null),
        with("note", json!(1)),
        with("note", json!("a".repeat(201))),
        with("note", json!("冷".repeat(201))),
        with("equipment_id", json!("not-a-uuid")),
        with(
            "equipment_id",
            json!("01890a5d-ac96-474b-bcce-b302099a8301"),
        ), // v4
        with("equipment_id", Value::Null),
        with("captured_at", json!("1791248400000")),
        with("captured_at", json!(1.5)),
        with("captured_at", Value::Null),
        with("captured_at", too_big.clone()),
        with("sent_at", json!("1791248400000")),
        with("sent_at", json!(1.5)),
        with("sent_at", Value::Null),
        with("sent_at", too_big),
        with("command_id", json!("not-a-uuid")),
        with("command_id", json!("01890a5d-ac96-474b-bcce-b302099a8401")), // v4
        with("occurred_at", json!(NOW)),                                   // 补录不属于本切片
        with("started_captured_at", json!(SENT)),
        with("unit", json!("C")),
    ];
    // AGENTS「ID 与时间」：只接受 36 位小写带连字符的形式。
    for text in non_canonical_uuids(&f1) {
        bodies.push(with("equipment_id", json!(text)));
    }
    for text in non_canonical_uuids(&cmd(2)) {
        bodies.push(with("command_id", json!(text)));
    }
    for key in [
        "command_id",
        "equipment_id",
        "celsius_x10",
        "captured_at",
        "sent_at",
    ] {
        bodies.push(without(key));
    }
    for request in &bodies {
        let reply = node.log(&STAFF, request).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }

    let json = Some("application/json");
    let valid_text = base.to_string();
    for (text, content_type) in [
        ("{", json),
        ("", json),
        ("[]", json),
        (valid_text.as_str(), None),
        (valid_text.as_str(), Some("text/plain")),
    ] {
        let req = raw_request(Method::POST, URI, Some(&STAFF), content_type, text).unwrap();
        let reply = send(&node.router, req).await.unwrap();
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }

    assert_eq!(node.state(), before);
}

// ---------------------------------------------------------------------------------------------
// 查询
// ---------------------------------------------------------------------------------------------

// 温度记录接口「查询」：business_date 必填，equipment_id 可选；按 occurred_at 升序、同一时刻按 seq 升序，
// 不按提交顺序；含停用设备的读数；每行带操作人、设备、recorded_at 和 occurred_at。
// 没有匹配的读数（含不存在的设备 ID）时为空数组。
#[tokio::test]
async fn list_filters_by_business_date_and_equipment() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    let f2 = node.equipment(&cmd(2), "F2").await;
    let r1 = node
        .log_ok(&STAFF, &body(&cmd(3), &f1, 38, None, 10 * MINUTE))
        .await;
    let r2 = node
        .log_ok(&STAFF, &body(&cmd(4), &f2, -185, Some("结霜"), 30 * MINUTE))
        .await;
    let r3 = node
        .log_ok(&MANAGER, &body(&cmd(5), &f1, 40, None, 0))
        .await;
    let r4 = node
        .log_ok(&STAFF, &body(&cmd(6), &f1, 36, None, 30 * MINUTE))
        .await;
    let r5 = node
        .log_ok(&STAFF, &body(&cmd(7), &f1, 35, None, 6 * HOUR))
        .await;
    node.deactivate(&cmd(8), &f2).await;

    let today = "2026-10-06";
    let row1 = row(&r1, &f1, 38, None, today, NOW - 10 * MINUTE, NOW, &STAFF);
    let row2 = row(
        &r2,
        &f2,
        -185,
        Some("结霜"),
        today,
        NOW - 30 * MINUTE,
        NOW,
        &STAFF,
    );
    let row3 = row(&r3, &f1, 40, None, today, NOW, NOW, &MANAGER);
    let row4 = row(&r4, &f1, 36, None, today, NOW - 30 * MINUTE, NOW, &STAFF);
    let row5 = row(
        &r5,
        &f1,
        35,
        None,
        "2026-10-05",
        NOW - 6 * HOUR,
        NOW,
        &STAFF,
    );

    assert_eq!(
        node.list(&STAFF, "business_date=2026-10-06").await,
        json!([row2, row4, row1, row3])
    );
    assert_eq!(
        node.list(
            &MANAGER,
            &format!("business_date=2026-10-06&equipment_id={f1}")
        )
        .await,
        json!([row4, row1, row3])
    );
    assert_eq!(
        node.list(
            &STAFF,
            &format!("equipment_id={f2}&business_date=2026-10-06")
        )
        .await,
        json!([row2])
    );
    assert_eq!(
        node.list(&STAFF, "business_date=2026-10-05").await,
        json!([row5])
    );
    assert_eq!(
        node.list(&STAFF, "business_date=2026-10-07").await,
        json!([])
    );
    assert_eq!(
        node.list(
            &STAFF,
            &format!("business_date=2026-10-06&equipment_id={UNKNOWN_ID}")
        )
        .await,
        json!([])
    );
}

// 温度记录接口「查询」与 AGENTS「HTTP 约定」「ID 与时间」：缺少 business_date、日期格式或取值非法、equipment_id 不是
// 规范形式的 UUIDv7、未知或重复的查询参数（含重复的 equipment_id），一律 400 VALIDATION_FAILED。
#[tokio::test]
async fn invalid_queries_are_rejected_with_validation_failed() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    node.log_ok(&STAFF, &body(&cmd(2), &f1, 38, None, 0)).await;

    for query in [
        "".to_owned(),
        format!("equipment_id={f1}"),
        "business_date=".into(),
        "business_date=2026-02-30".into(),
        "business_date=2026-13-01".into(),
        "business_date=2026-1-5".into(),
        "business_date=20261006".into(),
        "business_date=2026%2F10%2F06".into(),
        "business_date=2026-10-06&equipment_id=not-a-uuid".into(),
        "business_date=2026-10-06&equipment_id=01890a5d-ac96-474b-bcce-b302099a8301".into(),
        "business_date=2026-10-06&equipment_id=".into(),
        "business_date=2026-10-06&foo=1".into(),
        "business_date=2026-10-06&business_date=2026-10-05".into(),
        format!("business_date=2026-10-06&equipment_id={f1}&equipment_id={f1}"),
        format!("business_date=2026-10-06&equipment_id={f1}&equipment_id={UNKNOWN_ID}"),
    ]
    .into_iter()
    .chain(non_canonical_uuids(&f1).map(|text| {
        let text = text.replace('{', "%7B").replace('}', "%7D");
        format!("business_date=2026-10-06&equipment_id={text}")
    })) {
        let reply = node.query(&STAFF, &query).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }
}

// 查询走读连接：不写事件、投影或 processed_commands。
#[tokio::test]
async fn queries_do_not_write() {
    let node = node();
    let f1 = node.equipment(&cmd(1), "F1").await;
    node.log_ok(&STAFF, &body(&cmd(2), &f1, 38, None, 0)).await;
    let before = node.state();

    for actor in [&STAFF, &MANAGER] {
        for query in ["business_date=2026-10-06", "business_date=2026-02-30"] {
            node.query(actor, query).await;
        }
    }

    assert_eq!(node.state(), before);
}

// AGENTS「HTTP 约定」：路由存在但方法不允许时 405 METHOD_NOT_ALLOWED。
#[tokio::test]
async fn wrong_method_returns_method_not_allowed() {
    let node = node();

    for method in [Method::PUT, Method::PATCH, Method::DELETE] {
        let reply = send(
            &node.router,
            request(method.clone(), URI, Some(&MANAGER), None).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            assert_error(&reply, 405, "METHOD_NOT_ALLOWED"),
            &json!({}),
            "{method}"
        );
    }
}
