//! 锁定测试：物料、配方、供应商、报损原因四个实体共用的主数据接口行为，以及供应商、报损原因特有的字段规则。
//! 规则见 docs/domain.md「主数据」「主数据接口」「时间」，AGENTS.md「幂等」「HTTP 约定」「ID 与时间」。
//! 物料与配方特有的规则见 spec_items.rs、spec_recipes.rs。

mod spec_support;

use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use axum::Router;
use axum::http::Method;
use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use serde_json::{Map, Value, json};
use spec_support::{
    JsonReply, MANAGER, STAFF, assert_error, assert_success, is_uuid_v7, non_canonical_uuids,
    raw_request, request, send,
};
use tempfile::TempDir;

const NOW: i64 = 1_791_248_400_000; // 2026-10-06 09:00 +08:00
const CUTOFF_0400: i64 = 1_791_230_400_000; // 2026-10-06 04:00 +08:00
const UNKNOWN_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8399";

type Rows<T> = Result<T, Box<dyn Error>>;
/// 实体主表的公共列：(id, code, name, active, revision)。
type CommonRow = (String, String, String, i64, i64);

/// 配方引用的两个物料，在配方的测试节点上先建好。
#[derive(Default)]
struct Refs {
    flour: String,
    toast: String,
}

/// 一个主数据实体的接口约定（docs/domain.md「主数据接口」表）与两份内容样本。
struct Entity {
    name: &'static str,
    path: &'static str,
    table: &'static str,
    id_key: &'static str,
    one: &'static str,
    many: &'static str,
    prefix: &'static str,
    /// 样本 A：新建请求体中 `command_id`、`code` 以外的字段，及对应的快照字段（不含 `code`）。
    created: fn(&Refs) -> (Value, Value),
    /// 样本 B：修改请求体中 `command_id`、`base_revision` 以外的字段，及修改后的快照字段（不含 `code`）。与 A 不同。
    updated: fn(&Refs) -> (Value, Value),
    /// 快照与样本 A 相同的修改请求字段。
    unchanged: fn(&Refs) -> Value,
}

const ITEM: Entity = Entity {
    name: "ITEM",
    path: "/api/v1/items",
    table: "items",
    id_key: "item_id",
    one: "item",
    many: "items",
    prefix: "item",
    created: |_| {
        let fields = json!({
            "name": "高筋面粉", "base_unit": "g", "category": "RAW",
            "default_shelf_life_ms": 15_552_000_000_i64,
            "units": [{ "unit_code": "bag", "base_qty_per_unit": 25000 }],
            "active": true,
        });
        (fields.clone(), fields)
    },
    updated: |_| {
        let units = json!([
            { "unit_code": "bag", "base_qty_per_unit": 20000 },
            { "unit_code": "box", "base_qty_per_unit": 1000 },
        ]);
        (
            json!({ "name": "高筋面粉 T65", "category": "SEMI", "units": units, "active": false }),
            json!({
                "name": "高筋面粉 T65", "base_unit": "g", "category": "SEMI", "units": units,
                "active": false,
            }),
        )
    },
    unchanged: |_| {
        json!({
            "name": "高筋面粉", "category": "RAW", "default_shelf_life_ms": 15_552_000_000_i64,
            "units": [{ "unit_code": "bag", "base_qty_per_unit": 25000 }],
            "active": true,
        })
    },
};

const RECIPE: Entity = Entity {
    name: "RECIPE",
    path: "/api/v1/recipes",
    table: "recipes",
    id_key: "recipe_id",
    one: "recipe",
    many: "recipes",
    prefix: "recipe",
    created: |refs| {
        (
            json!({
                "name": "吐司", "output_item_id": refs.toast, "active": true,
                "output_qty_per_batch": 12,
                "lines": [{ "item_id": refs.flour, "qty_per_batch": 3000 }],
            }),
            recipe_snapshot(refs, "吐司", true),
        )
    },
    updated: |refs| {
        (
            json!({ "name": "白吐司", "active": false }),
            recipe_snapshot(refs, "白吐司", false),
        )
    },
    unchanged: |_| json!({ "name": "吐司", "active": true }),
};

const SUPPLIER: Entity = Entity {
    name: "SUPPLIER",
    path: "/api/v1/suppliers",
    table: "suppliers",
    id_key: "supplier_id",
    one: "supplier",
    many: "suppliers",
    prefix: "supplier",
    created: |_| {
        let fields =
            json!({ "name": "面粉供应商", "contact_phone": "021-5555 0101", "active": true });
        (fields.clone(), fields)
    },
    // 省略 contact_phone：修改后的快照没有该键。
    updated: |_| {
        let fields = json!({ "name": "面粉供应商 B", "active": false });
        (fields.clone(), fields)
    },
    unchanged: |_| json!({ "name": "面粉供应商", "contact_phone": "021-5555 0101", "active": true }),
};

