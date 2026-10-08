//! 锁定测试：golden payload。每个 `event_type@schema_version` 的每种 payload 结构分支一份样本，
//! 比对 `store_events.payload` 的完整序列化文本（键顺序、转义都锁定）。规则见 AGENTS.md「测试分工」「只追加」。

mod spec_support;

use std::error::Error;
use std::path::Path;

use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use serde_json::json;
use spec_support::{MANAGER, STAFF, assert_success};

const NOW: UnixMillis = UnixMillis(1_791_248_400_000); // 2026-10-06 09:00 +08:00

fn golden(text: &str) -> &str {
    text.strip_suffix('\n').unwrap_or(text)
}

fn payloads(db_path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let reader = spec_support::reader(db_path)?;
    let mut statement = reader.prepare("SELECT payload FROM store_events ORDER BY seq")?;
    let payloads = statement
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(payloads)
}

// MASTER_DATA_CHANGED@1，entity = EQUIPMENT，source = LOCAL：新建与修改的 payload 结构相同；非 ASCII 文本不转义。
#[tokio::test]
async fn master_data_changed_equipment() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let router = spec_support::router(&db_path, ManualClock::new(NOW).clock()).unwrap();

    let reply = spec_support::post(
        &router,
        "/api/v1/equipment",
        Some(&MANAGER),
        &json!({
            "command_id": "01890a5d-ac96-774b-bcce-b30209a90001",
            "code": "F1", "name": "冷藏柜 1", "equipment_type": "FRIDGE", "active": true,
        }),
    )
    .await
    .unwrap();
    let id = assert_success(&reply)["equipment"]["equipment_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let reply = spec_support::put(
        &router,
        &format!("/api/v1/equipment/{id}"),
        Some(&MANAGER),
        &json!({
            "command_id": "01890a5d-ac96-774b-bcce-b30209a90002",
            "base_revision": 1, "name": "急冻柜 \"B\"", "equipment_type": "BLAST_FREEZER", "active": false,
        }),
    )
    .await
    .unwrap();
    assert_success(&reply);

    assert_eq!(
        payloads(&db_path).unwrap(),
        [
            golden(include_str!("golden/MASTER_DATA_CHANGED@1/EQUIPMENT.json")),
            r#"{"entity":"EQUIPMENT","source":"LOCAL","snapshot":{"code":"F1","name":"急冻柜 \"B\"","equipment_type":"BLAST_FREEZER","active":false}}"#,
        ]
    );
}

/// 温度记录 golden 样本中的设备 ID。设备 ID 由服务端生成，比对前把实际 ID 替换成它，其余文本逐字节比对。
const GOLDEN_EQUIPMENT_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8301";

// TEMPERATURE_LOGGED@1：note 出现与省略是两种结构分支，各一份样本；省略时不写 null。
// 负温度、非 ASCII 文本不转义、引号转义一并锁定。
#[tokio::test]
async fn temperature_logged_with_and_without_note() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let router = spec_support::router(&db_path, ManualClock::new(NOW).clock()).unwrap();
    let reply = spec_support::post(
        &router,
        "/api/v1/equipment",
        Some(&MANAGER),
        &json!({
            "command_id": "01890a5d-ac96-774b-bcce-b30209a90001",
            "code": "F1", "name": "冷藏柜 1", "equipment_type": "FRIDGE", "active": true,
        }),
    )
    .await
    .unwrap();
    let id = assert_success(&reply)["equipment"]["equipment_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for (command_id, celsius_x10, note) in [
        (
            "01890a5d-ac96-774b-bcce-b30209a90002",
            -185,
            Some("门封条结霜 \"B\""),
        ),
        ("01890a5d-ac96-774b-bcce-b30209a90003", 38, None),
    ] {
        let mut body = json!({
            "command_id": command_id, "equipment_id": id, "celsius_x10": celsius_x10,
            "captured_at": NOW.0 - 60_000, "sent_at": NOW.0,
        });
        if let Some(note) = note {
            body["note"] = json!(note);
        }
        let reply =
            spec_support::post(&router, "/api/v1/temperature-readings", Some(&STAFF), &body)
                .await
                .unwrap();
        assert_success(&reply);
    }

    let payloads: Vec<String> = payloads(&db_path).unwrap()[1..]
        .iter()
        .map(|payload| payload.replace(&id, GOLDEN_EQUIPMENT_ID))
        .collect();
    assert_eq!(
        payloads,
        [
            golden(include_str!("golden/TEMPERATURE_LOGGED@1/WITH_NOTE.json")),
            golden(include_str!(
                "golden/TEMPERATURE_LOGGED@1/WITHOUT_NOTE.json"
            )),
        ]
    );
}

