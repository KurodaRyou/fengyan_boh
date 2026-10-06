//! 锁定测试：设备主数据接口（walking skeleton）。规则见 docs/domain.md「主数据」「设备接口」「时间」，
//! AGENTS.md「幂等」「HTTP 约定」，接口见 docs/interfaces.md。

mod spec_support;

use std::error::Error;
use std::path::PathBuf;

use axum::Router;
use axum::http::Method;
use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use serde_json::{Value, json};
use spec_support::{
    JsonReply, MANAGER, STAFF, assert_error, assert_success, is_uuid_v7, raw_request, request, send,
};
use tempfile::TempDir;

const NOW: i64 = 1_791_248_400_000; // 2026-10-06 09:00 +08:00
const CUTOFF_0400: i64 = 1_791_230_400_000; // 2026-10-06 04:00 +08:00
const UNKNOWN_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8399";

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
/// `equipment` 表的一行：(id, code, name, equipment_type, active, revision)。
type EquipmentRow = (String, String, String, String, i64, i64);

impl Node {
    #[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试；状态码和错误码由调用方断言。
    async fn create(&self, body: &Value) -> JsonReply {
        spec_support::post(&self.router, "/api/v1/equipment", Some(&MANAGER), body)
            .await
            .unwrap()
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试；状态码和错误码由调用方断言。
    async fn update(&self, equipment_id: &str, body: &Value) -> JsonReply {
        spec_support::put(
            &self.router,
            &format!("/api/v1/equipment/{equipment_id}"),
            Some(&MANAGER),
            body,
        )
        .await
        .unwrap()
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试。
    async fn list(&self) -> Value {
        let reply = spec_support::get(&self.router, "/api/v1/equipment", Some(&STAFF))
            .await
            .unwrap();
        assert_success(&reply).clone()
    }

    /// 新建一台设备并返回它的 ID。
    async fn create_ok(&self, command_id: &str, code: &str) -> String {
        let reply = self
            .create(&create_body(command_id, code, "Walk-in", "FRIDGE", true))
            .await;
        equipment_id(&reply)
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
            "SELECT event_type, schema_version, aggregate_type, aggregate_id, aggregate_version,
                    command_id, actor_id, device_id, business_date, occurred_at, recorded_at, payload
             FROM store_events ORDER BY seq",
        )?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    Event {
                        event_type: r.get(0)?,
                        schema_version: r.get(1)?,
                        aggregate_type: r.get(2)?,
                        aggregate_id: r.get(3)?,
                        aggregate_version: r.get(4)?,
                        command_id: r.get(5)?,
                        actor_id: r.get(6)?,
                        device_id: r.get(7)?,
                        business_date: r.get(8)?,
                        occurred_at: r.get(9)?,
                        recorded_at: r.get(10)?,
                        payload: Value::Null,
                    },
                    r.get::<_, String>(11)?,
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

    fn equipment_rows(&self) -> Rows<Vec<EquipmentRow>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT id, code, name, equipment_type, active, revision FROM equipment ORDER BY code",
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
}

#[derive(Debug, PartialEq)]
struct Event {
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

/// 第 `n` 个命令 ID（UUIDv7）。
fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209a9{n:04x}")
}

fn create_body(
    command_id: &str,
    code: &str,
    name: &str,
    equipment_type: &str,
    active: bool,
) -> Value {
    json!({
        "command_id": command_id,
        "code": code,
        "name": name,
        "equipment_type": equipment_type,
        "active": active,
    })
}

fn update_body(
    command_id: &str,
    base_revision: i64,
    name: &str,
    equipment_type: &str,
    active: bool,
) -> Value {
    json!({
        "command_id": command_id,
        "base_revision": base_revision,
        "name": name,
        "equipment_type": equipment_type,
        "active": active,
    })
}

fn row(
    id: &str,
    code: &str,
    name: &str,
    equipment_type: &str,
    active: bool,
    revision: i64,
) -> Value {
    json!({
        "equipment_id": id,
        "code": code,
        "name": name,
        "equipment_type": equipment_type,
        "active": active,
        "revision": revision,
    })
}

#[allow(clippy::unwrap_used)] // 测试夹具：成功响应缺少 equipment_id 已违反接口约定，直接终止测试。
fn equipment_id(reply: &JsonReply) -> String {
    let data = assert_success(reply);
    let id = data["equipment"]["equipment_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(is_uuid_v7(&id), "{id}");
    id
}

fn snapshot(code: &str, name: &str, equipment_type: &str, active: bool) -> Value {
    json!({
        "entity": "EQUIPMENT",
        "source": "LOCAL",
        "snapshot": { "code": code, "name": name, "equipment_type": equipment_type, "active": active },
    })
}

// 设备接口「新建」：写一条 aggregate_version 1 的 MASTER_DATA_CHANGED、投影行和 processed_commands；
// 身份取自开发桩；主数据命令 occurred_at = recorded_at，营业日按门店时区计算。
#[tokio::test]
async fn create_writes_event_projection_and_command() {
    let node = node();

    let reply = node
        .create(&create_body(
            &cmd(1),
            "F1",
            "Walk-in fridge",
            "FRIDGE",
            true,
        ))
        .await;

    let id = equipment_id(&reply);
    assert_eq!(
        reply.body,
        json!({
            "success": true,
            "data": { "equipment": row(&id, "F1", "Walk-in fridge", "FRIDGE", true, 1) },
            "warnings": [],
            "error": null,
        })
    );
    assert_eq!(
        node.events().unwrap(),
        [Event {
            event_type: "MASTER_DATA_CHANGED".into(),
            schema_version: 1,
            aggregate_type: "EQUIPMENT".into(),
            aggregate_id: id.clone(),
            aggregate_version: 1,
            command_id: cmd(1),
            actor_id: MANAGER.employee_id.into(),
            device_id: MANAGER.device_id.into(),
            business_date: "2026-10-06".into(),
            occurred_at: NOW,
            recorded_at: NOW,
            payload: snapshot("F1", "Walk-in fridge", "FRIDGE", true),
        }]
    );
    assert_eq!(
        node.equipment_rows().unwrap(),
        [(
            id,
            "F1".into(),
            "Walk-in fridge".into(),
            "FRIDGE".into(),
            1,
            1
        )]
    );
    assert_eq!(
        node.processed(&cmd(1)).unwrap(),
        (
            "equipment.create".into(),
            json!({ "code": "F1", "name": "Walk-in fridge", "equipment_type": "FRIDGE", "active": true }),
            NOW,
        )
    );
}

// domain「时间」营业日：Asia/Shanghai 当地 04:00 之前归前一营业日，恰好 04:00 归当日。
#[tokio::test]
async fn business_date_follows_store_timezone_and_cutoff() {
    let node = node();

    node.clock.set(UnixMillis(CUTOFF_0400 - 1));
    node.create_ok(&cmd(1), "F1").await;
    node.clock.set(UnixMillis(CUTOFF_0400));
    node.create_ok(&cmd(2), "F2").await;

    let dates: Vec<(String, i64, i64)> = node
        .events()
        .unwrap()
        .into_iter()
        .map(|e| (e.business_date, e.occurred_at, e.recorded_at))
        .collect();
    assert_eq!(
        dates,
        [
            ("2026-10-05".into(), CUTOFF_0400 - 1, CUTOFF_0400 - 1),
            ("2026-10-06".into(), CUTOFF_0400, CUTOFF_0400),
        ]
    );
}

// 设备接口「修改」：revision +1，code 保持不变，事件 aggregate_version 等于新 revision；
// 规范化请求含路径参数 equipment_id，不含 command_id。
#[tokio::test]
async fn update_writes_next_revision_and_keeps_code() {
    let node = node();
    let id = node.create_ok(&cmd(1), "F1").await;
    node.clock.advance(std::time::Duration::from_secs(60));

    let reply = node
        .update(
            &id,
            &update_body(&cmd(2), 1, "Blast chiller", "BLAST_FREEZER", false),
        )
        .await;

    assert_eq!(
        assert_success(&reply),
        &json!({ "equipment": row(&id, "F1", "Blast chiller", "BLAST_FREEZER", false, 2) })
    );
    assert_eq!(reply.body["warnings"], json!([]));
    let events = node.events().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[1],
        Event {
            event_type: "MASTER_DATA_CHANGED".into(),
            schema_version: 1,
            aggregate_type: "EQUIPMENT".into(),
            aggregate_id: id.clone(),
            aggregate_version: 2,
            command_id: cmd(2),
            actor_id: MANAGER.employee_id.into(),
            device_id: MANAGER.device_id.into(),
            business_date: "2026-10-06".into(),
            occurred_at: NOW + 60_000,
            recorded_at: NOW + 60_000,
            payload: snapshot("F1", "Blast chiller", "BLAST_FREEZER", false),
        }
    );
    assert_eq!(
        node.equipment_rows().unwrap(),
        [(
            id.clone(),
            "F1".into(),
            "Blast chiller".into(),
            "BLAST_FREEZER".into(),
            0,
            2
        )]
    );
    assert_eq!(
        node.processed(&cmd(2)).unwrap(),
        (
            "equipment.update".into(),
            json!({
                "equipment_id": id, "base_revision": 1, "name": "Blast chiller",
                "equipment_type": "BLAST_FREEZER", "active": false,
            }),
            NOW + 60_000,
        )
    );
}

// domain「主数据」：内容没有变化的修改不写事件，命令照常成功并写入 processed_commands，响应为当前行。
#[tokio::test]
async fn unchanged_update_succeeds_without_an_event() {
    let node = node();
    let id = node.create_ok(&cmd(1), "F1").await;

    let reply = node
        .update(&id, &update_body(&cmd(2), 1, "Walk-in", "FRIDGE", true))
        .await;

    assert_eq!(
        assert_success(&reply),
        &json!({ "equipment": row(&id, "F1", "Walk-in", "FRIDGE", true, 1) })
    );
    assert_eq!(node.counts().unwrap(), (1, 2));
    // revision 未变，下一次修改仍以 1 为基准。
    let reply = node
        .update(&id, &update_body(&cmd(3), 1, "Walk-in 2", "FRIDGE", true))
        .await;
    assert_eq!(assert_success(&reply)["equipment"]["revision"], json!(2));
    assert_eq!(node.counts().unwrap(), (2, 3));
}

// domain「主数据」base_revision：与当前 revision 不等时 409 REVISION_CONFLICT，details 给 current_revision；
// 被拒绝的命令不落库，修正后可以用同一个 command_id 重提。
#[tokio::test]
async fn stale_base_revision_conflicts() {
    let node = node();
    let id = node.create_ok(&cmd(1), "F1").await;
    assert_success(
        &node
            .update(&id, &update_body(&cmd(2), 1, "Walk-in 2", "FRIDGE", true))
            .await,
    );

    for base_revision in [1, 3] {
        let reply = node
            .update(
                &id,
                &update_body(&cmd(3), base_revision, "Walk-in 3", "FRIDGE", true),
            )
            .await;
        assert_eq!(
            assert_error(&reply, 409, "REVISION_CONFLICT"),
            &json!({ "current_revision": 2 })
        );
    }
    assert_eq!(node.counts().unwrap(), (2, 2));

    let reply = node
        .update(&id, &update_body(&cmd(3), 2, "Walk-in 3", "FRIDGE", true))
        .await;
    assert_eq!(assert_success(&reply)["equipment"]["revision"], json!(3));
    assert_eq!(node.counts().unwrap(), (3, 3));
}

// domain「主数据」base_revision 规则不因内容未变而豁免：当前 revision 2，新命令提交与当前完全相同的内容、
// base_revision = 1，仍是 409 REVISION_CONFLICT，不写事件或 processed_commands。
#[tokio::test]
async fn stale_base_revision_conflicts_even_when_content_is_unchanged() {
    let node = node();
    let id = node.create_ok(&cmd(1), "F1").await;
    assert_success(
        &node
            .update(&id, &update_body(&cmd(2), 1, "Walk-in 2", "FRIDGE", true))
            .await,
    );

    let reply = node
        .update(&id, &update_body(&cmd(3), 1, "Walk-in 2", "FRIDGE", true))
        .await;

    assert_eq!(
        assert_error(&reply, 409, "REVISION_CONFLICT"),
        &json!({ "current_revision": 2 })
    );
    assert_eq!(node.counts().unwrap(), (2, 2));
}

// 设备接口「新建」：code 已被其他设备使用（含停用的）时 409 CODE_ALREADY_EXISTS，不落库。
#[tokio::test]
async fn duplicate_code_conflicts_even_with_inactive_equipment() {
    let node = node();
    let reply = node
        .create(&create_body(&cmd(1), "F1", "Old fridge", "FRIDGE", false))
        .await;
    let id = equipment_id(&reply);

    let reply = node
        .create(&create_body(&cmd(2), "F1", "New fridge", "FREEZER", true))
        .await;

    assert_eq!(
        assert_error(&reply, 409, "CODE_ALREADY_EXISTS"),
        &json!({ "code": "F1", "equipment_id": id })
    );
    assert_eq!(node.counts().unwrap(), (1, 1));
}

// 设备接口「修改」：设备不存在时 404 REFERENCE_NOT_FOUND，details 给 entity 和 id，不落库。
#[tokio::test]
async fn updating_unknown_equipment_is_not_found() {
    let node = node();
    node.create_ok(&cmd(1), "F1").await;

    let reply = node
        .update(
            UNKNOWN_ID,
            &update_body(&cmd(2), 1, "Walk-in", "FRIDGE", true),
        )
        .await;

    assert_eq!(
        assert_error(&reply, 404, "REFERENCE_NOT_FOUND"),
        &json!({ "entity": "EQUIPMENT", "id": UNKNOWN_ID })
    );
    assert_eq!(node.counts().unwrap(), (1, 1));
}

// 设备接口「取值」与 AGENTS「HTTP 约定」：字段缺失、未知字段、类型或取值不对、路径参数无法解析，一律 400 VALIDATION_FAILED，不落库。
#[tokio::test]
async fn invalid_commands_are_rejected_with_validation_failed() {
    let node = node();
    let id = node.create_ok(&cmd(1), "F1").await;
    let create = create_body(&cmd(2), "F2", "Walk-in", "FRIDGE", true);
    let update = update_body(&cmd(2), 1, "Walk-in", "FRIDGE", true);

    let with = |base: &Value, key: &str, value: Value| {
        let mut body = base.clone();
        body[key] = value;
        body
    };
    let without = |base: &Value, key: &str| {
        let mut body = base.clone();
        body.as_object_mut().unwrap().remove(key);
        body
    };

    let mut creates = vec![
        with(&create, "code", json!("")),
        with(&create, "code", json!(" F2")),
        with(&create, "code", json!("F2\t")),
        with(&create, "code", json!(2)),
        with(&create, "code", Value::Null),
        with(&create, "name", json!("")),
        with(&create, "name", json!("Walk-in ")),
        with(&create, "name", json!("\u{3000}冷藏柜")),
        with(&create, "name", json!("冷藏柜\n")),
        with(&create, "equipment_type", json!("fridge")),
        with(&create, "equipment_type", json!("COOLER")),
        with(&create, "active", json!("true")),
        with(&create, "active", json!(1)),
        with(&create, "command_id", json!("not-a-uuid")),
        with(
            &create,
            "command_id",
            json!("01890a5d-ac96-474b-bcce-b302099a8401"),
        ), // v4
        with(&create, "note", json!("unknown field")),
        with(&create, "base_revision", json!(1)),
    ];
    for key in ["command_id", "code", "name", "equipment_type", "active"] {
        creates.push(without(&create, key));
    }
    for body in &creates {
        let reply = node.create(body).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }

    let mut updates = vec![
        with(&update, "name", json!("")),
        with(&update, "name", json!(" Walk-in")),
        with(&update, "equipment_type", json!("OVEN ")),
        with(&update, "active", Value::Null),
        with(&update, "base_revision", json!(0)),
        with(&update, "base_revision", json!(-1)),
        with(&update, "base_revision", json!("1")),
        with(&update, "base_revision", json!(1.5)),
        with(&update, "code", json!("F1")), // 修改不接受 code
        with(
            &update,
            "command_id",
            json!("01890a5d-ac96-474b-bcce-b302099a8401"),
        ),
    ];
    for key in [
        "command_id",
        "base_revision",
        "name",
        "equipment_type",
        "active",
    ] {
        updates.push(without(&update, key));
    }
    for body in &updates {
        let reply = node.update(&id, body).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }
    for bad_id in ["not-a-uuid", "01890a5d-ac96-474b-bcce-b302099a8301"] {
        let reply = node.update(bad_id, &update).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }

    assert_eq!(node.counts().unwrap(), (1, 1));
}

// 设备接口「取值」：首尾以外的空白和非 ASCII 文本照常受理、原样保存。
#[tokio::test]
async fn inner_whitespace_and_unicode_are_kept_verbatim() {
    let node = node();

    let reply = node
        .create(&create_body(
            &cmd(1),
            "冷藏-01",
            "后厨 冷藏柜  1",
            "FRIDGE",
            true,
        ))
        .await;

    let id = equipment_id(&reply);
    assert_eq!(
        node.equipment_rows().unwrap(),
        [(
            id,
            "冷藏-01".into(),
            "后厨 冷藏柜  1".into(),
            "FRIDGE".into(),
            1,
            1
        )]
    );
}

// AGENTS「幂等」：同一 command_id、同一内容重发，原样返回首次的响应，不重复执行；
// 即使之后状态已改变（revision 已前进、时钟已变），也不重新计算、不做业务校验。
#[tokio::test]
async fn retries_return_the_original_response() {
    let node = node();
    let create = create_body(&cmd(1), "F1", "Walk-in", "FRIDGE", true);
    let first_create = node.create(&create).await;
    let id = equipment_id(&first_create);
    let update = update_body(&cmd(2), 1, "Walk-in 2", "FREEZER", true);
    let first_update = node.update(&id, &update).await;
    assert_success(&first_update);
    assert_success(
        &node
            .update(&id, &update_body(&cmd(3), 2, "Walk-in 3", "OVEN", false))
            .await,
    );
    let before = (
        node.events().unwrap(),
        node.equipment_rows().unwrap(),
        node.counts().unwrap(),
    );
    node.clock.advance(std::time::Duration::from_secs(3600));

    for _ in 0..2 {
        let reply = node.create(&create).await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, first_create.body);
        let reply = node.update(&id, &update).await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.body, first_update.body);
    }