const WASTE_REASON: Entity = Entity {
    name: "WASTE_REASON",
    path: "/api/v1/waste-reasons",
    table: "waste_reasons",
    id_key: "waste_reason_id",
    one: "waste_reason",
    many: "waste_reasons",
    prefix: "waste_reason",
    created: |_| {
        let fields = json!({ "name": "过期", "active": true });
        (fields.clone(), fields)
    },
    updated: |_| {
        let fields = json!({ "name": "超过保质期", "active": false });
        (fields.clone(), fields)
    },
    unchanged: |_| json!({ "name": "过期", "active": true }),
};

const ENTITIES: [&Entity; 4] = [&ITEM, &RECIPE, &SUPPLIER, &WASTE_REASON];

fn recipe_snapshot(refs: &Refs, name: &str, active: bool) -> Value {
    json!({
        "name": name, "output_item_id": refs.toast,
        "versions": [{
            "version": 1, "output_qty_per_batch": 12,
            "lines": [{ "item_id": refs.flour, "qty_per_batch": 3000 }],
        }],
        "active": active,
    })
}

/// 第 `n` 个命令 ID（UUIDv7）。
fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209ae{n:04x}")
}

/// 把 `extra` 的键并入 `base`（同名覆盖）。
fn merge(base: Value, extra: &Value) -> Value {
    let mut map: Map<String, Value> = base.as_object().cloned().unwrap_or_default();
    for (key, value) in extra.as_object().into_iter().flatten() {
        map.insert(key.clone(), value.clone());
    }
    Value::Object(map)
}

struct Node {
    _dir: TempDir,
    db_path: PathBuf,
    clock: ManualClock,
    router: Router,
    entity: &'static Entity,
    refs: Refs,
    /// 准备引用数据之后的 (`store_events` 行数, `processed_commands` 行数)。
    base: (i64, i64),
}

