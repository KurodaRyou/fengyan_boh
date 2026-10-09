//! 锁定测试：物料特有的字段规则（编码字符集、基本单位、分类、默认保质期、单位换算）与 items / item_units 投影。
//! 规则见 docs/domain.md「主数据」「主数据接口」物料、「单位」；共用的接口行为见 spec_master_data.rs。

mod spec_support;

use std::error::Error;
use std::path::PathBuf;

use axum::Router;
use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use serde_json::{Value, json};
use spec_support::{JsonReply, MANAGER, STAFF, assert_error, assert_success};
use tempfile::TempDir;

const NOW: i64 = 1_791_248_400_000; // 2026-10-06 09:00 +08:00
const URI: &str = "/api/v1/items";
const HALF_YEAR: i64 = 15_552_000_000; // 180 天

type Rows<T> = Result<T, Box<dyn Error>>;
/// `items` 的一行：(id, code, name, base_unit, category, default_shelf_life_ms, active, revision)。
type ItemRow = (
    String,
    String,
    String,
    String,
    String,
    Option<i64>,
    i64,
    i64,
);

struct Node {
    _dir: TempDir,
    db_path: PathBuf,
    router: Router,
}

#[allow(clippy::unwrap_used)] // 测试夹具：临时目录或 Router 构造失败时测试无法开始，直接终止。
fn node() -> Node {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let router = spec_support::router(&db_path, ManualClock::new(UnixMillis(NOW)).clock()).unwrap();
    Node {
        _dir: dir,
        db_path,
        router,
    }
}

fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209af{n:04x}")
}

fn flour(command_id: &str) -> Value {
    json!({
        "command_id": command_id, "code": "FLOUR", "name": "高筋面粉", "base_unit": "g",
        "category": "RAW", "default_shelf_life_ms": HALF_YEAR,
        "units": [
            { "unit_code": "bag", "base_qty_per_unit": 25000 },
            { "unit_code": "cup", "base_qty_per_unit": 120 },
        ],
        "active": true,
    })
}

impl Node {
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