    assert_eq!(
        (
            node.events().unwrap(),
            node.equipment_rows().unwrap(),
            node.counts().unwrap()
        ),
        before
    );
}

// AGENTS「幂等」规范化请求：比对的是强类型命令重新序列化的结果，不是原始文本。
// 同一命令换键顺序、加排版空白、把中文写成 \u 转义后重发，仍原样返回首次的响应，不入账。
#[tokio::test]
async fn retries_with_equivalent_json_text_return_the_original_response() {
    let node = node();
    let command_id = cmd(1);
    let first = node
        .create(&create_body(&command_id, "F1", "冷藏柜", "FRIDGE", true))
        .await;
    equipment_id(&first);
    let before = (
        node.events().unwrap(),
        node.equipment_rows().unwrap(),
        node.counts().unwrap(),
    );

    let texts = [
        format!(
            r#"{{"active":true,"equipment_type":"FRIDGE","name":"冷藏柜","code":"F1","command_id":"{command_id}"}}"#
        ),
        format!(
            "{{\n  \"command_id\" : \"{command_id}\",\n\t\"code\": \"F1\" ,\n  \"name\":\"冷藏柜\",\r\n  \"equipment_type\":\"FRIDGE\",  \"active\" :true\n}}\n"
        ),
        format!(
            r#"{{"command_id":"{command_id}","code":"F1","name":"\u51b7\u85cf\u67dc","equipment_type":"FRIDGE","active":true}}"#
        ),
    ];
    // 第三个样本必须真的以 \u 转义发送中文，不能是字面的中文。
    assert!(!texts[2].contains('冷'), "{}", texts[2]);

    for text in texts {
        let req = raw_request(
            Method::POST,
            "/api/v1/equipment",
            Some(&MANAGER),
            Some("application/json"),
            &text,
        )
        .unwrap();
        let reply = send(&node.router, req).await.unwrap();
        assert_eq!(reply.status, 200, "{text}");
        assert_eq!(reply.body, first.body, "{text}");
    }

    assert_eq!(
        (
            node.events().unwrap(),
            node.equipment_rows().unwrap(),
            node.counts().unwrap(),
        ),
        before
    );
}