#[allow(clippy::unwrap_used)] // 测试夹具：临时目录、Router 或引用数据构造失败时测试无法开始，直接终止。
async fn node(entity: &'static Entity) -> Node {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(UnixMillis(NOW));
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let mut refs = Refs::default();
    if entity.name == "RECIPE" {
        for (n, code) in [(0xff01, "REF_FLOUR"), (0xff02, "REF_TOAST")] {
            let reply = spec_support::post(
                &router,
                "/api/v1/items",
                Some(&MANAGER),
                &json!({
                    "command_id": cmd(n), "code": code, "name": code, "base_unit": "g",
                    "category": "RAW", "units": [], "active": true,
                }),
            )
            .await
            .unwrap();
            let id = assert_success(&reply)["item"]["item_id"]
                .as_str()
                .unwrap()
                .to_owned();
            if code == "REF_FLOUR" {
                refs.flour = id;
            } else {
                refs.toast = id;
            }
        }
    }
    let mut node = Node {
        _dir: dir,
        db_path,
        clock,
        router,
        entity,
        refs,
        base: (0, 0),
    };
    node.base = node.counts().unwrap();
    node
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

impl Node {
    fn create_body(&self, command_id: &str, code: &str) -> Value {
        let (fields, _) = (self.entity.created)(&self.refs);
        merge(json!({ "command_id": command_id, "code": code }), &fields)
    }

    fn update_body(&self, command_id: &str, base_revision: i64) -> Value {
        let (fields, _) = (self.entity.updated)(&self.refs);
        merge(
            json!({ "command_id": command_id, "base_revision": base_revision }),
            &fields,
        )
    }

    fn unchanged_body(&self, command_id: &str, base_revision: i64) -> Value {
        merge(
            json!({ "command_id": command_id, "base_revision": base_revision }),
            &(self.entity.unchanged)(&self.refs),
        )
    }

    /// 样本 A（`updated = false`）或样本 B 的行。
    fn row(&self, id: &str, code: &str, updated: bool, revision: i64) -> Value {
        let snapshot = if updated {
            (self.entity.updated)(&self.refs).1
        } else {
            (self.entity.created)(&self.refs).1
        };
        merge(
            json!({ self.entity.id_key: id, "code": code, "revision": revision }),
            &snapshot,
        )
    }

    fn payload(&self, code: &str, updated: bool) -> Value {
        let snapshot = if updated {
            (self.entity.updated)(&self.refs).1
        } else {
            (self.entity.created)(&self.refs).1
        };
        json!({
            "entity": self.entity.name,
            "source": "LOCAL",
            "snapshot": merge(json!({ "code": code }), &snapshot),
        })
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试；状态码和错误码由调用方断言。
    async fn create(&self, body: &Value) -> JsonReply {
        spec_support::post(&self.router, self.entity.path, Some(&MANAGER), body)
            .await
            .unwrap()
    }

    #[allow(clippy::unwrap_used)] // 同上。
    async fn update(&self, id: &str, body: &Value) -> JsonReply {
        spec_support::put(
            &self.router,
            &format!("{}/{id}", self.entity.path),
            Some(&MANAGER),
            body,
        )
        .await
        .unwrap()
    }

    #[allow(clippy::unwrap_used)] // 同上。
    async fn list(&self) -> Value {
        let reply = spec_support::get(&self.router, self.entity.path, Some(&STAFF))
            .await
            .unwrap();
        assert_eq!(reply.body["warnings"], json!([]));
        assert_success(&reply).clone()
    }

    /// 用样本 A 新建一行，返回它的 ID。
    async fn create_ok(&self, command_id: &str, code: &str) -> String {
        let reply = self.create(&self.create_body(command_id, code)).await;
        self.id(&reply)
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：成功响应缺少 ID 已违反接口约定，直接终止测试。
    fn id(&self, reply: &JsonReply) -> String {
        let id = assert_success(reply)[self.entity.one][self.entity.id_key]
            .as_str()
            .unwrap_or_else(|| panic!("{}: {}", self.entity.name, reply.body))
            .to_owned();
        assert!(is_uuid_v7(&id), "{id}");
        id
    }

    /// (`store_events` 行数, `processed_commands` 行数)。
    fn counts(&self) -> Rows<(i64, i64)> {
        let reader = spec_support::reader(&self.db_path)?;
        Ok((
            spec_support::count(&reader, "store_events")?,
            spec_support::count(&reader, "processed_commands")?,
        ))
    }

    /// 准备引用数据之后新增的 (事件数, 命令数)。
    fn added(&self) -> Rows<(i64, i64)> {
        let (events, commands) = self.counts()?;
        Ok((events - self.base.0, commands - self.base.1))
    }

    /// 准备引用数据之后写入的事件。
    fn events(&self) -> Rows<Vec<Event>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT event_type, schema_version, aggregate_type, aggregate_id, aggregate_version,
                    command_id, actor_id, device_id, business_date, occurred_at, recorded_at, payload
             FROM store_events WHERE seq > ?1 ORDER BY seq",
        )?;
        let rows = statement
            .query_map([self.base.0], |r| {
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

    /// 实体主表的公共列：(id, code, name, active, revision)，按 code 排序。
    fn table_rows(&self) -> Rows<Vec<CommonRow>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(&format!(
            "SELECT id, code, name, active, revision FROM {} ORDER BY code",
            self.entity.table
        ))?;
        let rows = statement
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
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

    #[allow(clippy::unwrap_used)] // 测试夹具：读取失败本身就是断言失败。
    fn state(&self) -> (Vec<Event>, (i64, i64)) {
        (self.events().unwrap(), self.counts().unwrap())
    }
}

fn event(node: &Node, id: &str, version: i64, command_id: &str, at: i64, payload: Value) -> Event {
    Event {
        event_type: "MASTER_DATA_CHANGED".into(),
        schema_version: 1,
        aggregate_type: node.entity.name.into(),
        aggregate_id: id.into(),
        aggregate_version: version,
        command_id: command_id.into(),
        actor_id: MANAGER.employee_id.into(),
        device_id: MANAGER.device_id.into(),
        business_date: "2026-10-06".into(),
        occurred_at: at,
        recorded_at: at,
        payload,
    }
}

// 主数据接口「新建」：写一条 aggregate_version 1 的 MASTER_DATA_CHANGED（payload 为完整快照，source = LOCAL）、
// 主表一行和 processed_commands；身份取自开发桩；occurred_at = recorded_at，营业日按门店时区计算。
// 响应与查询的行是 ID 字段、快照全部字段和 revision。
#[tokio::test]
async fn create_writes_event_projection_and_command() {
    for entity in ENTITIES {
        let node = node(entity).await;
        let body = node.create_body(&cmd(1), "C1");

        let reply = node.create(&body).await;

        let id = node.id(&reply);
        let row = node.row(&id, "C1", false, 1);
        assert_eq!(
            reply.body,
            json!({ "success": true, "data": { entity.one: row }, "warnings": [], "error": null }),
            "{}",
            entity.name
        );
        assert_eq!(
            node.events().unwrap(),
            [event(
                &node,
                &id,
                1,
                &cmd(1),
                NOW,
                node.payload("C1", false)
            )],
            "{}",
            entity.name
        );
        let name = (entity.created)(&node.refs).1["name"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            node.table_rows().unwrap(),
            [(id.clone(), "C1".into(), name, 1, 1)],
            "{}",
            entity.name
        );
        let mut request = body.clone();
        request.as_object_mut().unwrap().remove("command_id");
        assert_eq!(
            node.processed(&cmd(1)).unwrap(),
            (format!("{}.create", entity.prefix), request, NOW),
            "{}",
            entity.name
        );
        assert_eq!(
            node.list().await,
            json!({ entity.many: [row] }),
            "{}",
            entity.name
        );
    }
}

// 主数据接口「修改」：revision +1，code 和不可修改的字段取当前值，事件 aggregate_version 等于新 revision；
// 规范化请求含路径参数（ID 字段），不含 command_id。
#[tokio::test]
async fn update_writes_next_revision_and_keeps_code() {
    for entity in ENTITIES {
        let node = node(entity).await;
        let id = node.create_ok(&cmd(1), "C1").await;
        node.clock.advance(Duration::from_secs(60));
        let body = node.update_body(&cmd(2), 1);

        let reply = node.update(&id, &body).await;

        let row = node.row(&id, "C1", true, 2);
        assert_eq!(
            assert_success(&reply),
            &json!({ entity.one: row }),
            "{}",
            entity.name
        );
        assert_eq!(reply.body["warnings"], json!([]));
        let events = node.events().unwrap();
        assert_eq!(events.len(), 2, "{}", entity.name);
        assert_eq!(
            events[1],
            event(
                &node,
                &id,
                2,
                &cmd(2),
                NOW + 60_000,
                node.payload("C1", true)
            ),
            "{}",
            entity.name
        );
        let mut request = body.clone();
        request.as_object_mut().unwrap().remove("command_id");
        request[entity.id_key] = json!(id);
        assert_eq!(
            node.processed(&cmd(2)).unwrap(),
            (format!("{}.update", entity.prefix), request, NOW + 60_000),
            "{}",
            entity.name
        );
        assert_eq!(
            node.list().await,
            json!({ entity.many: [row] }),
            "{}",
            entity.name
        );
    }
}

// domain「主数据」：内容没有变化的修改不写事件，命令照常成功并写入 processed_commands，响应为当前行；
// revision 不变，下一次修改仍以原 revision 为基准。
#[tokio::test]
async fn unchanged_update_succeeds_without_an_event() {
    for entity in ENTITIES {
        let node = node(entity).await;
        let id = node.create_ok(&cmd(1), "C1").await;

        let reply = node.update(&id, &node.unchanged_body(&cmd(2), 1)).await;

        assert_eq!(
            assert_success(&reply),
            &json!({ entity.one: node.row(&id, "C1", false, 1) }),
            "{}",
            entity.name
        );
        assert_eq!(node.added().unwrap(), (1, 2), "{}", entity.name);
        let reply = node.update(&id, &node.update_body(&cmd(3), 1)).await;
        assert_eq!(assert_success(&reply)[entity.one]["revision"], json!(2));
        assert_eq!(node.added().unwrap(), (2, 3), "{}", entity.name);
    }
}

// domain「主数据」base_revision：与当前 revision 不等时 409 REVISION_CONFLICT，details 给 current_revision，
// 内容未变也不豁免；不落库，修正后可以用同一个 command_id 重提。
#[tokio::test]
async fn stale_base_revision_conflicts() {
    for entity in ENTITIES {
        let node = node(entity).await;
        let id = node.create_ok(&cmd(1), "C1").await;
        assert_success(&node.update(&id, &node.update_body(&cmd(2), 1)).await);
        let before = node.state();

        for body in [
            node.update_body(&cmd(3), 1),
            node.update_body(&cmd(3), 3),
            node.unchanged_body(&cmd(3), 1),
        ] {
            let reply = node.update(&id, &body).await;
            assert_eq!(
                assert_error(&reply, 409, "REVISION_CONFLICT"),
                &json!({ "current_revision": 2 }),
                "{}",
                entity.name
            );
        }
        assert_eq!(node.state(), before, "{}", entity.name);

        let reply = node.update(&id, &node.unchanged_body(&cmd(3), 2)).await;
        assert_eq!(assert_success(&reply)[entity.one]["revision"], json!(3));
    }
}

// 主数据接口「新建」：code 已被同一实体的其他行使用（含停用的）时 409 CODE_ALREADY_EXISTS，
// details 给 code 和占用它的行的 ID；不落库。
#[tokio::test]
async fn duplicate_code_conflicts_even_with_inactive_rows() {
    for entity in ENTITIES {
        let node = node(entity).await;
        let id = node.create_ok(&cmd(1), "C1").await;
        assert_success(&node.update(&id, &node.update_body(&cmd(2), 1)).await); // 样本 B 停用
        let before = node.state();

        let reply = node.create(&node.create_body(&cmd(3), "C1")).await;

        assert_eq!(
            assert_error(&reply, 409, "CODE_ALREADY_EXISTS"),
            &json!({ "code": "C1", entity.id_key: id }),
            "{}",
            entity.name
        );
        assert_eq!(node.state(), before, "{}", entity.name);

        // AGENTS「幂等」：被业务校验拒绝的命令不落库，修正后用同一个 command_id 重提成功。
        let reply = node.create(&node.create_body(&cmd(3), "C2")).await;
        let row = &assert_success(&reply)[entity.one];
        assert_eq!(row["code"], json!("C2"), "{}", entity.name);
        assert_eq!(row["revision"], json!(1), "{}", entity.name);
    }
}

// domain「主数据」：code 只在同一实体内唯一，不同实体（含设备）可以使用相同的 code。
#[tokio::test]
async fn the_same_code_may_be_used_by_different_entities() {
    let node = node(&RECIPE).await;
    for (n, entity) in (0x10..).zip(ENTITIES) {
        let (fields, _) = (entity.created)(&node.refs);
        let body = merge(json!({ "command_id": cmd(n), "code": "SHARED" }), &fields);
        let reply = spec_support::post(&node.router, entity.path, Some(&MANAGER), &body)
            .await
            .unwrap();
        assert_eq!(
            assert_success(&reply)[entity.one]["code"],
            json!("SHARED"),
            "{}",
            entity.name
        );
    }
    let reply = spec_support::post(
        &node.router,
        "/api/v1/equipment",
        Some(&MANAGER),
        &json!({
            "command_id": cmd(0x20), "code": "SHARED", "name": "Walk-in",
            "equipment_type": "FRIDGE", "active": true,
        }),
    )
    .await
    .unwrap();
    assert_eq!(assert_success(&reply)["equipment"]["code"], json!("SHARED"));
}

// domain「时间」营业日：主数据命令 occurred_at = recorded_at；Asia/Shanghai 当地 04:00 之前归前一营业日，恰好 04:00 归当日。
#[tokio::test]
async fn business_date_follows_store_timezone_and_cutoff() {
    for entity in ENTITIES {
        let node = node(entity).await;
        node.clock.set(UnixMillis(CUTOFF_0400 - 1));
        node.create_ok(&cmd(1), "C1").await;
        node.clock.set(UnixMillis(CUTOFF_0400));
        node.create_ok(&cmd(2), "C2").await;

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
            ],
            "{}",
            entity.name
        );
    }
}

// 主数据接口「修改」：行不存在时 404 REFERENCE_NOT_FOUND，details 给 entity 和 id；不落库。
#[tokio::test]
async fn updating_an_unknown_row_is_not_found() {
    for entity in ENTITIES {
        let node = node(entity).await;
        node.create_ok(&cmd(1), "C1").await;
        let before = node.state();

        let reply = node.update(UNKNOWN_ID, &node.update_body(&cmd(2), 1)).await;

        assert_eq!(
            assert_error(&reply, 404, "REFERENCE_NOT_FOUND"),
            &json!({ "entity": entity.name, "id": UNKNOWN_ID }),
            "{}",
            entity.name
        );
        assert_eq!(node.state(), before, "{}", entity.name);
    }
}

// 主数据接口「取值」与 AGENTS「HTTP 约定」「ID 与时间」：code / name 为空或首尾有空白、类型不对、字段缺失、
// 未知字段（新建带 base_revision、修改带 code）、command_id 不是规范形式的 UUIDv7、base_revision 不是正整数、
// 路径参数无法解析或不是规范形式、请求体无法解析，一律 400 VALIDATION_FAILED，不落库。
#[tokio::test]
async fn invalid_commands_are_rejected_with_validation_failed() {
    for entity in ENTITIES {
        let node = node(entity).await;
        let id = node.create_ok(&cmd(1), "C1").await;
        let create = node.create_body(&cmd(2), "C2");
        let update = node.update_body(&cmd(2), 1);
        let before = node.state();

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
            with(&create, "code", json!(" C2")),
            with(&create, "code", json!("C2\t")),
            with(&create, "code", json!(2)),
            with(&create, "code", Value::Null),
            with(&create, "name", json!("")),
            with(&create, "name", json!("名称 ")),
            with(&create, "name", json!("\u{3000}名称")),
            with(&create, "name", json!("名称\n")),
            with(&create, "name", Value::Null),
            with(&create, "active", json!("true")),
            with(&create, "active", json!(1)),
            with(&create, "command_id", json!("not-a-uuid")),
            with(
                &create,
                "command_id",
                json!("01890a5d-ac96-474b-bcce-b302099a8401"),
            ), // v4
            with(
                &create,
                "command_id",
                json!("01890a5d-ac96-774b-0cce-b302099a8401"),
            ), // 变体位 0
            with(
                &create,
                "command_id",
                json!("01890a5d-ac96-774b-ccce-b302099a8401"),
            ), // 变体位 c
            with(&create, "base_revision", json!(1)),
            with(&create, "note", json!("unknown field")),
        ];
        for text in non_canonical_uuids(&cmd(2)) {
            creates.push(with(&create, "command_id", json!(text)));
        }
        for key in ["command_id", "code", "name", "active"] {
            creates.push(without(&create, key));
        }
        for body in &creates {
            let reply = node.create(body).await;
            assert_error(&reply, 400, "VALIDATION_FAILED");
        }

        let mut updates = vec![
            with(&update, "name", json!("")),
            with(&update, "name", json!(" 名称")),
            with(&update, "active", Value::Null),
            with(&update, "base_revision", json!(0)),
            with(&update, "base_revision", json!(-1)),
            with(&update, "base_revision", json!("1")),
            with(&update, "base_revision", json!(1.5)),
            with(&update, "code", json!("C1")),
            with(&update, entity.id_key, json!(id)),
        ];
        for text in non_canonical_uuids(&cmd(2)) {
            updates.push(with(&update, "command_id", json!(text)));
        }
        for key in ["command_id", "base_revision", "name", "active"] {
            updates.push(without(&update, key));
        }
        for body in &updates {
            let reply = node.update(&id, body).await;
            assert_error(&reply, 400, "VALIDATION_FAILED");
        }
        let mut bad_ids = vec![
            "not-a-uuid".to_owned(),
            "01890a5d-ac96-474b-bcce-b302099a8301".to_owned(), // v4
            "01890a5d-ac96-774b-0cce-b302099a8301".to_owned(), // 变体位 0
            "01890a5d-ac96-774b-ccce-b302099a8301".to_owned(), // 变体位 c
        ];
        for text in non_canonical_uuids(&id) {
            bad_ids.push(text.replace('{', "%7B").replace('}', "%7D"));
        }
        for bad_id in &bad_ids {
            let reply = node.update(bad_id, &update).await;
            assert_error(&reply, 400, "VALIDATION_FAILED");
        }

        let json = Some("application/json");
        let create_text = create.to_string();
        let update_text = update.to_string();
        let update_uri = format!("{}/{id}", entity.path);
        for (method, uri, text, content_type) in [
            (Method::POST, entity.path, "{", json),
            (Method::POST, entity.path, "[]", json),
            (Method::POST, entity.path, create_text.as_str(), None),
            (
                Method::POST,
                entity.path,
                create_text.as_str(),
                Some("text/plain"),
            ),
            (Method::PUT, update_uri.as_str(), "", json),
            (Method::PUT, update_uri.as_str(), update_text.as_str(), None),
        ] {
            let req = raw_request(method, uri, Some(&MANAGER), content_type, text).unwrap();
            let reply = send(&node.router, req).await.unwrap();
            assert_error(&reply, 400, "VALIDATION_FAILED");
        }

        assert_eq!(node.state(), before, "{}", entity.name);
    }
}

// AGENTS「幂等」：同一 command_id、同一内容重发，原样返回首次的响应，不重复执行；即使之后状态已改变
// （revision 已前进、时钟已变）也不重新计算、不做业务校验。键顺序不同的同义 JSON 文本同样视为同一内容。
#[tokio::test]
async fn retries_return_the_original_response() {
    for entity in ENTITIES {
        let node = node(entity).await;
        let create = node.create_body(&cmd(1), "C1");
        let first_create = node.create(&create).await;
        let id = node.id(&first_create);
        let update = node.update_body(&cmd(2), 1);
        let first_update = node.update(&id, &update).await;
        assert_success(&first_update);
        assert_success(&node.update(&id, &node.unchanged_body(&cmd(3), 2)).await);
        let before = node.state();
        node.clock.advance(Duration::from_secs(3600));

        let reversed = |body: &Value| -> String {
            let pairs: Vec<String> = body
                .as_object()
                .unwrap()
                .iter()
                .rev()
                .map(|(key, value)| format!("{}:{value}", json!(key)))
                .collect();
            format!("{{ {} }}", pairs.join(" , "))
        };
        for _ in 0..2 {
            let reply = node.create(&create).await;
            assert_eq!(
                (reply.status.as_u16(), &reply.body),
                (200, &first_create.body)
            );
            let reply = node.update(&id, &update).await;
            assert_eq!(
                (reply.status.as_u16(), &reply.body),
                (200, &first_update.body)
            );
        }
        let req = raw_request(
            Method::POST,
            entity.path,
            Some(&MANAGER),
            Some("application/json"),
            &reversed(&create),
        )
        .unwrap();
        let reply = send(&node.router, req).await.unwrap();
        assert_eq!(
            (reply.status.as_u16(), &reply.body),
            (200, &first_create.body)
        );

        assert_eq!(node.state(), before, "{}", entity.name);
    }
}

// AGENTS「幂等」与「HTTP 约定」：同一 command_id、内容不同时 409 IDEMPOTENCY_CONFLICT，details.fields 为
// 不同的顶层字段名（字典序，含路径参数）；command_type 不同时只列 "command_type"。幂等检查先于业务校验。不落库。
#[tokio::test]
async fn idempotency_conflicts_list_differing_fields() {
    for entity in ENTITIES {
        let node = node(entity).await;
        let a = node.create_ok(&cmd(1), "C1").await;
        let b = node.create_ok(&cmd(2), "C2").await;
        let update_a = node.update_body(&cmd(3), 1);
        assert_success(&node.update(&a, &update_a).await);
        let before = node.state();

        let mut renamed = node.create_body(&cmd(1), "C1");
        renamed["name"] = json!("另一个名称");
        let mut other_revision = update_a.clone();
        other_revision["base_revision"] = json!(2);
        let cases: Vec<(JsonReply, Value)> = vec![
            (node.create(&renamed).await, json!(["name"])),
            (
                node.create(&node.create_body(&cmd(1), "C9")).await,
                json!(["code"]),
            ),
            (
                node.update(&a, &node.update_body(&cmd(1), 1)).await,
                json!(["command_type"]),
            ),
            (
                node.create(&node.create_body(&cmd(3), "C3")).await,
                json!(["command_type"]),
            ),
            (node.update(&b, &update_a).await, json!([entity.id_key])),
            (
                node.update(&a, &other_revision).await,
                json!(["base_revision"]),
            ),
            // 业务上会 404 的修改：幂等检查在前，仍是 409。
            (
                node.update(UNKNOWN_ID, &update_a).await,
                json!([entity.id_key]),
            ),
        ];
        for (reply, fields) in &cases {
            assert_eq!(
                assert_error(reply, 409, "IDEMPOTENCY_CONFLICT"),
                &json!({ "fields": fields }),
                "{}",
                entity.name
            );
        }

        assert_eq!(node.state(), before, "{}", entity.name);
    }
}

// AGENTS「HTTP 约定」处理顺序：请求结构与取值校验先于幂等检查——已成功的 command_id 携带非法内容重发时是 400，不是 409。
#[tokio::test]
async fn validation_precedes_the_idempotency_check() {
    for entity in ENTITIES {
        let node = node(entity).await;
        node.create_ok(&cmd(1), "C1").await;
        let before = node.state();

        let reply = node.create(&node.create_body(&cmd(1), "")).await;

        assert_error(&reply, 400, "VALIDATION_FAILED");
        assert_eq!(node.state(), before, "{}", entity.name);
    }
}

// 主数据接口「查询」：含停用的行，按 code 的字节序升序（大写在小写之前，"A1" 在 "A10" 之前）；没有行时为空数组。
// ITEM 的 code 不能有小写字母（domain「主数据」），用 "_0" 代替 "a0"：'_' 同样排在大写字母之后。
#[tokio::test]
async fn list_returns_all_rows_ordered_by_code_bytes() {
    for entity in ENTITIES {
        let node = node(entity).await;
        assert_eq!(
            node.list().await,
            json!({ entity.many: [] }),
            "{}",
            entity.name
        );

        let b2 = node.create_ok(&cmd(1), "B2").await;
        let last_code = if entity.name == "ITEM" { "_0" } else { "a0" };
        let lower = node.create_ok(&cmd(2), last_code).await;
        let a1 = node.create_ok(&cmd(3), "A1").await;
        let a10 = node.create_ok(&cmd(4), "A10").await;
        assert_success(&node.update(&b2, &node.update_body(&cmd(5), 1)).await);

        assert_eq!(
            node.list().await,
            json!({ entity.many: [
                node.row(&a1, "A1", false, 1),
                node.row(&a10, "A10", false, 1),
                node.row(&b2, "B2", true, 2),
                node.row(&lower, last_code, false, 1),
            ] }),
            "{}",
            entity.name
        );
    }
}

// domain「主数据」与「员工认证」开发桩：写接口只允许 MANAGER（STAFF 为 403 FORBIDDEN），查询任何已认证员工可用；
// 没有身份时一律 401 UNAUTHENTICATED。被拒绝的请求不落库。
#[tokio::test]
async fn writes_require_manager_and_all_calls_require_identity() {
    for entity in ENTITIES {
        let node = node(entity).await;
        let id = node.create_ok(&cmd(1), "C1").await;
        let before = node.state();
        let update_uri = format!("{}/{id}", entity.path);
        let calls = [
            (
                Method::POST,
                entity.path,
                Some(node.create_body(&cmd(2), "C2")),
            ),
            (
                Method::PUT,
                update_uri.as_str(),
                Some(node.update_body(&cmd(3), 1)),
            ),
            (Method::GET, entity.path, None),
        ];

        for (method, uri, body) in &calls {
            let reply = send(
                &node.router,
                request(method.clone(), uri, None, body.as_ref()).unwrap(),
            )
            .await
            .unwrap();
            assert_error(&reply, 401, "UNAUTHENTICATED");
            let reply = send(
                &node.router,
                request(method.clone(), uri, Some(&STAFF), body.as_ref()).unwrap(),
            )
            .await
            .unwrap();
            if *method == Method::GET {
                assert_success(&reply);
            } else {
                assert_error(&reply, 403, "FORBIDDEN");
            }
        }

        assert_eq!(node.state(), before, "{}", entity.name);
    }
}

// AGENTS「HTTP 约定」：路由存在但方法不允许时 405 METHOD_NOT_ALLOWED；查询走读连接，不写任何东西。
#[tokio::test]
async fn wrong_methods_are_rejected_and_queries_do_not_write() {
    for entity in ENTITIES {
        let node = node(entity).await;
        let id = node.create_ok(&cmd(1), "C1").await;
        let before = node.state();
        let item_uri = format!("{}/{id}", entity.path);

        for (method, uri) in [
            (Method::DELETE, entity.path),
            (Method::PUT, entity.path),
            (Method::DELETE, item_uri.as_str()),
            (Method::GET, item_uri.as_str()),
            (Method::PATCH, item_uri.as_str()),
        ] {
            let reply = send(
                &node.router,
                request(method.clone(), uri, Some(&MANAGER), None).unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(
                assert_error(&reply, 405, "METHOD_NOT_ALLOWED"),
                &json!({}),
                "{} {method} {uri}",
                entity.name
            );
        }
        for _ in 0..3 {
            node.list().await;
        }

        assert_eq!(node.state(), before, "{}", entity.name);
    }
}

// ---------------------------------------------------------------------------------------------
// 供应商与报损原因特有的规则
// ---------------------------------------------------------------------------------------------

// 主数据接口「供应商」：contact_phone 可以省略，省略时快照、行和投影都没有它（NULL）；出现时原样保存，不校验格式。
// 修改时省略表示删除联系电话。
#[tokio::test]
async fn supplier_contact_phone_is_optional() {
    let node = node(&SUPPLIER).await;
    let reply = node
        .create(&json!({
            "command_id": cmd(1), "code": "S1", "name": "鸡蛋供应商", "active": true,
        }))
        .await;
    let id = node.id(&reply);
    assert_eq!(
        assert_success(&reply),
        &json!({ "supplier": {
            "supplier_id": id, "code": "S1", "name": "鸡蛋供应商", "active": true, "revision": 1,
        } })
    );
    let reply = node
        .update(
            &id,
            &json!({
                "command_id": cmd(2), "base_revision": 1, "name": "鸡蛋供应商",
                "contact_phone": "+86 (21) 5555-0101 转 8", "active": true,
            }),
        )
        .await;
    assert_eq!(
        assert_success(&reply)["supplier"]["contact_phone"],
        json!("+86 (21) 5555-0101 转 8")
    );
    let reply = node
        .update(
            &id,
            &json!({
                "command_id": cmd(3), "base_revision": 2, "name": "鸡蛋供应商", "active": true,
            }),
        )
        .await;
    assert_eq!(
        assert_success(&reply),
        &json!({ "supplier": {
            "supplier_id": id, "code": "S1", "name": "鸡蛋供应商", "active": true, "revision": 3,
        } })
    );

    let payloads: Vec<Value> = node
        .events()
        .unwrap()
        .into_iter()
        .map(|e| e.payload)
        .collect();
    assert_eq!(
        payloads,
        [
            json!({ "entity": "SUPPLIER", "source": "LOCAL",
                    "snapshot": { "code": "S1", "name": "鸡蛋供应商", "active": true } }),
            json!({ "entity": "SUPPLIER", "source": "LOCAL",
                    "snapshot": { "code": "S1", "name": "鸡蛋供应商",
                                  "contact_phone": "+86 (21) 5555-0101 转 8", "active": true } }),
            json!({ "entity": "SUPPLIER", "source": "LOCAL",
                    "snapshot": { "code": "S1", "name": "鸡蛋供应商", "active": true } }),
        ]
    );
    let phone: Option<String> = spec_support::reader(&node.db_path)
        .unwrap()
        .query_row("SELECT contact_phone FROM suppliers", [], |r| r.get(0))
        .unwrap();
    assert_eq!(phone, None);
}

// 主数据接口「供应商」：contact_phone 出现时非空、首尾不能有空白字符、是字符串，不接受 null；否则 400 VALIDATION_FAILED。
#[tokio::test]
async fn invalid_supplier_contact_phone_is_rejected() {
    let node = node(&SUPPLIER).await;
    let id = node.create_ok(&cmd(1), "S1").await;
    let before = node.state();

    for phone in [
        json!(""),
        json!(" 021-5555"),
        json!("021-5555 "),
        json!("021-5555\n"),
        Value::Null,
        json!(2155550101_i64),
    ] {
        let reply = node
            .create(&json!({
                "command_id": cmd(2), "code": "S2", "name": "鸡蛋供应商",
                "contact_phone": phone, "active": true,
            }))
            .await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
        let reply = node
            .update(
                &id,
                &json!({
                    "command_id": cmd(2), "base_revision": 1, "name": "鸡蛋供应商",
                    "contact_phone": phone, "active": true,
                }),
            )
            .await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }

    assert_eq!(node.state(), before);
}

// domain「主数据」：命令引用已停用的主数据照常受理——停用的行可以修改，也可以重新启用。
#[tokio::test]
async fn inactive_rows_can_be_updated_and_reactivated() {
    for entity in ENTITIES {
        let node = node(entity).await;
        let id = node.create_ok(&cmd(1), "C1").await;
        assert_success(&node.update(&id, &node.update_body(&cmd(2), 1)).await); // 样本 B 停用

        let reply = node.update(&id, &node.unchanged_body(&cmd(3), 2)).await;

        assert_eq!(
            assert_success(&reply),
            &json!({ entity.one: node.row(&id, "C1", false, 3) }),
            "{}",
            entity.name
        );
    }
}
