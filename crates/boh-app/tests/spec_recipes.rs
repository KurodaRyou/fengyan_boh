//! 锁定测试：配方特有的规则——版本 1 随新建产生、追加版本、引用物料、不可修改的字段、每批产出与用量的 input 和单位换算，
//! 以及 recipes / recipe_versions / recipe_lines 投影。规则见 docs/domain.md「主数据」「主数据接口」配方、「数量」「单位」；
//! 共用的接口行为见 spec_master_data.rs。
//! 数量与换算系数在请求、响应和快照中是小数字符串；投影中数量存千分之一（基本单位或录入单位），系数存十亿分之一基本单位。

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
/// 换算系数 1 的存储值（十亿分之一基本单位）。
const ONE: i64 = 1_000_000_000;
/// 1 oz = 28.349523125 g 的存储值。
const OZ: i64 = 28_349_523_125;

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
    /// 基本单位 g，单位 kg（1000）、oz（28.349523125）。
    flour: String,
    /// 基本单位 g，单位 oz（28.349523125）。
    butter: String,
    /// 基本单位 pcs，没有其他单位。
    egg: String,
    /// 基本单位 pcs，没有其他单位。
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
    for (n, code, base_unit, category, units) in [
        (
            0xff01,
            "FLOUR",
            "g",
            "RAW",
            json!([
                { "unit_code": "kg", "base_qty_per_unit": "1000" },
                { "unit_code": "oz", "base_qty_per_unit": "28.349523125" },
            ]),
        ),
        (
            0xff02,
            "BUTTER",
            "g",
            "RAW",
            json!([{ "unit_code": "oz", "base_qty_per_unit": "28.349523125" }]),
        ),
        (0xff03, "EGG", "pcs", "RAW", json!([])),
        (0xff04, "TOAST", "pcs", "FINISHED", json!([])),
    ] {
        let reply = spec_support::post(
            &router,
            "/api/v1/items",
            Some(&MANAGER),
            &json!({
                "command_id": cmd(n), "code": code, "name": code, "base_unit": base_unit,
                "category": category, "units": units, "active": true,
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
        butter,
        egg,
        toast,
        v1,
        vn,
    }
}

/// 录入快照 `{qty, unit_code, base_qty_per_unit}`。
fn input(qty: &str, unit_code: &str, factor: &str) -> Value {
    json!({ "qty": qty, "unit_code": unit_code, "base_qty_per_unit": factor })
}

/// 请求中的一行用料。
fn req_line(item_id: &str, qty: &str, unit_code: &str, factor: &str) -> Value {
    json!({ "item_id": item_id, "input": input(qty, unit_code, factor) })
}

/// 快照与行中的一行用料：换算后的 `qty_per_batch` 与录入快照 `input`。
fn snap_line(item_id: &str, qty_per_batch: &str, input: Value) -> Value {
    json!({ "item_id": item_id, "qty_per_batch": qty_per_batch, "input": input })
}

/// `recipe_versions` 的一行：(recipe_id, version, output_qty_per_batch, output_input_qty, output_input_unit_code,
/// output_input_base_qty_per_unit)。
fn version_row(
    id: &str,
    version: i64,
    qty: i64,
    input_qty: i64,
    unit: &str,
    factor: i64,
) -> Vec<Value> {
    vec![
        json!(id),
        json!(version),
        json!(qty),
        json!(input_qty),
        json!(unit),
        json!(factor),
    ]
}

/// `recipe_lines` 的一行：(recipe_id, version, line_no, item_id, qty_per_batch, input_qty, input_unit_code,
/// input_base_qty_per_unit)。
#[allow(clippy::too_many_arguments)] // 测试辅助：参数逐一对应表的列。
fn line_row(
    id: &str,
    version: i64,
    line_no: i64,
    item_id: &str,
    qty: i64,
    input_qty: i64,
    unit: &str,
    factor: i64,
) -> Vec<Value> {
    vec![
        json!(id),
        json!(version),
        json!(line_no),
        json!(item_id),
        json!(qty),
        json!(input_qty),
        json!(unit),
        json!(factor),
    ]
}

impl Node {
    fn base_unit(&self, item_id: &str) -> &'static str {
        if item_id == self.egg || item_id == self.toast {
            "pcs"
        } else {
            "g"
        }
    }

    /// 以基本单位录入的一行用料（系数 "1"）。
    fn line(&self, item_id: &str, qty: &str) -> Value {
        req_line(item_id, qty, self.base_unit(item_id), "1")
    }

    /// 以基本单位录入的一行用料在快照中的样子。
    fn snapshot_line(&self, item_id: &str, qty: &str) -> Value {
        snap_line(item_id, qty, input(qty, self.base_unit(item_id), "1"))
    }

    /// 吐司配方：版本 1 两行用料 200、3000 g（物料见 `v1`），每批产出 12 pcs。
    fn create_body(&self, command_id: &str) -> Value {
        json!({
            "command_id": command_id, "code": "R-TOAST", "name": "吐司",
            "output_item_id": self.toast, "active": true,
            "output_input": input("12", "pcs", "1"),
            "lines": [self.line(&self.v1[0], "200"), self.line(&self.v1[1], "3000")],
        })
    }

    /// 追加版本：每批产出 10 pcs，用料 `vn` 两行 2800、4（各自的基本单位）。
    fn version_body(&self, command_id: &str, base_revision: i64) -> Value {
        json!({
            "command_id": command_id, "base_revision": base_revision,
            "output_input": input("10", "pcs", "1"),
            "lines": [self.line(&self.vn[0], "2800"), self.line(&self.vn[1], "4")],
        })
    }

    fn version_1(&self) -> Value {
        json!({
            "version": 1, "output_qty_per_batch": "12", "output_input": input("12", "pcs", "1"),
            "lines": [
                self.snapshot_line(&self.v1[0], "200"),
                self.snapshot_line(&self.v1[1], "3000"),
            ],
        })
    }

    fn version_n(&self, version: i64) -> Value {
        json!({
            "version": version, "output_qty_per_batch": "10", "output_input": input("10", "pcs", "1"),
            "lines": [
                self.snapshot_line(&self.vn[0], "2800"),
                self.snapshot_line(&self.vn[1], "4"),
            ],
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

    /// 修改物料的单位（名称取物料 ID，分类 RAW，启用）。
    #[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态的请求失败时，后续步骤没有意义，直接终止测试。
    async fn set_units(&self, n: u16, item_id: &str, base_revision: i64, units: Value) {
        let reply = spec_support::put(
            &self.router,
            &format!("/api/v1/items/{item_id}"),
            Some(&MANAGER),
            &json!({
                "command_id": cmd(n), "base_revision": base_revision, "name": item_id,
                "category": "RAW", "units": units, "active": true,
            }),
        )
        .await
        .unwrap();
        assert_success(&reply);
    }

    fn recipe_row(&self, id: &str, name: &str, versions: Value, revision: i64) -> Value {
        json!({
            "recipe_id": id, "code": "R-TOAST", "name": name, "output_item_id": self.toast,
            "versions": versions, "active": true, "revision": revision,
        })
    }

    /// 物料之后写入的配方事件：(aggregate_type, aggregate_id, aggregate_version, command_id, recorded_at, payload)。
    fn events(&self) -> Rows<Vec<EventRow>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT aggregate_type, aggregate_id, aggregate_version, command_id, recorded_at, payload
             FROM store_events WHERE aggregate_type = 'RECIPE' ORDER BY seq",
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
                "SELECT recipe_id, version, output_qty_per_batch, output_input_qty, output_input_unit_code,
                        output_input_base_qty_per_unit
                 FROM recipe_versions ORDER BY recipe_id, version",
                6,
            )?,
            query(
                "SELECT recipe_id, version, line_no, item_id, qty_per_batch, input_qty, input_unit_code,
                        input_base_qty_per_unit
                 FROM recipe_lines ORDER BY recipe_id, version, line_no",
                8,
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

// 配方「新建」：每批产出（output_input）与 lines 构成版本 1；lines 按提交顺序保存（提交时 ID 较大的物料在前，不按物料 ID 排序）；
// 快照、行和三张投影表一致；规范化请求就是去掉 command_id 的请求体。
// 用基本单位录入时 qty_per_batch 等于 input.qty；投影中 12 pcs 存 12000，系数 1 存 10^9。
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
            vec![version_row(&id, 1, 12_000, 12_000, "pcs", ONE)],
            vec![
                line_row(&id, 1, 0, &node.v1[0], 200_000, 200_000, "g", ONE),
                line_row(&id, 1, 1, &node.v1[1], 3_000_000, 3_000_000, "g", ONE),
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

// 主数据接口「配方」数量与 domain「单位」换算：每批产出和每行用量按录入时所选的单位提交，快照同时保存换算后的基本单位数量
// （qty_per_batch、output_qty_per_batch）和 input；每行单独换算，四舍五入到 0.001 基本单位。
// - 面粉 2.5 kg（请求写作 "2.500"）→ qty_per_batch "2500"；黄油 1.25 oz → 35.43690390625 g，记 "35.437"；
//   每批产出 12 pcs。
// - 快照、行、规范化请求中都只有规范形式（"2.500" 写成 "2.5"）。
// - 投影：qty_per_batch 存千分之一基本单位（2500000、35437），input_qty 存千分之一录入单位（2500、1250），
//   系数存十亿分之一基本单位（10^12、28349523125）。
// - 录入上限只看录入的数字：9999.999 kg 照常受理，qty_per_batch "9999999"。
#[tokio::test]
async fn inputs_are_converted_and_kept_with_the_version() {
    let node = node().await;
    let body = json!({
        "command_id": cmd(1), "code": "R-TOAST", "name": "吐司", "output_item_id": node.toast,
        "active": true, "output_input": input("12", "pcs", "1"),
        "lines": [
            req_line(&node.flour, "2.500", "kg", "1000"),
            req_line(&node.butter, "1.25", "oz", "28.349523125"),
        ],
    });

    let reply = node.create(&body).await;

    let recipe = &assert_success(&reply)["recipe"];
    let id = recipe["recipe_id"].as_str().unwrap().to_owned();
    let version = json!({
        "version": 1, "output_qty_per_batch": "12", "output_input": input("12", "pcs", "1"),
        "lines": [
            snap_line(&node.flour, "2500", input("2.5", "kg", "1000")),
            snap_line(&node.butter, "35.437", input("1.25", "oz", "28.349523125")),
        ],
    });
    assert_eq!(recipe["versions"], json!([version]));
    assert_eq!(
        node.events().unwrap()[0].5["snapshot"]["versions"],
        json!([version])
    );
    let (_, versions, lines) = node.projection().unwrap();
    assert_eq!(versions, [version_row(&id, 1, 12_000, 12_000, "pcs", ONE)]);
    assert_eq!(
        lines,
        [
            line_row(&id, 1, 0, &node.flour, 2_500_000, 2_500, "kg", 1_000 * ONE),
            line_row(&id, 1, 1, &node.butter, 35_437, 1_250, "oz", OZ),
        ]
    );
    let (_, request) = node.processed(&cmd(1)).unwrap();
    assert_eq!(request["lines"][0]["input"], input("2.5", "kg", "1000"));

    let reply = node
        .add_version(
            &id,
            &json!({
                "command_id": cmd(2), "base_revision": 1,
                "output_input": input("9999.999", "pcs", "1"),
                "lines": [req_line(&node.flour, "9999.999", "kg", "1000")],
            }),
        )
        .await;
    assert_eq!(
        assert_success(&reply)["recipe"]["versions"][1],
        json!({
            "version": 2, "output_qty_per_batch": "9999.999",
            "output_input": input("9999.999", "pcs", "1"),
            "lines": [snap_line(&node.flour, "9999999", input("9999.999", "kg", "1000"))],
        })
    );
    let (_, versions, lines) = node.projection().unwrap();
    assert_eq!(
        versions[1],
        version_row(&id, 2, 9_999_999, 9_999_999, "pcs", ONE)
    );
    assert_eq!(
        lines[2],
        line_row(
            &id,
            2,
            0,
            &node.flour,
            9_999_999_000,
            9_999_999,
            "kg",
            1_000 * ONE
        )
    );
}

// 配方「追加版本」：新版本号 = 当前最大版本 + 1，revision + 1，已有版本原样保留；规范化请求含路径参数 recipe_id。
// 每批产出和 lines（含行顺序和 input）都与最新版本相同时不新增版本、不写事件，revision 不变，命令照常写入
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

    let reversed = [node.line(&node.vn[1], "4"), node.line(&node.vn[0], "2800")];
    let reply = node
        .add_version(
            &id,
            &json!({
                "command_id": cmd(5), "base_revision": 2, "output_input": input("10", "pcs", "1"),
                "lines": reversed,
            }),
        )
        .await;
    let v3 = json!({
        "version": 3, "output_qty_per_batch": "10", "output_input": input("10", "pcs", "1"),
        "lines": [node.snapshot_line(&node.vn[1], "4"), node.snapshot_line(&node.vn[0], "2800")],
    });
    let versions = json!([node.version_1(), node.version_n(2), v3]);
    assert_eq!(
        assert_success(&reply),
        &json!({ "recipe": node.recipe_row(&id, "吐司", versions, 3) })
    );

    let reply = node
        .add_version(
            &id,
            &json!({
                "command_id": cmd(6), "base_revision": 3, "output_input": input("12", "pcs", "1"),
                "lines": [node.line(&node.v1[0], "200"), node.line(&node.v1[1], "3000")],
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
            version_row(&id, 1, 12_000, 12_000, "pcs", ONE),
            version_row(&id, 2, 10_000, 10_000, "pcs", ONE),
            version_row(&id, 3, 10_000, 10_000, "pcs", ONE),
            version_row(&id, 4, 12_000, 12_000, "pcs", ONE),
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

// 主数据接口「配方」追加版本：是否与最新版本相同按 input 比较，比较规范形式。
// - 最新版本面粉 2.5 kg：同样内容写作 "2.500" kg、系数 "1000.0"，与最新版本相同，不新增版本、不写事件，revision 不变。
// - 同一数量换一种单位录入（2500 g，换算后的 qty_per_batch 同为 "2500"）：新增版本。每批产出同理（12 pcs 不变时只看 lines）。
#[tokio::test]
async fn unchanged_versions_compare_canonical_inputs() {
    let node = node().await;
    let id = node
        .create_ok(&json!({
            "command_id": cmd(1), "code": "R-TOAST", "name": "吐司", "output_item_id": node.toast,
            "active": true, "output_input": input("12", "pcs", "1"),
            "lines": [req_line(&node.flour, "2.5", "kg", "1000")],
        }))
        .await;

    let reply = node
        .add_version(
            &id,
            &json!({
                "command_id": cmd(2), "base_revision": 1, "output_input": input("12.000", "pcs", "1.0"),
                "lines": [req_line(&node.flour, "2.500", "kg", "1000.0")],
            }),
        )
        .await;
    let recipe = &assert_success(&reply)["recipe"];
    assert_eq!(recipe["revision"], json!(1));
    assert_eq!(recipe["versions"].as_array().unwrap().len(), 1);
    assert_eq!(node.events().unwrap().len(), 1);

    let reply = node
        .add_version(
            &id,
            &json!({
                "command_id": cmd(3), "base_revision": 1, "output_input": input("12", "pcs", "1"),
                "lines": [req_line(&node.flour, "2500", "g", "1")],
            }),
        )
        .await;
    let recipe = &assert_success(&reply)["recipe"];
    assert_eq!(recipe["revision"], json!(2));
    assert_eq!(
        recipe["versions"][1],
        json!({
            "version": 2, "output_qty_per_batch": "12", "output_input": input("12", "pcs", "1"),
            "lines": [snap_line(&node.flour, "2500", input("2500", "g", "1"))],
        })
    );
}

// domain「单位」与主数据接口「配方」：写入线程按当前换算核对产出物料和各行物料的单位（业务校验，幂等检查之后）。
// - 用料的 unit_code 既不是基本单位也不在 units 中：400 UNKNOWN_UNIT，details {line, item_id, unit_code}；
//   系数与当前值不相等（按数值比较）：409 UNIT_CONVERSION_CHANGED，details {line, item_id, unit_code, base_qty_per_unit: 当前值}，
//   当前值是规范形式的小数字符串，基本单位为 "1"。
// - 产出没有行下标：details 不带 line。
// - 新建与追加版本相同；都不写事件或 processed_commands，修正后可以用同一个 command_id 重提。
// - 系数只是写法不同（"1000.0" 与当前的 "1000"）照常受理，快照中写成规范形式。
// - 首次成功后换算系数变了，原内容重试仍返回首次响应。
#[tokio::test]
async fn unit_checks_cover_the_output_and_every_line() {
    let node = node().await;
    let id = node.create_ok(&node.create_body(&cmd(1))).await;
    let before = node.state();

    let with_line = |body: &Value, line: Value| {
        let mut body = body.clone();
        body["lines"][1] = line;
        body
    };
    let with_output = |body: &Value, output: Value| {
        let mut body = body.clone();
        body["output_input"] = output;
        body
    };
    let mut create = node.create_body(&cmd(2));
    create["code"] = json!("R2");
    create["lines"] = json!([node.line(&node.egg, "4"), node.line(&node.flour, "100")]);
    let version = {
        let mut body = node.version_body(&cmd(2), 1);
        body["lines"] = create["lines"].clone();
        body
    };
    let flour = node.flour.clone();
    let toast = node.toast.clone();
    let cases: Vec<(Value, u16, &str, Value)> = vec![
        (
            req_line(&flour, "1", "bag", "1000"),
            400,
            "UNKNOWN_UNIT",
            json!({ "line": 1, "item_id": flour, "unit_code": "bag" }),
        ),
        (
            req_line(&flour, "1", "KG", "1000"),
            400,
            "UNKNOWN_UNIT",
            json!({ "line": 1, "item_id": flour, "unit_code": "KG" }),
        ),
        (
            req_line(&flour, "1", "kg", "1001"),
            409,
            "UNIT_CONVERSION_CHANGED",
            json!({ "line": 1, "item_id": flour, "unit_code": "kg", "base_qty_per_unit": "1000" }),
        ),
        (
            req_line(&flour, "1", "oz", "28.34952312"),
            409,
            "UNIT_CONVERSION_CHANGED",
            json!({ "line": 1, "item_id": flour, "unit_code": "oz", "base_qty_per_unit": "28.349523125" }),
        ),
        (
            req_line(&flour, "100", "g", "1.5"),
            409,
            "UNIT_CONVERSION_CHANGED",
            json!({ "line": 1, "item_id": flour, "unit_code": "g", "base_qty_per_unit": "1" }),
        ),
    ];
    let outputs: Vec<(Value, u16, &str, Value)> = vec![
        (
            input("12", "box", "6"),
            400,
            "UNKNOWN_UNIT",
            json!({ "item_id": toast, "unit_code": "box" }),
        ),
        (
            input("12", "pcs", "2"),
            409,
            "UNIT_CONVERSION_CHANGED",
            json!({ "item_id": toast, "unit_code": "pcs", "base_qty_per_unit": "1" }),
        ),
    ];
    for (line, status, code, details) in &cases {
        for (name, reply) in [
            (
                "create",
                node.create(&with_line(&create, line.clone())).await,
            ),
            (
                "add_version",
                node.add_version(&id, &with_line(&version, line.clone()))
                    .await,
            ),
        ] {
            assert_eq!(
                assert_error(&reply, *status, code),
                details,
                "{name} {line}"
            );
        }
    }
    for (output, status, code, details) in &outputs {
        for (name, reply) in [
            (
                "create",
                node.create(&with_output(&create, output.clone())).await,
            ),
            (
                "add_version",
                node.add_version(&id, &with_output(&version, output.clone()))
                    .await,
            ),
        ] {
            assert_eq!(
                assert_error(&reply, *status, code),
                details,
                "{name} {output}"
            );
        }
    }
    assert_eq!(node.state(), before);

    // 同一个 command_id 按当前换算重提；系数写法不同但数值相等照常受理。
    let fixed = with_line(&create, req_line(&flour, "0.25", "kg", "1000.0"));
    let reply = node.create(&fixed).await;
    let first = assert_success(&reply).clone();
    assert_eq!(
        first["recipe"]["versions"][0]["lines"][1],
        snap_line(&flour, "250", input("0.25", "kg", "1000"))
    );

    // 首次成功后 kg 的系数改了：原内容重试返回首次响应；新命令则被拒绝。
    node.set_units(
        3,
        &flour,
        1,
        json!([
            { "unit_code": "kg", "base_qty_per_unit": "999" },
            { "unit_code": "oz", "base_qty_per_unit": "28.349523125" },
        ]),
    )
    .await;
    let retry = node.create(&fixed).await;
    assert_eq!((retry.status.as_u16(), &retry.body), (200, &reply.body));
    let mut again = fixed.clone();
    again["command_id"] = json!(cmd(4));
    again["code"] = json!("R3");
    let reply = node.create(&again).await;
    assert_eq!(
        assert_error(&reply, 409, "UNIT_CONVERSION_CHANGED"),
        &json!({ "line": 1, "item_id": flour, "unit_code": "kg", "base_qty_per_unit": "999" })
    );
    // 已保存的版本不受之后修改换算系数的影响。
    let reply = spec_support::get(&node.router, URI, Some(&STAFF))
        .await
        .unwrap();
    let recipes = assert_success(&reply)["recipes"]
        .as_array()
        .unwrap()
        .clone();
    let r2 = recipes.iter().find(|r| r["code"] == json!("R2")).unwrap();
    assert_eq!(r2["versions"], first["recipe"]["versions"]);
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

// domain「主数据」：引用已停用的物料照常受理；「主数据接口」配方：不限制物料的 category，产出物料可以出现在自己的用料中。
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
    body["lines"] = json!([node.line(&node.toast, "1"), node.line(&node.flour, "3000")]);
    let id = node.create_ok(&body).await;
    let reply = node
        .add_version(
            &id,
            &json!({
                "command_id": cmd(3), "base_revision": 1, "output_input": input("1", "pcs", "1"),
                "lines": [node.line(&node.toast, "2")],
            }),
        )
        .await;
    assert_eq!(
        assert_success(&reply)["recipe"]["versions"][1],
        json!({
            "version": 2, "output_qty_per_batch": "1", "output_input": input("1", "pcs", "1"),
            "lines": [node.snapshot_line(&node.toast, "2")],
        })
    );
}

// 配方取值：lines 为空、缺失或 null，同一版本内物料重复，行缺字段或有未知字段（含旧结构的 qty_per_batch），物料 ID 不是
// 规范形式的 UUIDv7；output_input 与各行 input 缺失、为 null、缺字段、多字段，input.qty 不符合数量规则（domain「数量」：
// 小数字符串、大于 0、最多 3 位小数、不超过录入上限 9999.999），input.base_qty_per_unit 不符合换算系数规则，unit_code
// 为空或首尾有空白，换算结果为 0（0.4 mg × 0.001 = 0.0004 g）；修改不接受 code、output_item_id、版本字段；追加版本不接受
// name、active、code、output_item_id、version；路径参数不是规范形式的 UUIDv7；追加版本的请求体无法解析或缺少
// Content-Type: application/json。一律 400 VALIDATION_FAILED，不落库。
#[tokio::test]
async fn invalid_recipe_commands_are_rejected() {
    let node = node().await;
    let id = node.create_ok(&node.create_body(&cmd(1))).await;
    let before = node.state();
    let flour = node.flour.clone();

    let mut inputs: Vec<Value> = vec![
        Value::Null,
        json!({}),
        json!({ "unit_code": "g", "base_qty_per_unit": "1" }),
        json!({ "qty": "1", "base_qty_per_unit": "1" }),
        json!({ "qty": "1", "unit_code": "g" }),
        json!({ "qty": "1", "unit_code": "g", "base_qty_per_unit": "1", "note": "x" }),
        input("1", "", "1"),
        input("1", " g", "1"),
        input("1", "g ", "1"),
        json!({ "qty": "1", "unit_code": 1, "base_qty_per_unit": "1" }),
        input("0.4", "mg", "0.001"),
        input("0.001", "g", "0.4"),
    ];
    for (_, qty) in spec_support::invalid_input_qty_texts() {
        inputs.push(json!({ "qty": qty, "unit_code": "g", "base_qty_per_unit": "1" }));
    }
    for (_, factor) in spec_support::invalid_factor_texts() {
        inputs.push(json!({ "qty": "1", "unit_code": "g", "base_qty_per_unit": factor }));
    }

    let mut version_cases: Vec<(&str, Value)> = vec![
        ("lines", json!([])),
        ("lines", Value::Null),
        ("lines", json!({})),
        (
            "lines",
            json!([node.line(&flour, "1"), node.line(&flour, "2")]),
        ),
        ("lines", json!([{ "item_id": flour }])),
        ("lines", json!([{ "input": input("1", "g", "1") }])),
        (
            "lines",
            json!([{ "item_id": flour, "input": input("1", "g", "1"), "note": "x" }]),
        ),
        (
            "lines",
            json!([{ "item_id": flour, "input": input("1", "g", "1"), "qty_per_batch": "1" }]),
        ),
        ("lines", json!([{ "item_id": flour, "qty_per_batch": "1" }])),
        ("lines", json!([req_line("not-a-uuid", "1", "g", "1")])),
        (
            "lines",
            json!([req_line(
                "01890a5d-ac96-474b-bcce-b302099a8301",
                "1",
                "g",
                "1"
            )]),
        ), // v4
        (
            "lines",
            json!([req_line(
                "01890a5d-ac96-774b-0cce-b302099a8301",
                "1",
                "g",
                "1"
            )]),
        ), // 变体位 0
        ("output_qty_per_batch", json!("12")),
    ];
    for text in non_canonical_uuids(&flour) {
        version_cases.push(("lines", json!([req_line(&text, "1", "g", "1")])));
    }
    for value in &inputs {
        version_cases.push(("output_input", value.clone()));
        version_cases.push(("lines", json!([{ "item_id": flour, "input": value }])));
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
    for key in ["output_item_id", "output_input", "lines"] {
        let mut body = node.create_body(&cmd(2));
        body["code"] = json!("R2");
        body.as_object_mut().unwrap().remove(key);
        creates.push(body);
    }
    for key in ["output_input", "lines", "base_revision"] {
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
        ("output_input", input("12", "pcs", "1")),
        ("lines", json!([node.line(&flour, "3000")])),
        ("versions", json!([node.version_1()])),
    ] {
        let mut body = update.clone();
        body[key] = value;
        updates.push(body);
    }

    for body in &creates {
        let reply = node.create(body).await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "{body}"
        );
    }
    for body in &versions {
        let reply = node.add_version(&id, body).await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "{body}"
        );
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

// AGENTS「HTTP 约定」处理顺序与 domain「数量」「单位」：录入上限与换算结果为 0 属于取值，在幂等检查之前——已成功的
// command_id 携带超出录入上限（10000 g）或换算为 0 的内容重发时是 400 VALIDATION_FAILED，不是 409。新建与追加版本相同。
#[tokio::test]
async fn quantity_values_are_checked_before_idempotency() {
    let node = node().await;
    let create = node.create_body(&cmd(1));
    let id = node.create_ok(&create).await;
    let version = node.version_body(&cmd(2), 1);
    assert_success(&node.add_version(&id, &version).await);
    let before = node.state();

    for (pointer, value) in [
        ("/output_input", input("10000", "pcs", "1")),
        ("/lines/0/input", input("10000", "g", "1")),
        ("/lines/0/input", input("0.4", "mg", "0.001")),
    ] {
        let mut body = create.clone();
        *body.pointer_mut(pointer).unwrap() = value.clone();
        let reply = node.create(&body).await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "create {pointer} {value}"
        );
        let mut body = version.clone();
        *body.pointer_mut(pointer).unwrap() = value.clone();
        let reply = node.add_version(&id, &body).await;
        assert_eq!(
            assert_error(&reply, 400, "VALIDATION_FAILED"),
            &json!({}),
            "add_version {pointer} {value}"
        );
    }

    assert_eq!(node.state(), before);
}

// AGENTS「幂等」：追加版本重发原内容时原样返回首次的响应，不新增版本；之后配方又追加了不同内容的版本也一样。
// 与最新版本相同、没有新增版本的成功命令同样如此：重试返回当时的 revision 和版本，不返回当前配方。
// 数量只是写法不同（"2800.000" 与 "2800"）时规范化请求相同，也返回首次的响应。
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
        "command_id": cmd(4), "base_revision": 2, "output_input": input("8", "pcs", "1"),
        "lines": [node.line(&node.egg, "5")],
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
    let mut rewritten = body.clone();
    rewritten["lines"][0]["input"]["qty"] = json!("2800.000");
    rewritten["output_input"]["base_qty_per_unit"] = json!("1.000000000");
    let reply = node.add_version(&id, &rewritten).await;
    assert_eq!((reply.status.as_u16(), &reply.body), (200, &first.body));

    let mut changed = body.clone();
    changed["lines"] = json!([node.line(&node.flour, "2900"), node.line(&node.egg, "4")]);
    let mut changed_qty = body.clone();
    changed_qty["output_input"]["qty"] = json!("11");
    let mut changed_unit = body.clone();
    changed_unit["lines"][0]["input"]["unit_code"] = json!("kg");
    let cases = [
        (node.add_version(&id, &changed).await, json!(["lines"])),
        (
            node.add_version(&id, &changed_qty).await,
            json!(["output_input"]),
        ),
        (node.add_version(&id, &changed_unit).await, json!(["lines"])),
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