// AGENTS「幂等」：无变化的修改成功后，其他命令推进了 revision；重试原来的无变化命令，
// 仍返回它首次成功时的行（旧 revision、旧内容），不报 REVISION_CONFLICT，不新增事件或命令记录。
#[tokio::test]
async fn retrying_an_unchanged_update_returns_its_original_response() {
    let node = node();
    let id = node.create_ok(&cmd(1), "F1").await;
    let unchanged = update_body(&cmd(2), 1, "Walk-in", "FRIDGE", true);
    let first = node.update(&id, &unchanged).await;
    assert_eq!(
        assert_success(&first),
        &json!({ "equipment": row(&id, "F1", "Walk-in", "FRIDGE", true, 1) })
    );
    assert_success(
        &node
            .update(&id, &update_body(&cmd(3), 1, "Walk-in 2", "OVEN", false))
            .await,
    );
    let before = (
        node.events().unwrap(),
        node.equipment_rows().unwrap(),
        node.counts().unwrap(),
    );
    assert_eq!(before.2, (2, 3));

    let reply = node.update(&id, &unchanged).await;

    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, first.body);
    assert_eq!(
        (
            node.events().unwrap(),
            node.equipment_rows().unwrap(),
            node.counts().unwrap(),
        ),
        before
    );
}