/// 配方 golden 样本中的物料 ID：比对前把服务端生成的实际 ID 替换成它们，其余文本逐字节比对。
const GOLDEN_FLOUR_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8501";
const GOLDEN_TOAST_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8502";
const GOLDEN_EGG_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8503";

#[allow(clippy::unwrap_used)] // 测试夹具：写入样本数据的请求失败时，比对没有意义，直接终止测试。
async fn post_ok(router: &axum::Router, uri: &str, body: serde_json::Value) -> serde_json::Value {
    let reply = spec_support::post(router, uri, Some(&MANAGER), &body)
        .await
        .unwrap();
    assert_success(&reply).clone()
}

// MASTER_DATA_CHANGED@1，entity = ITEM / RECIPE / SUPPLIER / WASTE_REASON，source = LOCAL：
// ITEM 的 default_shelf_life_ms 出现与省略、SUPPLIER 的 contact_phone 出现与省略各一份样本，省略时不写 null；
// ITEM 的 units 为空数组时照常写出；RECIPE 新建（一个版本）一份样本，追加版本后的快照含全部版本。
// 键顺序为 domain.md「主数据」快照字段的顺序；非 ASCII 文本不转义、引号转义一并锁定。
#[tokio::test]
async fn master_data_changed_items_recipes_suppliers_and_waste_reasons() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let router = spec_support::router(&db_path, ManualClock::new(NOW).clock()).unwrap();
    let cmd = |n: u16| format!("01890a5d-ac96-774b-bcce-b30209b1{n:04x}");

    let flour = post_ok(
        &router,
        "/api/v1/items",
        json!({
            "command_id": cmd(1), "code": "FLOUR", "name": "高筋面粉 \"T65\"", "base_unit": "g",
            "category": "RAW", "default_shelf_life_ms": 15_552_000_000_i64,
            "units": [
                { "unit_code": "bag", "base_qty_per_unit": 25000 },
                { "unit_code": "cup", "base_qty_per_unit": 120 },
            ],
            "active": true,
        }),
    )
    .await["item"]["item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let toast = post_ok(
        &router,
        "/api/v1/items",
        json!({
            "command_id": cmd(2), "code": "TOAST", "name": "吐司", "base_unit": "pcs",
            "category": "FINISHED", "units": [], "active": true,
        }),
    )
    .await["item"]["item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let egg = post_ok(
        &router,
        "/api/v1/items",
        json!({
            "command_id": cmd(3), "code": "EGG", "name": "鸡蛋", "base_unit": "pcs",
            "category": "RAW", "units": [], "active": true,
        }),
    )
    .await["item"]["item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let recipe = post_ok(
        &router,
        "/api/v1/recipes",
        json!({
            "command_id": cmd(4), "code": "R-TOAST", "name": "吐司", "output_item_id": toast,
            "active": true, "output_qty_per_batch": 12,
            "lines": [
                { "item_id": flour, "qty_per_batch": 3000 },
                { "item_id": egg, "qty_per_batch": 6 },
            ],
        }),
    )
    .await["recipe"]["recipe_id"]
        .as_str()
        .unwrap()
        .to_owned();
    post_ok(
        &router,
        &format!("/api/v1/recipes/{recipe}/versions"),
        json!({
            "command_id": cmd(5), "base_revision": 1, "output_qty_per_batch": 10,
            "lines": [{ "item_id": egg, "qty_per_batch": 8 }],
        }),
    )
    .await;
    post_ok(
        &router,
        "/api/v1/suppliers",
        json!({
            "command_id": cmd(6), "code": "S-FLOUR", "name": "面粉供应商",
            "contact_phone": "021-5555 0101", "active": true,
        }),
    )
    .await;
    post_ok(
        &router,
        "/api/v1/suppliers",
        json!({ "command_id": cmd(7), "code": "S-EGG", "name": "鸡蛋供应商", "active": false }),
    )
    .await;
    post_ok(
        &router,
        "/api/v1/waste-reasons",
        json!({ "command_id": cmd(8), "code": "EXPIRED", "name": "过期", "active": true }),
    )
    .await;

    let payloads: Vec<String> = payloads(&db_path)
        .unwrap()
        .iter()
        .map(|payload| {
            payload
                .replace(&flour, GOLDEN_FLOUR_ID)
                .replace(&toast, GOLDEN_TOAST_ID)
                .replace(&egg, GOLDEN_EGG_ID)
        })
        .collect();
    assert_eq!(
        payloads,
        [
            golden(include_str!("golden/MASTER_DATA_CHANGED@1/ITEM.json")),
            golden(include_str!(
                "golden/MASTER_DATA_CHANGED@1/ITEM_WITHOUT_DEFAULT_SHELF_LIFE.json"
            )),
            r#"{"entity":"ITEM","source":"LOCAL","snapshot":{"code":"EGG","name":"鸡蛋","base_unit":"pcs","category":"RAW","units":[],"active":true}}"#,
            golden(include_str!("golden/MASTER_DATA_CHANGED@1/RECIPE.json")),
            r#"{"entity":"RECIPE","source":"LOCAL","snapshot":{"code":"R-TOAST","name":"吐司","output_item_id":"01890a5d-ac96-774b-bcce-b302099a8502","versions":[{"version":1,"output_qty_per_batch":12,"lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8501","qty_per_batch":3000},{"item_id":"01890a5d-ac96-774b-bcce-b302099a8503","qty_per_batch":6}]},{"version":2,"output_qty_per_batch":10,"lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8503","qty_per_batch":8}]}],"active":true}}"#,
            golden(include_str!("golden/MASTER_DATA_CHANGED@1/SUPPLIER.json")),
            golden(include_str!(
                "golden/MASTER_DATA_CHANGED@1/SUPPLIER_WITHOUT_CONTACT_PHONE.json"
            )),
            golden(include_str!(
                "golden/MASTER_DATA_CHANGED@1/WASTE_REASON.json"
            )),
        ]
    );
}

