//! 锁定测试：配方特有的规则——版本 1 随新建产生、追加版本、引用物料、不可修改的字段，以及 recipes /
//! recipe_versions / recipe_lines 投影。规则见 docs/domain.md「主数据」「主数据接口」配方；
//! 共用的接口行为见 spec_master_data.rs。

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
    JsonReply, MANAGER, STAFF, assert_error, assert_success, non_canonical_uuids, raw_request,
    request, send,
};
use tempfile::TempDir;

const NOW: i64 = 1_791_248_400_000; // 2026-10-06 09:00 +08:00
const URI: &str = "/api/v1/recipes";
const UNKNOWN_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8399";

type Rows<T> = Result<T, Box<dyn Error>>;
/// 事件：(aggregate_type, aggregate_id, aggregate_version, command_id, recorded_at, payload)。
type EventRow = (String, String, i64, String, i64, Value);
/// 一张表的全部行，每列转成 JSON 值。
type Table = Vec<Vec<Value>>;

struct Node {
    _dir: TempDir,
    db_path: PathBuf,
    clock: ManualClock,
    router: Router,
    flour: String,
    egg: String,
    toast: String,
    /// 版本 1 的两行用料：黄油、面粉中 ID 字节序较大的在前，以便发现按物料 ID 排序的实现。
    v1: [String; 2],
    /// 追加版本的两行用料：面粉、鸡蛋中 ID 字节序较大的在前。
    vn: [String; 2],
}

/// 两个物料 ID，字节序较大的在前。
fn descending(a: &str, b: &str) -> [String; 2] {
    if a > b {
        [a.to_owned(), b.to_owned()]
    } else {
        [b.to_owned(), a.to_owned()]
    }
}

fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209b0{n:04x}")
}