// AGENTS「幂等」与「HTTP 约定」：同一 command_id、内容不同时 409 IDEMPOTENCY_CONFLICT，
// details.fields 为不同的顶层字段名（字典序）；command_type 不同时只列 "command_type"；不落库。
#[tokio::test]
async fn idempotency_conflicts_list_differing_fields() {
    let node = node();
    let a = node.create_ok(&cmd(1), "F1").await;
    let b = node.create_ok(&cmd(2), "F2").await;
    let update_a = update_body(&cmd(3), 1, "Walk-in 2", "FRIDGE", true);
    assert_success(&node.update(&a, &update_a).await);
    let before = (
        node.events().unwrap(),
        node.equipment_rows().unwrap(),
        node.counts().unwrap(),
    );

    let cases: Vec<(JsonReply, Value)> = vec![
        (
            node.create(&create_body(&cmd(1), "F1", "Walk-in", "FRIDGE", false))
                .await,
            json!(["active"]),
        ),
        (
            node.create(&create_body(&cmd(1), "F9", "Reach-in", "OVEN", true))
                .await,
            json!(["code", "equipment_type", "name"]),
        ),
        // 同一 command_id 用于另一种命令：只列 command_type。
        (
            node.update(&a, &update_body(&cmd(1), 1, "Walk-in", "FRIDGE", true))
                .await,
            json!(["command_type"]),
        ),
        (
            node.create(&create_body(&cmd(3), "F3", "Walk-in 2", "FRIDGE", true))
                .await,
            json!(["command_type"]),
        ),
        // 路径参数是规范化请求的一部分。
        (node.update(&b, &update_a).await, json!(["equipment_id"])),
        (
            node.update(&a, &update_body(&cmd(3), 2, "Walk-in 2", "FRIDGE", true))
                .await,
            json!(["base_revision"]),
        ),
    ];
    for (reply, fields) in &cases {
        assert_eq!(
            assert_error(reply, 409, "IDEMPOTENCY_CONFLICT"),
            &json!({ "fields": fields })
        );
    }

    assert_eq!(
        (
            node.events().unwrap(),
            node.equipment_rows().unwrap(),
            node.counts().unwrap()
        ),
        before
    );
}

