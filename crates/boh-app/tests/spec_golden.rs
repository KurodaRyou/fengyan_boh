//! 锁定测试：golden payload。每个 `event_type@schema_version` 的每种 payload 结构分支一份样本，
//! 比对 `store_events.payload` 的完整序列化文本（键顺序、转义都锁定）。规则见 AGENTS.md「测试分工」「只追加」。

mod spec_support;

use std::error::Error;
use std::path::Path;

use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use serde_json::json;
use spec_support::{MANAGER, assert_success};

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