/// 测试节点，已有四个物料（命令 ID 0xff01～0xff04）。
#[allow(clippy::unwrap_used)] // 测试夹具：临时目录、Router 或物料构造失败时测试无法开始，直接终止。
async fn node() -> Node {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(UnixMillis(NOW));
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let mut ids = Vec::new();
    for (n, code, base_unit, category) in [
        (0xff01, "FLOUR", "g", "RAW"),
        (0xff02, "BUTTER", "g", "RAW"),
        (0xff03, "EGG", "pcs", "RAW"),
        (0xff04, "TOAST", "pcs", "FINISHED"),
    ] {
        let reply = spec_support::post(
            &router,
            "/api/v1/items",
            Some(&MANAGER),
            &json!({
                "command_id": cmd(n), "code": code, "name": code, "base_unit": base_unit,
                "category": category, "units": [], "active": true,
            }),
        )
        .await
        .unwrap();
        ids.push(
            assert_success(&reply)["item"]["item_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let [flour, butter, egg, toast] = <[String; 4]>::try_from(ids).unwrap();
    let v1 = descending(&butter, &flour);
    let vn = descending(&flour, &egg);
    Node {
        _dir: dir,
        db_path,
        clock,
        router,
        flour,
        egg,
        toast,
        v1,
        vn,
    }
}

fn line(item_id: &str, qty_per_batch: i64) -> Value {
    json!({ "item_id": item_id, "qty_per_batch": qty_per_batch })
}

impl Node {
    /// 吐司配方：版本 1 两行用料 200、3000（物料见 `v1`），每批产出 12。
    fn create_body(&self, command_id: &str) -> Value {
        json!({
            "command_id": command_id, "code": "R-TOAST", "name": "吐司",
            "output_item_id": self.toast, "active": true, "output_qty_per_batch": 12,
            "lines": [line(&self.v1[0], 200), line(&self.v1[1], 3000)],
        })
    }

    fn version_body(&self, command_id: &str, base_revision: i64) -> Value {
        json!({
            "command_id": command_id, "base_revision": base_revision, "output_qty_per_batch": 10,
            "lines": [line(&self.vn[0], 2800), line(&self.vn[1], 4)],
        })
    }

    fn version_1(&self) -> Value {
        json!({
            "version": 1, "output_qty_per_batch": 12,
            "lines": [line(&self.v1[0], 200), line(&self.v1[1], 3000)],
        })
    }

    fn version_n(&self, version: i64) -> Value {
        json!({
            "version": version, "output_qty_per_batch": 10,
            "lines": [line(&self.vn[0], 2800), line(&self.vn[1], 4)],
        })
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试；状态码和错误码由调用方断言。
    async fn create(&self, body: &Value) -> JsonReply {
        spec_support::post(&self.router, URI, Some(&MANAGER), body)
            .await
            .unwrap()
    }

    #[allow(clippy::unwrap_used)] // 同上。
    async fn update(&self, id: &str, body: &Value) -> JsonReply {
        spec_support::put(&self.router, &format!("{URI}/{id}"), Some(&MANAGER), body)
            .await
            .unwrap()
    }

    #[allow(clippy::unwrap_used)] // 同上。
    async fn add_version(&self, id: &str, body: &Value) -> JsonReply {
        spec_support::post(
            &self.router,
            &format!("{URI}/{id}/versions"),
            Some(&MANAGER),
            body,
        )
        .await
        .unwrap()
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：成功响应缺少 recipe_id 已违反接口约定，直接终止测试。
    async fn create_ok(&self, body: &Value) -> String {
        let reply = self.create(body).await;
        assert_success(&reply)["recipe"]["recipe_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn recipe_row(&self, id: &str, name: &str, versions: Value, revision: i64) -> Value {
        json!({
            "recipe_id": id, "code": "R-TOAST", "name": name, "output_item_id": self.toast,
            "versions": versions, "active": true, "revision": revision,
        })
    }

    /// 物料之后写入的事件：(aggregate_type, aggregate_id, aggregate_version, command_id, recorded_at, payload)。
    fn events(&self) -> Rows<Vec<EventRow>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT aggregate_type, aggregate_id, aggregate_version, command_id, recorded_at, payload
             FROM store_events WHERE seq > 4 ORDER BY seq",
        )?;
        let rows: Vec<(String, String, i64, String, i64, String)> = statement
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
        rows.into_iter()
            .map(|(t, id, v, c, at, payload)| {
                Ok((t, id, v, c, at, serde_json::from_str(&payload)?))
            })
            .collect()
    }

    /// 三张配方投影表的全部行。
    fn projection(&self) -> Rows<(Table, Table, Table)> {
        let reader = spec_support::reader(&self.db_path)?;
        let query = |sql: &str, columns: usize| -> Rows<Table> {
            let mut statement = reader.prepare(sql)?;
            let rows = statement
                .query_map([], |r| {
                    (0..columns)
                        .map(|i| {
                            Ok(match r.get::<_, boh_storage::rusqlite::types::Value>(i)? {
                                boh_storage::rusqlite::types::Value::Integer(n) => json!(n),
                                boh_storage::rusqlite::types::Value::Text(s) => json!(s),
                                other => json!(format!("{other:?}")),
                            })
                        })
                        .collect()
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        };
        Ok((
            query(
                "SELECT id, code, name, output_item_id, active, revision FROM recipes ORDER BY code",
                6,
            )?,
            query(
                "SELECT recipe_id, version, output_qty_per_batch FROM recipe_versions
                 ORDER BY recipe_id, version",
                3,
            )?,
            query(
                "SELECT recipe_id, version, line_no, item_id, qty_per_batch FROM recipe_lines
                 ORDER BY recipe_id, version, line_no",
                5,
            )?,
        ))
    }

    fn processed(&self, command_id: &str) -> Rows<(String, Value)> {
        let (command_type, request): (String, String) = spec_support::reader(&self.db_path)?
            .query_row(
                "SELECT command_type, request FROM processed_commands WHERE command_id = ?1",
                [command_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
        Ok((command_type, serde_json::from_str(&request)?))
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：读取失败本身就是断言失败。
    fn state(&self) -> (Vec<EventRow>, i64) {
        let reader = spec_support::reader(&self.db_path).unwrap();
        (
            self.events().unwrap(),
            spec_support::count(&reader, "processed_commands").unwrap(),
        )
    }
}

// 配方「新建」：output_qty_per_batch 与 lines 构成版本 1；lines 按提交顺序保存（提交时 ID 较大的物料在前，不按物料 ID 排序）；
// 快照、行和三张投影表一致；规范化请求就是去掉 command_id 的请求体。
#[tokio::test]
async fn create_builds_version_one() {
    let node = node().await;
    let body = node.create_body(&cmd(1));

    let reply = node.create(&body).await;

    let data = assert_success(&reply);
    let id = data["recipe"]["recipe_id"].as_str().unwrap().to_owned();
    assert_eq!(
        data,
        &json!({ "recipe": node.recipe_row(&id, "吐司", json!([node.version_1()]), 1) })
    );
    assert_eq!(
        node.events().unwrap(),
        [(
            "RECIPE".into(),
            id.clone(),
            1,
            cmd(1),
            NOW,
            json!({ "entity": "RECIPE", "source": "LOCAL", "snapshot": {
                "code": "R-TOAST", "name": "吐司", "output_item_id": node.toast,
                "versions": [node.version_1()], "active": true,
            } }),
        )]
    );
    assert_eq!(
        node.projection().unwrap(),
        (
            vec![vec![
                json!(id),
                json!("R-TOAST"),
                json!("吐司"),
                json!(node.toast),
                json!(1),
                json!(1)
            ]],
            vec![vec![json!(id), json!(1), json!(12)]],
            vec![
                vec![json!(id), json!(1), json!(0), json!(node.v1[0]), json!(200)],
                vec![
                    json!(id),
                    json!(1),
                    json!(1),
                    json!(node.v1[1]),
                    json!(3000)
                ],
            ],
        )
    );
    let mut request = body.clone();
    request.as_object_mut().unwrap().remove("command_id");
    assert_eq!(
        node.processed(&cmd(1)).unwrap(),
        ("recipe.create".into(), request)
    );
}

// 配方「追加版本」：新版本号 = 当前最大版本 + 1，revision + 1，已有版本原样保留；规范化请求含路径参数 recipe_id。
// output_qty_per_batch 和 lines（含行顺序）都与最新版本相同时不新增版本、不写事件，revision 不变，命令照常写入
// processed_commands，响应为当前配方，重试返回原响应；首次执行仍先检查 base_revision。只与最新版本比较：
// 行顺序不同算不同内容；从 A 改为 B 再改回 A 仍新增版本。之后修改名称不改动任何版本。
#[tokio::test]
async fn add_version_appends_and_keeps_existing_versions() {
    let node = node().await;
    let id = node.create_ok(&node.create_body(&cmd(1))).await;
    node.clock.advance(Duration::from_secs(60));

    let body = node.version_body(&cmd(2), 1);
    let reply = node.add_version(&id, &body).await;

    let versions = json!([node.version_1(), node.version_n(2)]);
    assert_eq!(
        assert_success(&reply),
        &json!({ "recipe": node.recipe_row(&id, "吐司", versions.clone(), 2) })
    );
    let events = node.events().unwrap();
    assert_eq!(
        events[1],
        (
            "RECIPE".into(),
            id.clone(),
            2,
            cmd(2),
            NOW + 60_000,
            json!({ "entity": "RECIPE", "source": "LOCAL", "snapshot": {
                "code": "R-TOAST", "name": "吐司", "output_item_id": node.toast,
                "versions": versions, "active": true,
            } }),
        )
    );
    let mut request = body.clone();
    request.as_object_mut().unwrap().remove("command_id");
    request["recipe_id"] = json!(id);
    assert_eq!(
        node.processed(&cmd(2)).unwrap(),
        ("recipe.add_version".into(), request)
    );

    let same = node.version_body(&cmd(3), 2);
    let reply = node.add_version(&id, &same).await;
    assert_eq!(
        assert_success(&reply),
        &json!({ "recipe": node.recipe_row(&id, "吐司", versions.clone(), 2) })
    );
    assert_eq!(node.events().unwrap().len(), 2);
    let mut request = same.clone();
    request.as_object_mut().unwrap().remove("command_id");
    request["recipe_id"] = json!(id);
    assert_eq!(
        node.processed(&cmd(3)).unwrap(),
        ("recipe.add_version".into(), request)
    );
    let retry = node.add_version(&id, &same).await;
    assert_eq!((retry.status.as_u16(), &retry.body), (200, &reply.body));

    let reply = node.add_version(&id, &node.version_body(&cmd(4), 1)).await;
    assert_eq!(
        assert_error(&reply, 409, "REVISION_CONFLICT"),
        &json!({ "current_revision": 2 })
    );

    let reversed = [line(&node.vn[1], 4), line(&node.vn[0], 2800)];
    let reply = node
        .add_version(
            &id,
            &json!({
                "command_id": cmd(5), "base_revision": 2, "output_qty_per_batch": 10,
                "lines": reversed,
            }),
        )
        .await;
    let v3 = json!({ "version": 3, "output_qty_per_batch": 10, "lines": reversed });
    let versions = json!([node.version_1(), node.version_n(2), v3]);
    assert_eq!(
        assert_success(&reply),
        &json!({ "recipe": node.recipe_row(&id, "吐司", versions, 3) })
    );

    let reply = node
        .add_version(
            &id,
            &json!({
                "command_id": cmd(6), "base_revision": 3, "output_qty_per_batch": 12,
                "lines": [line(&node.v1[0], 200), line(&node.v1[1], 3000)],
            }),
        )
        .await;
    let mut v4 = node.version_1();
    v4["version"] = json!(4);
    let versions = json!([node.version_1(), node.version_n(2), v3, v4]);
    assert_eq!(
        assert_success(&reply),
        &json!({ "recipe": node.recipe_row(&id, "吐司", versions.clone(), 4) })
    );
    assert_eq!(node.events().unwrap().len(), 4);

    let reply = node
        .update(
            &id,
            &json!({ "command_id": cmd(7), "base_revision": 4, "name": "白吐司", "active": true }),
        )
        .await;
    assert_eq!(
        assert_success(&reply),
        &json!({ "recipe": node.recipe_row(&id, "白吐司", versions.clone(), 5) })
    );
    let (_, version_rows, line_rows) = node.projection().unwrap();
    assert_eq!(
        version_rows,
        [
            vec![json!(id), json!(1), json!(12)],
            vec![json!(id), json!(2), json!(10)],
            vec![json!(id), json!(3), json!(10)],
            vec![json!(id), json!(4), json!(12)],
        ]
    );
    assert_eq!(line_rows.len(), 8);
    let reply = spec_support::get(&node.router, URI, Some(&STAFF))
        .await
        .unwrap();
    assert_eq!(
        assert_success(&reply),
        &json!({ "recipes": [node.recipe_row(&id, "白吐司", versions, 5)] })
    );
}

// 配方「追加版本」业务校验：base_revision 过期 409 REVISION_CONFLICT；配方不存在 404（entity = RECIPE）；
// 用料引用不存在的物料 404（entity = ITEM）。新建时产出物料或用料不存在同样 404（entity = ITEM）。都不落库。
#[tokio::test]
async fn missing_references_and_stale_revisions_are_rejected() {
    let node = node().await;
    let id = node.create_ok(&node.create_body(&cmd(1))).await;
    let before = node.state();

    let reply = node.add_version(&id, &node.version_body(&cmd(2), 2)).await;
    assert_eq!(
        assert_error(&reply, 409, "REVISION_CONFLICT"),
        &json!({ "current_revision": 1 })
    );
    let reply = node
        .add_version(UNKNOWN_ID, &node.version_body(&cmd(2), 1))
        .await;
    assert_eq!(
        assert_error(&reply, 404, "REFERENCE_NOT_FOUND"),
        &json!({ "entity": "RECIPE", "id": UNKNOWN_ID })
    );
    let mut body = node.version_body(&cmd(2), 1);
    body["lines"][1]["item_id"] = json!(UNKNOWN_ID);
    let reply = node.add_version(&id, &body).await;
    assert_eq!(
        assert_error(&reply, 404, "REFERENCE_NOT_FOUND"),
        &json!({ "entity": "ITEM", "id": UNKNOWN_ID })
    );

    for (pointer, code) in [("/output_item_id", "R2"), ("/lines/0/item_id", "R3")] {
        let mut body = node.create_body(&cmd(3));
        body["code"] = json!(code);
        *body.pointer_mut(pointer).unwrap() = json!(UNKNOWN_ID);
        let reply = node.create(&body).await;
        assert_eq!(
            assert_error(&reply, 404, "REFERENCE_NOT_FOUND"),
            &json!({ "entity": "ITEM", "id": UNKNOWN_ID }),
            "{pointer}"
        );
    }

    assert_eq!(node.state(), before);
    // 修正后可以用同一个 command_id 重提。
    assert_success(&node.add_version(&id, &node.version_body(&cmd(2), 1)).await);
}

// domain「主数据」：引用已停用的物料照常受理；不限制物料的 category，产出物料可以出现在自己的用料中。
#[tokio::test]
async fn inactive_items_and_any_category_are_accepted() {
    let node = node().await;
    let reply = spec_support::put(
        &node.router,
        &format!("/api/v1/items/{}", node.toast),
        Some(&MANAGER),
        &json!({
            "command_id": cmd(1), "base_revision": 1, "name": "TOAST", "category": "FINISHED",
            "units": [], "active": false,
        }),
    )
    .await
    .unwrap();
    assert_success(&reply);

    let mut body = node.create_body(&cmd(2));
    body["lines"] = json!([line(&node.toast, 1), line(&node.flour, 3000)]);
    let id = node.create_ok(&body).await;
    let reply = node
        .add_version(
            &id,
            &json!({
                "command_id": cmd(3), "base_revision": 1, "output_qty_per_batch": 1,
                "lines": [line(&node.toast, 2)],
            }),
        )
        .await;
    assert_eq!(
        assert_success(&reply)["recipe"]["versions"][1],
        json!({ "version": 2, "output_qty_per_batch": 1, "lines": [line(&node.toast, 2)] })
    );
}

// 配方取值：lines 为空、缺失或 null，同一版本内物料重复，用量或产出数量不是正整数，行缺字段或有未知字段，
// 物料 ID 不是规范形式的 UUIDv7；修改不接受 code、output_item_id、版本字段；追加版本不接受 name、active、
// code、output_item_id、version；路径参数不是规范形式的 UUIDv7；追加版本的请求体无法解析或缺少
// Content-Type: application/json。一律 400 VALIDATION_FAILED，不落库。
#[tokio::test]
async fn invalid_recipe_commands_are_rejected() {
    let node = node().await;
    let id = node.create_ok(&node.create_body(&cmd(1))).await;
    let before = node.state();

    let mut version_cases: Vec<(&str, Value)> = vec![
        ("lines", json!([])),
        ("lines", Value::Null),
        ("lines", json!({})),
        ("lines", json!([line(&node.flour, 1), line(&node.flour, 2)])),
        ("lines", json!([line(&node.flour, 0)])),
        ("lines", json!([line(&node.flour, -1)])),
        (
            "lines",
            json!([{ "item_id": node.flour, "qty_per_batch": 1.5 }]),
        ),
        (
            "lines",
            json!([{ "item_id": node.flour, "qty_per_batch": "1" }]),
        ),
        ("lines", json!([{ "item_id": node.flour }])),
        ("lines", json!([{ "qty_per_batch": 1 }])),
        (
            "lines",
            json!([{ "item_id": node.flour, "qty_per_batch": 1, "unit": "g" }]),
        ),
        ("lines", json!([line("not-a-uuid", 1)])),
        (
            "lines",
            json!([line("01890a5d-ac96-474b-bcce-b302099a8301", 1)]),
        ), // v4
        (
            "lines",
            json!([line("01890a5d-ac96-774b-0cce-b302099a8301", 1)]),
        ), // 变体位 0
        ("output_qty_per_batch", json!(0)),
        ("output_qty_per_batch", json!(-12)),
        ("output_qty_per_batch", json!(1.5)),
        ("output_qty_per_batch", Value::Null),
    ];
    for text in non_canonical_uuids(&node.flour) {
        version_cases.push(("lines", json!([line(&text, 1)])));
    }

    let mut creates: Vec<Value> = Vec::new();
    let mut versions: Vec<Value> = Vec::new();
    for (key, value) in &version_cases {
        let mut body = node.create_body(&cmd(2));
        body["code"] = json!("R2");
        body[*key] = value.clone();
        creates.push(body);
        let mut body = node.version_body(&cmd(2), 1);
        body[*key] = value.clone();
        versions.push(body);
    }
    let mut output_ids = vec![json!("not-a-uuid"), Value::Null];
    for text in non_canonical_uuids(&node.toast) {
        output_ids.push(json!(text));
    }
    for value in output_ids {
        let mut body = node.create_body(&cmd(2));
        body["code"] = json!("R2");
        body["output_item_id"] = value;
        creates.push(body);
    }
    for key in ["output_item_id", "output_qty_per_batch", "lines"] {
        let mut body = node.create_body(&cmd(2));
        body["code"] = json!("R2");
        body.as_object_mut().unwrap().remove(key);
        creates.push(body);
    }
    for key in ["output_qty_per_batch", "lines", "base_revision"] {
        let mut body = node.version_body(&cmd(2), 1);
        body.as_object_mut().unwrap().remove(key);
        versions.push(body);
    }
    for (key, value) in [
        ("name", json!("吐司")),
        ("active", json!(true)),
        ("code", json!("R-TOAST")),
        ("output_item_id", json!(node.toast)),
        ("version", json!(2)),
    ] {
        let mut body = node.version_body(&cmd(2), 1);
        body[key] = value;
        versions.push(body);
    }
    let update =
        json!({ "command_id": cmd(2), "base_revision": 1, "name": "吐司", "active": true });
    let mut updates: Vec<Value> = Vec::new();
    for (key, value) in [
        ("code", json!("R-TOAST")),
        ("output_item_id", json!(node.toast)),
        ("output_qty_per_batch", json!(12)),
        ("lines", json!([line(&node.flour, 3000)])),
        ("versions", json!([node.version_1()])),
    ] {
        let mut body = update.clone();
        body[key] = value;
        updates.push(body);
    }

    for body in &creates {
        let reply = node.create(body).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }
    for body in &versions {
        let reply = node.add_version(&id, body).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }
    for body in &updates {
        let reply = node.update(&id, body).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }
    let mut bad_ids = vec!["not-a-uuid".to_owned()];
    for text in non_canonical_uuids(&id) {
        bad_ids.push(text.replace('{', "%7B").replace('}', "%7D"));
    }
    for bad_id in &bad_ids {
        let reply = node
            .add_version(bad_id, &node.version_body(&cmd(2), 1))
            .await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }
    let json = Some("application/json");
    let version_uri = format!("{URI}/{id}/versions");
    let version_text = node.version_body(&cmd(2), 1).to_string();
    for (text, content_type) in [
        ("{", json),
        ("[]", json),
        ("", json),
        (version_text.as_str(), None),
        (version_text.as_str(), Some("text/plain")),
    ] {
        let req = raw_request(
            Method::POST,
            &version_uri,
            Some(&MANAGER),
            content_type,
            text,
        )
        .unwrap();
        let reply = send(&node.router, req).await.unwrap();
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }

    assert_eq!(node.state(), before);
}

// AGENTS「幂等」：追加版本重发原内容时原样返回首次的响应，不新增版本；之后配方又追加了不同内容的版本也一样。
// 与最新版本相同、没有新增版本的成功命令同样如此：重试返回当时的 revision 和版本，不返回当前配方。
// 内容不同时 409 IDEMPOTENCY_CONFLICT，details.fields 列出不同的顶层字段（含路径参数 recipe_id）；
// 同一 command_id 用于修改时只列 "command_type"。
#[tokio::test]
async fn add_version_retries_and_conflicts() {
    let node = node().await;
    let id = node.create_ok(&node.create_body(&cmd(1))).await;
    let mut other = node.create_body(&cmd(9));
    other["code"] = json!("R-OTHER");
    let other_id = node.create_ok(&other).await;
    let body = node.version_body(&cmd(2), 1);
    let first = node.add_version(&id, &body).await;
    assert_eq!(assert_success(&first)["recipe"]["revision"], json!(2));
    let same = node.version_body(&cmd(3), 2);
    let unchanged = node.add_version(&id, &same).await;
    assert_eq!(assert_success(&unchanged)["recipe"]["revision"], json!(2));
    let different = json!({
        "command_id": cmd(4), "base_revision": 2, "output_qty_per_batch": 8,
        "lines": [line(&node.egg, 5)],
    });
    let reply = node.add_version(&id, &different).await;
    let recipe = &assert_success(&reply)["recipe"];
    assert_eq!(recipe["revision"], json!(3));
    assert_eq!(recipe["versions"].as_array().unwrap().len(), 3);
    let before = node.state();
    node.clock.advance(Duration::from_secs(3600));

    let reply = node.add_version(&id, &body).await;
    assert_eq!((reply.status.as_u16(), &reply.body), (200, &first.body));
    let reply = node.add_version(&id, &same).await;
    assert_eq!((reply.status.as_u16(), &reply.body), (200, &unchanged.body));

    let mut changed = body.clone();
    changed["lines"] = json!([line(&node.flour, 2900), line(&node.egg, 4)]);
    let mut changed_qty = body.clone();
    changed_qty["output_qty_per_batch"] = json!(11);
    let cases = [
        (node.add_version(&id, &changed).await, json!(["lines"])),
        (
            node.add_version(&id, &changed_qty).await,
            json!(["output_qty_per_batch"]),
        ),
        (node.add_version(&other_id, &body).await, json!(["recipe_id"])),
        (
            node.update(
                &id,
                &json!({ "command_id": cmd(2), "base_revision": 1, "name": "吐司", "active": true }),
            )
            .await,
            json!(["command_type"]),
        ),
    ];
    for (reply, fields) in &cases {
        assert_eq!(
            assert_error(reply, 409, "IDEMPOTENCY_CONFLICT"),
            &json!({ "fields": fields })
        );
    }

    assert_eq!(node.state(), before);
}

// 配方「追加版本」权限同修改：只允许 MANAGER（STAFF 403），没有身份 401；路由只接受 POST（其他方法 405）。不落库。
#[tokio::test]
async fn add_version_requires_manager() {
    let node = node().await;
    let id = node.create_ok(&node.create_body(&cmd(1))).await;
    let before = node.state();
    let uri = format!("{URI}/{id}/versions");
    let body = node.version_body(&cmd(2), 1);

    let reply = send(
        &node.router,
        request(Method::POST, &uri, None, Some(&body)).unwrap(),
    )
    .await
    .unwrap();
    assert_error(&reply, 401, "UNAUTHENTICATED");
    let reply = send(
        &node.router,
        request(Method::POST, &uri, Some(&STAFF), Some(&body)).unwrap(),
    )
    .await
    .unwrap();
    assert_error(&reply, 403, "FORBIDDEN");
    for method in [Method::GET, Method::PUT, Method::DELETE] {
        let reply = send(
            &node.router,
            request(method.clone(), &uri, Some(&MANAGER), None).unwrap(),
        )
        .await
        .unwrap();
        assert_error(&reply, 405, "METHOD_NOT_ALLOWED");
    }

    assert_eq!(node.state(), before);
}