// AGENTS「HTTP 约定」处理顺序：请求结构与取值校验先于幂等检查——已成功的 command_id 携带非法内容重发时是 400，不是 409。
#[tokio::test]
async fn validation_precedes_the_idempotency_check() {
    let node = node();
    node.create_ok(&cmd(1), "F1").await;

    let reply = node
        .create(&create_body(&cmd(1), "", "Walk-in", "FRIDGE", true))
        .await;

    assert_error(&reply, 400, "VALIDATION_FAILED");
    assert_eq!(node.counts().unwrap(), (1, 1));
}

// 设备接口「查询」：含停用的设备，按 code 的字节序升序；没有设备时为空数组。
#[tokio::test]
async fn list_returns_all_equipment_ordered_by_code_bytes() {
    let node = node();
    assert_eq!(node.list().await, json!({ "equipment": [] }));

    let b2 = node.create_ok(&cmd(1), "B2").await;
    let lower = node.create_ok(&cmd(2), "a0").await;
    let a1 = node.create_ok(&cmd(3), "A1").await;
    let a10 = node.create_ok(&cmd(4), "A10").await;
    assert_success(
        &node
            .update(&b2, &update_body(&cmd(5), 1, "Old", "MIXER", false))
            .await,
    );

    assert_eq!(
        node.list().await,
        json!({ "equipment": [
            row(&a1, "A1", "Walk-in", "FRIDGE", true, 1),
            row(&a10, "A10", "Walk-in", "FRIDGE", true, 1),
            row(&b2, "B2", "Old", "MIXER", false, 2),
            row(&lower, "a0", "Walk-in", "FRIDGE", true, 1),
        ] })
    );
}

// domain「主数据」：命令引用已停用的主数据照常受理——停用的设备可以修改，也可以重新启用。
#[tokio::test]
async fn inactive_equipment_can_be_updated_and_reactivated() {
    let node = node();
    let id = node.create_ok(&cmd(1), "F1").await;
    assert_success(
        &node
            .update(&id, &update_body(&cmd(2), 1, "Walk-in", "FRIDGE", false))
            .await,
    );

    let reply = node
        .update(&id, &update_body(&cmd(3), 2, "Walk-in", "FRIDGE", true))
        .await;

    assert_eq!(
        assert_success(&reply),
        &json!({ "equipment": row(&id, "F1", "Walk-in", "FRIDGE", true, 3) })
    );
}

// 查询走读连接：不写事件，也不写 processed_commands。
#[tokio::test]
async fn queries_do_not_write() {
    let node = node();
    node.create_ok(&cmd(1), "F1").await;

    for _ in 0..3 {
        node.list().await;
        let reply = send(
            &node.router,
            request(Method::GET, "/api/v1/equipment", Some(&MANAGER), None).unwrap(),
        )
        .await
        .unwrap();
        assert_success(&reply);
    }

    assert_eq!(node.counts().unwrap(), (1, 1));
}