/// 收货 golden 样本中的供应商 ID 和批次 ID（物料 ID 沿用 `GOLDEN_FLOUR_ID`）：比对前把服务端生成的实际 ID 替换成它们。
const GOLDEN_SUPPLIER_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8601";
const GOLDEN_LOT_IDS: [&str; 2] = [
    "01890a5d-ac96-774b-bcce-b302099a8701",
    "01890a5d-ac96-774b-bcce-b302099a8702",
];

// GOODS_RECEIVED@1：行的 manufacturer_lot_no 出现与省略各一份样本，省略时不写 null；键顺序见 domain.md「收货接口」。
// expires_at 是 expires_on 在 Asia/Shanghai 的当日最后一毫秒；非 ASCII 文本不转义、引号转义一并锁定。
// 被吸收分支（absorbed_by_event_id 代替 lot_id）的样本随盘点切片加入：在那之前没有写入口能产生它。
#[tokio::test]
async fn goods_received_with_and_without_manufacturer_lot_no() {
    let cmd = |n: u16| format!("01890a5d-ac96-774b-bcce-b30209b2{n:04x}");
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let router = spec_support::router(&db_path, ManualClock::new(NOW).clock()).unwrap();
    let flour = post_ok(
        &router,
        "/api/v1/items",
        json!({
            "command_id": cmd(1), "code": "FLOUR", "name": "面粉", "base_unit": "g",
            "category": "RAW", "units": [{ "unit_code": "袋", "base_qty_per_unit": 25000 }],
            "active": true,
        }),
    )
    .await["item"]["item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let supplier = post_ok(
        &router,
        "/api/v1/suppliers",
        json!({ "command_id": cmd(2), "code": "S-FLOUR", "name": "面粉供应商", "active": true }),
    )
    .await["supplier"]["supplier_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut lots = Vec::new();
    for (command_id, line) in [
        (
            cmd(3),
            json!({
                "item_id": flour, "input": { "qty": 2, "unit_code": "袋", "base_qty_per_unit": 25000 },
                "manufacturer_lot_no": "批号 \"A\"-1", "produced_on": "2026-10-01",
                "expires_on": "2026-10-20", "line_cost_cents": 12345,
            }),
        ),
        (
            cmd(4),
            json!({
                "item_id": flour, "input": { "qty": 3000, "unit_code": "g", "base_qty_per_unit": 1 },
                "produced_on": "2026-10-06", "expires_on": "2026-10-06", "line_cost_cents": 0,
            }),
        ),
    ] {
        let reply = spec_support::post(
            &router,
            "/api/v1/receipts",
            Some(&STAFF),
            &json!({
                "command_id": command_id, "supplier_id": supplier, "lines": [line],
                "captured_at": NOW.0 - 60_000, "sent_at": NOW.0,
            }),
        )
        .await
        .unwrap();
        lots.push(
            assert_success(&reply)["receipt"]["lines"][0]["lot_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }

    let payloads: Vec<String> = payloads(&db_path).unwrap()[2..]
        .iter()
        .map(|payload| {
            payload
                .replace(&flour, GOLDEN_FLOUR_ID)
                .replace(&supplier, GOLDEN_SUPPLIER_ID)
                .replace(&lots[0], GOLDEN_LOT_IDS[0])
                .replace(&lots[1], GOLDEN_LOT_IDS[1])
        })
        .collect();
    assert_eq!(
        payloads,
        [
            golden(include_str!(
                "golden/GOODS_RECEIVED@1/WITH_MANUFACTURER_LOT_NO.json"
            )),
            golden(include_str!(
                "golden/GOODS_RECEIVED@1/WITHOUT_MANUFACTURER_LOT_NO.json"
            )),
        ]
    );
}