    #[allow(clippy::unwrap_used)] // 测试夹具：成功响应缺少 item_id 已违反接口约定，直接终止测试。
    async fn create_ok(&self, body: &Value) -> String {
        let reply = self.create(body).await;
        assert_success(&reply)["item"]["item_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn items(&self) -> Rows<Vec<ItemRow>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT id, code, name, base_unit, category, default_shelf_life_ms, active, revision
             FROM items ORDER BY code",
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
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// `item_units` 的全部行：(item_id, unit_code, base_qty_per_unit)，按物料和单位排序。
    fn units(&self) -> Rows<Vec<(String, String, i64)>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare(
            "SELECT item_id, unit_code, base_qty_per_unit FROM item_units
             ORDER BY item_id, unit_code",
        )?;
        let rows = statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    fn payloads(&self) -> Rows<Vec<Value>> {
        let reader = spec_support::reader(&self.db_path)?;
        let mut statement = reader.prepare("SELECT payload FROM store_events ORDER BY seq")?;
        let texts: Vec<String> = statement
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(texts
            .iter()
            .map(|text| serde_json::from_str(text))
            .collect::<Result<_, _>>()?)
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：读取失败本身就是断言失败。
    fn counts(&self) -> (i64, i64) {
        let reader = spec_support::reader(&self.db_path).unwrap();
        (
            spec_support::count(&reader, "store_events").unwrap(),
            spec_support::count(&reader, "processed_commands").unwrap(),
        )
    }
}

// 主数据接口「物料」：快照、行、items 与 item_units 投影一致；units 按 unit_code 拆成 item_units 的行，不含基本单位。
#[tokio::test]
async fn create_projects_units_and_shelf_life() {
    let node = node();

    let reply = node.create(&flour(&cmd(1))).await;

    let data = assert_success(&reply);
    let id = data["item"]["item_id"].as_str().unwrap().to_owned();
    assert_eq!(
        data,
        &json!({ "item": {
            "item_id": id, "code": "FLOUR", "name": "高筋面粉", "base_unit": "g", "category": "RAW",
            "default_shelf_life_ms": HALF_YEAR,
            "units": [
                { "unit_code": "bag", "base_qty_per_unit": 25000 },
                { "unit_code": "cup", "base_qty_per_unit": 120 },
            ],
            "active": true, "revision": 1,
        } })
    );
    assert_eq!(
        node.items().unwrap(),
        [(
            id.clone(),
            "FLOUR".into(),
            "高筋面粉".into(),
            "g".into(),
            "RAW".into(),
            Some(HALF_YEAR),
            1,
            1
        )]
    );
    assert_eq!(
        node.units().unwrap(),
        [(id.clone(), "bag".into(), 25000), (id, "cup".into(), 120)]
    );
}

// 主数据接口「物料」修改：base_unit 与 code 取当前值；units 整体替换（改系数、删单位、加单位，可以改为空数组），
// 省略 default_shelf_life_ms 表示没有默认保质期——快照、行中没有该键，投影为 NULL。
#[tokio::test]
async fn update_replaces_units_and_clears_shelf_life() {
    let node = node();
    let id = node.create_ok(&flour(&cmd(1))).await;

    let reply = node
        .update(
            &id,
            &json!({
                "command_id": cmd(2), "base_revision": 1, "name": "高筋面粉", "category": "SEMI",
                "units": [
                    { "unit_code": "bag", "base_qty_per_unit": 20000 },
                    { "unit_code": "box", "base_qty_per_unit": 1000 },
                ],
                "active": true,
            }),
        )
        .await;

    let snapshot = json!({
        "code": "FLOUR", "name": "高筋面粉", "base_unit": "g", "category": "SEMI",
        "units": [
            { "unit_code": "bag", "base_qty_per_unit": 20000 },
            { "unit_code": "box", "base_qty_per_unit": 1000 },
        ],
        "active": true,
    });
    let mut row = snapshot.clone();
    row["item_id"] = json!(id);
    row["revision"] = json!(2);
    assert_eq!(assert_success(&reply), &json!({ "item": row }));
    assert_eq!(
        node.payloads().unwrap()[1],
        json!({ "entity": "ITEM", "source": "LOCAL", "snapshot": snapshot })
    );
    assert_eq!(node.items().unwrap()[0].5, None);
    assert_eq!(
        node.units().unwrap(),
        [
            (id.clone(), "bag".into(), 20000),
            (id.clone(), "box".into(), 1000)
        ]
    );

    // units 改为空数组：删除该物料的全部 item_units 行。
    let reply = node
        .update(
            &id,
            &json!({
                "command_id": cmd(3), "base_revision": 2, "name": "高筋面粉", "category": "SEMI",
                "units": [], "active": true,
            }),
        )
        .await;
    assert_eq!(assert_success(&reply)["item"]["units"], json!([]));
    assert!(node.units().unwrap().is_empty());
}

// 主数据接口「物料」：units 可以为空数组；unit_code 按字节序严格升序即可（大写在小写之前），
// 首尾以外的空白和非 ASCII 文本原样保存。
#[tokio::test]
async fn units_may_be_empty_and_are_ordered_by_bytes() {
    let node = node();

    let egg = node
        .create_ok(&json!({
            "command_id": cmd(1), "code": "EGG", "name": "鸡蛋", "base_unit": "pcs",
            "category": "RAW", "units": [], "active": true,
        }))
        .await;
    let cream = node
        .create_ok(&json!({
            "command_id": cmd(2), "code": "CREAM", "name": "淡奶油", "base_unit": "ml",
            "category": "SEMI",
            "units": [
                { "unit_code": "Box", "base_qty_per_unit": 12000 },
                { "unit_code": "box", "base_qty_per_unit": 1000 },
                { "unit_code": "大 盒", "base_qty_per_unit": 2000 },
            ],
            "active": true,
        }))
        .await;

    assert_eq!(
        node.units()
            .unwrap()
            .into_iter()
            .filter(|u| u.0 == egg)
            .count(),
        0
    );
    let codes: Vec<String> = node
        .units()
        .unwrap()
        .into_iter()
        .filter(|u| u.0 == cream)
        .map(|u| u.1)
        .collect();
    assert_eq!(codes, ["Box", "box", "大 盒"]);
    let reply = spec_support::get(&node.router, URI, Some(&STAFF))
        .await
        .unwrap();
    let items = &assert_success(&reply)["items"];
    assert_eq!(items[0]["code"], json!("CREAM"));
    assert_eq!(items[1]["units"], json!([]));
    assert!(items[1].get("default_shelf_life_ms").is_none(), "{items}");
}

// domain「主数据」：ITEM 的 code 只能由 A–Z、0–9 和 _ 组成（它是批次号的一段）。小写、混合大小写、-、内部空格、中文、全角字母、
// 其他标点都是 400 VALIDATION_FAILED，不落库；只有 _ 或只有数字的编码照常受理。其他实体的 code 不受此限（见 spec_master_data.rs）。
#[tokio::test]
async fn item_code_uses_only_uppercase_letters_digits_and_underscores() {
    let node = node();
    let before = (node.counts(), node.items().unwrap());

    for code in [
        "flour",
        "Flour",
        "FL-OUR",
        "FL OUR",
        "面粉",
        "ＦＬＯＵＲ",
        "FLOUR.1",
        "FLOUR/1",
        "É",
    ] {
        let mut body = flour(&cmd(1));
        body["code"] = json!(code);
        let reply = node.create(&body).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }
    assert_eq!((node.counts(), node.items().unwrap()), before);

    for (n, code) in ["A_1", "_", "0", "FLOUR_T65", "T65"].iter().enumerate() {
        let mut body = flour(&cmd(n as u16 + 2));
        body["code"] = json!(code);
        let id = node.create_ok(&body).await;
        assert!(
            node.items()
                .unwrap()
                .iter()
                .any(|row| row.0 == id && row.1 == *code),
            "{code}"
        );
    }
}

// 主数据接口「物料」取值：base_unit、category 不在枚举内（区分大小写）；default_shelf_life_ms 不是正整数或为 null；
// units 缺失、为 null、不是数组；单位项缺字段、多字段、unit_code 为空或首尾有空白、等于 base_unit、重复或未按字节序升序；
// base_qty_per_unit 不是正整数。新建与修改一律 400 VALIDATION_FAILED，不落库。修改不接受 base_unit、code。
#[tokio::test]
async fn invalid_item_fields_are_rejected() {
    let node = node();
    let id = node.create_ok(&flour(&cmd(1))).await;
    let before = (node.counts(), node.items().unwrap(), node.units().unwrap());

    let create = flour(&cmd(2));
    let mut update = create.clone();
    for key in ["code", "base_unit"] {
        update.as_object_mut().unwrap().remove(key);
    }
    update["base_revision"] = json!(1);

    let unit = |code: &str, qty: Value| json!({ "unit_code": code, "base_qty_per_unit": qty });
    let mut field_cases: Vec<(&str, Value)> = vec![
        ("category", json!("raw")),
        ("category", json!("PACKAGING")),
        ("category", Value::Null),
        ("default_shelf_life_ms", json!(0)),
        ("default_shelf_life_ms", json!(-1)),
        ("default_shelf_life_ms", Value::Null),
        ("default_shelf_life_ms", json!("86400000")),
        ("default_shelf_life_ms", json!(1.5)),
        ("units", Value::Null),
        ("units", json!({})),
        ("units", json!("bag")),
        ("units", json!([{ "unit_code": "bag" }])),
        ("units", json!([{ "base_qty_per_unit": 25000 }])),
        (
            "units",
            json!([{ "unit_code": "bag", "base_qty_per_unit": 25000, "note": "x" }]),
        ),
        ("units", json!([unit("", json!(1000))])),
        ("units", json!([unit(" bag", json!(1000))])),
        ("units", json!([unit("bag ", json!(1000))])),
        ("units", json!([unit("g", json!(1000))])), // 等于 base_unit
        ("units", json!([unit("bag", json!(0))])),
        ("units", json!([unit("bag", json!(-1))])),
        ("units", json!([unit("bag", json!(1.5))])),
        ("units", json!([unit("bag", json!("1000"))])),
        ("units", json!([unit("bag", Value::Null)])),
        (
            "units",
            json!([unit("bag", json!(1000)), unit("bag", json!(2000))]),
        ),
        (
            "units",
            json!([unit("box", json!(1000)), unit("bag", json!(2000))]),
        ),
        (
            "units",
            json!([unit("box", json!(1000)), unit("Box", json!(2000))]),
        ),
    ];
    let mut creates: Vec<Value> = Vec::new();
    let mut updates: Vec<Value> = Vec::new();
    for (key, value) in field_cases.drain(..) {
        let mut body = create.clone();
        body[key] = value.clone();
        creates.push(body);
        let mut body = update.clone();
        body[key] = value;
        updates.push(body);
    }
    for value in [json!("G"), json!("kg"), json!(""), Value::Null, json!(1)] {
        let mut body = create.clone();
        body["base_unit"] = value;
        creates.push(body);
    }
    for key in ["base_unit", "category", "units"] {
        let mut body = create.clone();
        body.as_object_mut().unwrap().remove(key);
        creates.push(body);
    }
    for key in ["category", "units"] {
        let mut body = update.clone();
        body.as_object_mut().unwrap().remove(key);
        updates.push(body);
    }
    for (key, value) in [("base_unit", json!("g")), ("code", json!("FLOUR"))] {
        let mut body = update.clone();
        body[key] = value;
        updates.push(body);
    }

    for body in &creates {
        let reply = node.create(body).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }
    for body in &updates {
        let reply = node.update(&id, body).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }

    assert_eq!(
        (node.counts(), node.items().unwrap(), node.units().unwrap()),
        before
    );
}

// domain「主数据」：unit_code 只在同一物料内唯一，不同物料可以有同名的单位且系数各自独立。
#[tokio::test]
async fn unit_codes_are_scoped_per_item() {
    let node = node();
    let flour_id = node.create_ok(&flour(&cmd(1))).await;

    let sugar_id = node
        .create_ok(&json!({
            "command_id": cmd(2), "code": "SUGAR", "name": "白砂糖", "base_unit": "g",
            "category": "RAW", "units": [{ "unit_code": "bag", "base_qty_per_unit": 1000 }],
            "active": true,
        }))
        .await;

    let bags: Vec<(String, i64)> = node
        .units()
        .unwrap()
        .into_iter()
        .filter(|u| u.1 == "bag")
        .map(|u| (u.0, u.2))
        .collect();
    let mut expected = vec![(flour_id, 25000), (sugar_id, 1000)];
    expected.sort();
    assert_eq!(bags, expected);
}
