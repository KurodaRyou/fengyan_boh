//! 锁定测试：重放一致性。清空投影、按 seq 重建，结果与在线写入逐行一致。
//! 规则见 AGENTS.md「只追加」，接口见 docs/interfaces.md（`rebuild_projections`、投影表的范围）。

mod spec_support;

use std::error::Error;
use std::path::Path;

use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use boh_storage::rusqlite::types::Value as SqlValue;
use boh_storage::rusqlite::{self, Connection};
use serde_json::{Value, json};
use spec_support::{MANAGER, STAFF, assert_error, assert_success};

const NOW: UnixMillis = UnixMillis(1_791_248_400_000); // 2026-10-06 09:00 +08:00
/// 不是投影的表：账本、幂等记录、门店身份。认证状态表随认证切片加入此清单。
const NOT_PROJECTIONS: [&str; 3] = ["store_meta", "processed_commands", "store_events"];

fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209a9{n:04x}")
}

type Snapshot = Vec<(String, Vec<Vec<SqlValue>>)>;

fn projection_tables(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
    )?;
    let names = statement
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names
        .into_iter()
        .filter(|name| !NOT_PROJECTIONS.contains(&name.as_str()))
        .collect())
}

/// 整表内容，按全部列排序，便于逐行比对。
fn table(conn: &Connection, name: &str) -> rusqlite::Result<Vec<Vec<SqlValue>>> {
    let columns = conn
        .prepare(&format!("SELECT * FROM \"{name}\""))?
        .column_count();
    let order: Vec<String> = (1..=columns).map(|i| i.to_string()).collect();
    spec_support::rows(
        conn,
        &format!("SELECT * FROM \"{name}\" ORDER BY {}", order.join(", ")),
    )
}

fn snapshot(conn: &Connection, tables: &[String]) -> rusqlite::Result<Snapshot> {
    tables
        .iter()
        .map(|name| Ok((name.clone(), table(conn, name)?)))
        .collect()
}

fn ledger(db_path: &Path) -> Result<Snapshot, Box<dyn Error>> {
    let reader = spec_support::reader(db_path)?;
    let names: Vec<String> = NOT_PROJECTIONS.iter().map(|s| (*s).to_owned()).collect();
    Ok(snapshot(&reader, &names)?)
}

fn projections(db_path: &Path) -> Result<Snapshot, Box<dyn Error>> {
    let reader = spec_support::reader(db_path)?;
    let tables = projection_tables(&reader)?;
    assert!(tables.contains(&"equipment".to_owned()), "{tables:?}");
    Ok(snapshot(&reader, &tables)?)
}

#[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态的请求失败时，后续步骤没有意义，直接终止测试。
async fn create(router: &axum::Router, command_id: &str, code: &str) -> String {
    let reply = spec_support::post(
        router,
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

#[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态的请求失败时，后续步骤没有意义，直接终止测试。
async fn update(
    router: &axum::Router,
    id: &str,
    command_id: &str,
    base_revision: i64,
    name: &str,
    equipment_type: &str,
    active: bool,
) -> Value {
    let reply = spec_support::put(
        router,
        &format!("/api/v1/equipment/{id}"),
        Some(&MANAGER),
        &json!({
            "command_id": command_id, "base_revision": base_revision, "name": name,
            "equipment_type": equipment_type, "active": active,
        }),
    )
    .await
    .unwrap();
    assert_success(&reply).clone()
}

// 在线写入若干事件后篡改投影（删行、改行、插入多余的行），重建后投影与在线写入逐行一致，
// 账本、幂等记录和 store_meta 不变；重建可重复执行；重建后的投影能继续支撑新的写命令。
#[tokio::test]
async fn rebuild_restores_projections_written_online() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(NOW);
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let a = create(&router, &cmd(1), "F1").await;
    let b = create(&router, &cmd(2), "F2").await;
    update(&router, &a, &cmd(3), 1, "Walk-in A", "FREEZER", true).await;
    update(&router, &b, &cmd(4), 1, "Walk-in", "FRIDGE", false).await;
    update(&router, &a, &cmd(5), 2, "Walk-in A", "FREEZER", true).await; // 内容不变，不写事件
    create(&router, &cmd(6), "F3").await;
    update(&router, &a, &cmd(7), 2, "Walk-in A", "OVEN", true).await;
    drop(router);
    let online = projections(&db_path).unwrap();
    let ledger_before = ledger(&db_path).unwrap();

    let mut writer = spec_support::writer(&db_path).unwrap();
    writer
        .execute_batch(
            "DELETE FROM equipment WHERE code = 'F2';
             UPDATE equipment SET name = 'tampered', revision = 9 WHERE code = 'F1';
             INSERT INTO equipment (id, code, name, equipment_type, active, revision)
             VALUES ('01890a5d-ac96-774b-bcce-b302099a8999', 'X9', 'stray', 'OTHER', 1, 1);",
        )
        .unwrap();
    assert_ne!(projections(&db_path).unwrap(), online);

    for _ in 0..2 {
        assert_eq!(spec_support::rebuild_projections(&mut writer).unwrap(), 6);
        assert_eq!(projections(&db_path).unwrap(), online);
        assert_eq!(ledger(&db_path).unwrap(), ledger_before);
    }
    drop(writer);

    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let data = update(&router, &a, &cmd(8), 3, "Walk-in A", "OVEN", false).await;
    assert_eq!(data["equipment"]["revision"], json!(4));
    let reply = spec_support::post(
        &router,
        "/api/v1/equipment",
        Some(&MANAGER),
        &json!({
            "command_id": cmd(9), "code": "F2", "name": "Walk-in",
            "equipment_type": "FRIDGE", "active": true,
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        assert_error(&reply, 409, "CODE_ALREADY_EXISTS"),
        &json!({ "code": "F2", "equipment_id": b })
    );
}

// 空账本重建：返回 0，投影表被清空。
#[tokio::test]
async fn rebuild_of_an_empty_ledger_clears_projections() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    drop(spec_support::router(&db_path, ManualClock::new(NOW).clock()).unwrap());
    let mut writer = spec_support::writer(&db_path).unwrap();
    writer
        .execute(
            "INSERT INTO equipment (id, code, name, equipment_type, active, revision)
             VALUES ('01890a5d-ac96-774b-bcce-b302099a8999', 'X9', 'stray', 'OTHER', 1, 1)",
            [],
        )
        .unwrap();

    assert_eq!(spec_support::rebuild_projections(&mut writer).unwrap(), 0);

    for (name, rows) in projections(&db_path).unwrap() {
        assert!(rows.is_empty(), "{name}: {rows:?}");
    }
}

// interfaces「Writer::rebuild_projections」失败时回滚：账本中夹着一条 apply 无法处理的事件
// （EQUIPMENT 快照的 equipment_type 不在枚举内，由测试直接写入账本），重建在它之前已应用了若干事件，
// 遇到它时返回错误；全部投影保持重建前的状态（包括重建前被篡改的内容），账本、processed_commands、store_meta 不变。
#[tokio::test]
async fn failed_rebuild_rolls_back_every_projection() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(NOW);
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let a = create(&router, &cmd(1), "F1").await;
    create(&router, &cmd(2), "F2").await;
    update(&router, &a, &cmd(3), 1, "Walk-in A", "FREEZER", true).await;
    drop(router);

    let mut writer = spec_support::writer(&db_path).unwrap();
    let tx = writer.transaction().unwrap();
    tx.execute(
        "INSERT INTO processed_commands (command_id, command_type, request, response, recorded_at)
         VALUES (?1, 'equipment.create', '{}', '{}', ?2)",
        rusqlite::params![cmd(90), NOW.0],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO store_events (id, event_type, schema_version, aggregate_type, aggregate_id,
             aggregate_version, command_id, actor_id, device_id, business_date,
             occurred_at, recorded_at, payload)
         VALUES (?1, 'MASTER_DATA_CHANGED', 1, 'EQUIPMENT', ?1, 1, ?2, ?3, ?4, '2026-10-06', ?5, ?5,
             '{\"entity\":\"EQUIPMENT\",\"source\":\"LOCAL\",\"snapshot\":{\"code\":\"F9\",\"name\":\"Bad\",\"equipment_type\":\"COOLER\",\"active\":true}}')",
        rusqlite::params![
            "01890a5d-ac96-774b-bcce-b302099a8990",
            cmd(90),
            MANAGER.employee_id,
            MANAGER.device_id,
            NOW.0
        ],
    )
    .unwrap();
    tx.commit().unwrap();
    drop(writer);

    // 坏事件之后再在线写入一条，坏事件位于账本中间。
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    create(&router, &cmd(4), "F3").await;
    drop(router);

    let mut writer = spec_support::writer(&db_path).unwrap();
    writer
        .execute_batch(
            "DELETE FROM equipment WHERE code = 'F2';
             INSERT INTO equipment (id, code, name, equipment_type, active, revision)
             VALUES ('01890a5d-ac96-774b-bcce-b302099a8999', 'X9', 'stray', 'OTHER', 1, 1);",
        )
        .unwrap();
    let projections_before = projections(&db_path).unwrap();
    let ledger_before = ledger(&db_path).unwrap();

    assert!(spec_support::rebuild_projections(&mut writer).is_err());

    assert_eq!(projections(&db_path).unwrap(), projections_before);
    assert_eq!(ledger(&db_path).unwrap(), ledger_before);
}

/// 平板发送时刻的平板本地时钟。
const SENT: i64 = NOW.0 + 7 * 60_000;

/// 记录一条温度读数：平板在 `captured_at` 录入、在 `SENT` 发送。返回完整响应。
#[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试。
async fn log(
    router: &axum::Router,
    command_id: &str,
    equipment_id: &str,
    celsius_x10: i64,
    note: Option<&str>,
    captured_at: i64,
    sent_at: i64,
) -> spec_support::JsonReply {
    let mut body = json!({
        "command_id": command_id, "equipment_id": equipment_id, "celsius_x10": celsius_x10,
        "captured_at": captured_at, "sent_at": sent_at,
    });
    if let Some(note) = note {
        body["note"] = json!(note);
    }
    let reply = spec_support::post(router, "/api/v1/temperature-readings", Some(&STAFF), &body)
        .await
        .unwrap();
    assert_success(&reply);
    reply
}

// 设备与温度记录混合的账本：在线写入后篡改两张投影（删行、改行、插入多余的行），重建后全部投影与在线写入逐行一致，
// 账本、processed_commands 和 store_meta 不变；重建可重复执行。
// 重建不影响幂等回执：之后用原 command_id、原内容（sent_at 不同）重发读数，原样返回首次响应（含 warnings），不新增事件。
// 账本中包含停用设备上的读数、带 / 不带 note 的读数和时钟回拨后的读数。
#[tokio::test]
async fn rebuild_restores_equipment_and_temperature_projections() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(NOW);
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let a = create(&router, &cmd(1), "F1").await;
    let b = create(&router, &cmd(2), "F2").await;
    log(
        &router,
        &cmd(3),
        &a,
        -185,
        Some("门封条结霜"),
        SENT - 600_000,
        SENT,
    )
    .await;
    update(&router, &a, &cmd(4), 1, "Walk-in A", "FREEZER", true).await;
    // 负 lag：首次响应带 CAPTURE_TIME_ADJUSTED。
    let first = log(&router, &cmd(5), &b, 38, None, SENT + 60_000, SENT).await;
    assert_eq!(
        first.body["warnings"][0]["code"],
        json!("CAPTURE_TIME_ADJUSTED")
    );
    update(&router, &b, &cmd(6), 1, "Walk-in", "FRIDGE", false).await;
    log(&router, &cmd(7), &b, 41, None, SENT, SENT).await;
    clock.set(UnixMillis(NOW.0 - 2 * 86_400_000));
    log(
        &router,
        &cmd(8),
        &a,
        -180,
        Some("回拨"),
        SENT - 60_000,
        SENT,
    )
    .await;
    drop(router);
    let online = projections(&db_path).unwrap();
    assert!(
        online
            .iter()
            .any(|(name, rows)| name == "temperature_readings" && rows.len() == 4)
    );
    let ledger_before = ledger(&db_path).unwrap();

    let mut writer = spec_support::writer(&db_path).unwrap();
    writer
        .execute_batch(&format!(
            "DELETE FROM temperature_readings WHERE celsius_x10 = 38;
             UPDATE temperature_readings SET celsius_x10 = 0, note = 'tampered' WHERE celsius_x10 = -185;
             UPDATE temperature_readings SET note = NULL WHERE celsius_x10 = -180;
             INSERT INTO temperature_readings (id, event_seq, equipment_id, celsius_x10, note, actor_id,
                 device_id, business_date, occurred_at, recorded_at)
             VALUES ('01890a5d-ac96-774b-bcce-b302099a8998', 1, '{a}', 0, 'stray', '{actor}', '{device}',
                 '2026-10-06', 0, 0);
             UPDATE equipment SET name = 'tampered' WHERE code = 'F2';",
            actor = MANAGER.employee_id,
            device = MANAGER.device_id,
        ))
        .unwrap();
    assert_ne!(projections(&db_path).unwrap(), online);

    for _ in 0..2 {
        assert_eq!(spec_support::rebuild_projections(&mut writer).unwrap(), 8);
        assert_eq!(projections(&db_path).unwrap(), online);
        assert_eq!(ledger(&db_path).unwrap(), ledger_before);
    }
    drop(writer);

    clock.set(UnixMillis(NOW.0 + 3_600_000));
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let retry = log(
        &router,
        &cmd(5),
        &b,
        38,
        None,
        SENT + 60_000,
        SENT + 120_000,
    )
    .await;
    assert_eq!(retry.body, first.body);
    assert_eq!(ledger(&db_path).unwrap(), ledger_before);
    assert_eq!(projections(&db_path).unwrap(), online);
}

#[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态的请求失败时，后续步骤没有意义，直接终止测试。
async fn write(router: &axum::Router, method: &str, uri: &str, body: Value) -> Value {
    let reply = if method == "PUT" {
        spec_support::put(router, uri, Some(&MANAGER), &body).await
    } else {
        spec_support::post(router, uri, Some(&MANAGER), &body).await
    }
    .unwrap();
    assert_success(&reply).clone()
}

// 物料、配方、供应商、报损原因混合的账本（含修改、追加版本、删除单位与联系电话、内容不变的修改）：
// 在线写入后篡改主数据投影并在库存投影中插入多余的行，重建后全部投影与在线写入逐行一致——库存投影在本切片没有事件写入，重建后为空；
// 账本、processed_commands 和 store_meta 不变；重建可重复执行；重建后的投影能继续支撑新的写命令。
#[tokio::test]
async fn rebuild_restores_master_data_projections() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(NOW);
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let flour = write(
        &router,
        "POST",
        "/api/v1/items",
        json!({
            "command_id": cmd(1), "code": "FLOUR", "name": "高筋面粉", "base_unit": "g",
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
    let toast = write(
        &router,
        "POST",
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
    let recipe = write(
        &router,
        "POST",
        "/api/v1/recipes",
        json!({
            "command_id": cmd(3), "code": "R-TOAST", "name": "吐司", "output_item_id": toast,
            "active": true, "output_qty_per_batch": 12,
            "lines": [{ "item_id": flour, "qty_per_batch": 3000 }],
        }),
    )
    .await["recipe"]["recipe_id"]
        .as_str()
        .unwrap()
        .to_owned();
    write(
        &router,
        "PUT",
        &format!("/api/v1/items/{flour}"),
        json!({
            "command_id": cmd(4), "base_revision": 1, "name": "高筋面粉", "category": "RAW",
            "units": [{ "unit_code": "bag", "base_qty_per_unit": 20000 }], "active": false,
        }),
    )
    .await;
    write(
        &router,
        "POST",
        &format!("/api/v1/recipes/{recipe}/versions"),
        json!({
            "command_id": cmd(5), "base_revision": 1, "output_qty_per_batch": 10,
            "lines": [
                { "item_id": toast, "qty_per_batch": 1 },
                { "item_id": flour, "qty_per_batch": 2800 },
            ],
        }),
    )
    .await;
    write(
        &router,
        "PUT",
        &format!("/api/v1/recipes/{recipe}"),
        json!({ "command_id": cmd(6), "base_revision": 2, "name": "白吐司", "active": true }),
    )
    .await;
    let supplier = write(
        &router,
        "POST",
        "/api/v1/suppliers",
        json!({
            "command_id": cmd(7), "code": "S1", "name": "面粉供应商",
            "contact_phone": "021-5555 0101", "active": true,
        }),
    )
    .await["supplier"]["supplier_id"]
        .as_str()
        .unwrap()
        .to_owned();
    write(
        &router,
        "PUT",
        &format!("/api/v1/suppliers/{supplier}"),
        json!({ "command_id": cmd(8), "base_revision": 1, "name": "面粉供应商", "active": false }),
    )
    .await;
    let reason = write(
        &router,
        "POST",
        "/api/v1/waste-reasons",
        json!({ "command_id": cmd(9), "code": "EXPIRED", "name": "过期", "active": true }),
    )
    .await["waste_reason"]["waste_reason_id"]
        .as_str()
        .unwrap()
        .to_owned();
    write(
        &router,
        "PUT",
        &format!("/api/v1/waste-reasons/{reason}"),
        json!({ "command_id": cmd(10), "base_revision": 1, "name": "过期", "active": true }),
    )
    .await; // 内容不变，不写事件
    drop(router);
    let online = projections(&db_path).unwrap();
    for (table, rows) in [
        ("items", 2),
        ("item_units", 1),
        ("recipes", 1),
        ("recipe_versions", 2),
        ("recipe_lines", 3),
        ("suppliers", 1),
        ("waste_reasons", 1),
        ("inventory_unallocated", 0),
    ] {
        assert!(
            online
                .iter()
                .any(|(name, content)| name == table && content.len() == rows),
            "{table}: {online:?}"
        );
    }
    let ledger_before = ledger(&db_path).unwrap();

    let mut writer = spec_support::writer(&db_path).unwrap();
    writer
        .execute_batch(&format!(
            "DELETE FROM item_units;
             INSERT INTO item_units (item_id, unit_code, base_qty_per_unit) VALUES ('{toast}', 'box', 6);
             UPDATE items SET name = 'tampered', default_shelf_life_ms = 1 WHERE id = '{flour}';
             UPDATE recipe_lines SET qty_per_batch = 1 WHERE version = 2;
             DELETE FROM recipe_lines WHERE version = 1;
             INSERT INTO recipe_versions (recipe_id, version, output_qty_per_batch) VALUES ('{recipe}', 9, 1);
             UPDATE suppliers SET contact_phone = '000';
             DELETE FROM waste_reasons;
             INSERT INTO inventory_unallocated (item_id, qty) VALUES ('{flour}', -5);"
        ))
        .unwrap();
    assert_ne!(projections(&db_path).unwrap(), online);

    for _ in 0..2 {
        assert_eq!(spec_support::rebuild_projections(&mut writer).unwrap(), 9);
        assert_eq!(projections(&db_path).unwrap(), online);
        assert_eq!(ledger(&db_path).unwrap(), ledger_before);
    }
    drop(writer);

    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let data = write(
        &router,
        "PUT",
        &format!("/api/v1/recipes/{recipe}"),
        json!({ "command_id": cmd(11), "base_revision": 3, "name": "白吐司", "active": false }),
    )
    .await;
    assert_eq!(data["recipe"]["revision"], json!(4));
    assert_eq!(data["recipe"]["versions"].as_array().unwrap().len(), 2);
    let reply = spec_support::post(
        &router,
        "/api/v1/waste-reasons",
        Some(&MANAGER),
        &json!({ "command_id": cmd(12), "code": "EXPIRED", "name": "过期", "active": true }),
    )
    .await
    .unwrap();
    assert_eq!(
        assert_error(&reply, 409, "CODE_ALREADY_EXISTS"),
        &json!({ "code": "EXPIRED", "waste_reason_id": reason })
    );
}

// domain「主数据」：source = HQ_PACKAGE 的 MASTER_DATA_CHANGED 与 LOCAL 的一样重建投影（payload 见 golden 样本，
// 本期没有写入口，经 open_writer 直接写入账本，见 docs/interfaces.md「boh_storage::testing」）。
// 重建后该行可以查询，也可以在本地修改：新事件 source = LOCAL，aggregate_version 2。
#[tokio::test]
async fn hq_package_master_data_rebuilds_like_local_changes() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(NOW);
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let local = write(
        &router,
        "POST",
        "/api/v1/waste-reasons",
        json!({ "command_id": cmd(1), "code": "EXPIRED", "name": "过期", "active": true }),
    )
    .await;
    drop(router);

    let hq_id = "01890a5d-ac96-774b-bcce-b30209a9f001";
    let payload = include_str!("golden/MASTER_DATA_CHANGED@1/WASTE_REASON_HQ_PACKAGE.json");
    let mut writer = spec_support::writer(&db_path).unwrap();
    writer
        .execute(
            "INSERT INTO store_events (id, event_type, schema_version, aggregate_type, aggregate_id,
                 aggregate_version, command_id, actor_id, device_id, business_date, occurred_at,
                 recorded_at, payload)
             VALUES ('01890a5d-ac96-774b-bcce-b30209a9f002', 'MASTER_DATA_CHANGED', 1, 'WASTE_REASON',
                 ?1, 1, ?2, '00000000-0000-7000-8000-000000000000',
                 '00000000-0000-7000-8000-000000000001', '2026-10-06', ?3, ?3, ?4)",
            rusqlite::params![hq_id, cmd(1), NOW.0, payload.trim_end()],
        )
        .unwrap();
    assert_eq!(spec_support::rebuild_projections(&mut writer).unwrap(), 2);
    drop(writer);

    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let reply = spec_support::get(&router, "/api/v1/waste-reasons", Some(&STAFF))
        .await
        .unwrap();
    let hq_row = json!({
        "waste_reason_id": hq_id, "code": "HQ_RETURN", "name": "总部召回", "active": true,
        "revision": 1,
    });
    assert_eq!(
        assert_success(&reply),
        &json!({ "waste_reasons": [local["waste_reason"], hq_row] })
    );

    let data = write(
        &router,
        "PUT",
        &format!("/api/v1/waste-reasons/{hq_id}"),
        json!({ "command_id": cmd(2), "base_revision": 1, "name": "召回", "active": false }),
    )
    .await;
    assert_eq!(data["waste_reason"]["revision"], json!(2));
    let reader = spec_support::reader(&db_path).unwrap();
    let (version, payload): (i64, String) = reader
        .query_row(
            "SELECT aggregate_version, payload FROM store_events WHERE seq = 3",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(version, 2);
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap(),
        json!({ "entity": "WASTE_REASON", "source": "LOCAL", "snapshot": {
            "code": "HQ_RETURN", "name": "召回", "active": false,
        } })
    );
}

/// 收货请求的一行：`qty` 个 `unit_code`（每个 `factor` 个基本单位），到期日 `expires_on`。
fn receipt_line(item_id: &str, qty: i64, unit_code: &str, factor: i64, expires_on: &str) -> Value {
    json!({
        "item_id": item_id,
        "input": { "qty": qty, "unit_code": unit_code, "base_qty_per_unit": factor },
        "produced_on": "2026-10-01", "expires_on": expires_on, "line_cost_cents": 100,
    })
}

/// 收货：平板在 `SENT − lag` 录入、在 `sent_at` 发送。返回完整响应。
#[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试。
async fn receive(
    router: &axum::Router,
    command_id: &str,
    supplier_id: &str,
    lines: Vec<Value>,
    lag: i64,
    sent_at: i64,
) -> spec_support::JsonReply {
    let body = json!({
        "command_id": command_id, "supplier_id": supplier_id, "lines": lines,
        "captured_at": SENT - lag, "sent_at": sent_at,
    });
    let reply = spec_support::post(router, "/api/v1/receipts", Some(&STAFF), &body)
        .await
        .unwrap();
    assert_success(&reply);
    reply
}

// 收货的账本（同一物料两行、带生产商批号、单位换算、负 lag 的警告、前一营业日）：在线写入后篡改库存投影
// （删批次及其流水、改余量与来源行序、改流水数量），重建后全部投影与在线写入逐行一致，账本、processed_commands 和 store_meta 不变；
// 重建可重复执行。验收用例「批次号重建」：同次收货的 A、B 重建后仍为 RAW-FLOUR-20261006-001、…-002，source_line_no 仍为 0、1；
// occurred_at 为 10-06 03:00 的黄油批次属于前一营业日，批次日期仍是当地日历日期 10-06（RAW-BUTTER-20261006-002）。
// 之后黄油改为 SEMI：重建时黄油的当前分类与建批次时不同，两个 RAW-BUTTER 批次号照原样恢复，不按当前分类重新拼装。
// 重建不影响幂等回执：之后用原 command_id、原内容（sent_at 不同）重发，原样返回首次响应（含 warnings），不新增事件。
// 重建后的批次能继续支撑新的写命令：面粉新批次的流水号接着重建出的批次取 …-004；它早于 A 到期，返回 EXPIRES_BEFORE_OLDER_STOCK；
// 同次的黄油行取当前分类，流水号接着 RAW-BUTTER 批次取 SEMI-BUTTER-20261006-003，到期晚于已有黄油批次，不警告。
#[tokio::test]
async fn rebuild_restores_receipt_projections() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(NOW);
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let id = |data: Value, key: &str, field: &str| data[key][field].as_str().unwrap().to_owned();
    let flour = id(
        write(
            &router,
            "POST",
            "/api/v1/items",
            json!({
                "command_id": cmd(1), "code": "FLOUR", "name": "面粉", "base_unit": "g",
                "category": "RAW", "units": [{ "unit_code": "bag", "base_qty_per_unit": 25000 }],
                "active": true,
            }),
        )
        .await,
        "item",
        "item_id",
    );
    let butter = id(
        write(
            &router,
            "POST",
            "/api/v1/items",
            json!({
                "command_id": cmd(2), "code": "BUTTER", "name": "黄油", "base_unit": "g",
                "category": "RAW", "units": [], "active": true,
            }),
        )
        .await,
        "item",
        "item_id",
    );
    let supplier = id(
        write(
            &router,
            "POST",
            "/api/v1/suppliers",
            json!({ "command_id": cmd(3), "code": "S1", "name": "S1", "active": true }),
        )
        .await,
        "supplier",
        "supplier_id",
    );
    let first = receive(
        &router,
        &cmd(4),
        &supplier,
        vec![
            receipt_line(&flour, 5, "g", 1, "2026-10-20"),
            receipt_line(&flour, 10, "g", 1, "2026-10-25"),
        ],
        0,
        SENT,
    )
    .await;
    let a = first.body["data"]["receipt"]["lines"][0]["lot_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let b = first.body["data"]["receipt"]["lines"][1]["lot_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut with_lot_no = receipt_line(&butter, 2, "g", 1, "2026-10-15");
    with_lot_no["manufacturer_lot_no"] = json!("L-1");
    let lines = vec![
        with_lot_no,
        receipt_line(&flour, 1, "bag", 25000, "2026-10-22"),
    ];
    let second = receive(&router, &cmd(5), &supplier, lines.clone(), -60_000, SENT).await;
    assert_eq!(
        second.body["warnings"][0]["code"],
        json!("CAPTURE_TIME_ADJUSTED")
    );
    let butter_lot = second.body["data"]["receipt"]["lines"][0]["lot_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        [a.as_str(), b.as_str(), butter_lot.as_str()],
        [
            "RAW-FLOUR-20261006-001",
            "RAW-FLOUR-20261006-002",
            "RAW-BUTTER-20261006-001"
        ]
    );
    assert_eq!(
        second.body["data"]["receipt"]["lines"][1]["lot_id"],
        json!("RAW-FLOUR-20261006-003")
    );
    // lag 6 小时：occurred_at 为 03:00，归前一营业日；批次日期仍是 10-06。
    let third = receive(
        &router,
        &cmd(6),
        &supplier,
        vec![receipt_line(&butter, 3, "g", 1, "2026-10-20")],
        6 * 3_600_000,
        SENT,
    )
    .await;
    assert_eq!(
        third.body["data"]["receipt"]["business_date"],
        json!("2026-10-05")
    );
    assert_eq!(
        third.body["data"]["receipt"]["lines"][0]["lot_id"],
        json!("RAW-BUTTER-20261006-002")
    );
    write(
        &router,
        "PUT",
        &format!("/api/v1/items/{butter}"),
        json!({
            "command_id": cmd(8), "base_revision": 1, "name": "黄油", "category": "SEMI",
            "units": [], "active": true,
        }),
    )
    .await;

    let online = projections(&db_path).unwrap();
    for (table, rows) in [("inventory_lots", 5), ("inventory_movements", 5)] {
        assert!(
            online
                .iter()
                .any(|(name, content)| name == table && content.len() == rows),
            "{table}: {online:?}"
        );
    }
    let ledger_before = ledger(&db_path).unwrap();

    let mut writer = spec_support::writer(&db_path).unwrap();
    writer
        .execute_batch(&format!(
            "DELETE FROM inventory_movements WHERE lot_id = '{b}';
             DELETE FROM inventory_lots WHERE lot_id = '{b}';
             UPDATE inventory_lots SET remaining_qty = 1, source_line_no = 7 WHERE lot_id = '{a}';
             UPDATE inventory_movements SET nominal_qty = 9, qty_delta = 9 WHERE lot_id = '{butter_lot}';"
        ))
        .unwrap();
    assert_ne!(projections(&db_path).unwrap(), online);

    for _ in 0..2 {
        assert_eq!(spec_support::rebuild_projections(&mut writer).unwrap(), 7);
        assert_eq!(projections(&db_path).unwrap(), online);
        assert_eq!(ledger(&db_path).unwrap(), ledger_before);
    }
    let line_nos = spec_support::rows(
        &writer,
        &format!(
            "SELECT lot_id, source_line_no FROM inventory_lots
             WHERE lot_id IN ('{a}', '{b}') ORDER BY source_line_no"
        ),
    )
    .unwrap();
    assert_eq!(
        line_nos,
        [
            vec![SqlValue::Text(a.clone()), SqlValue::Integer(0)],
            vec![SqlValue::Text(b.clone()), SqlValue::Integer(1)],
        ]
    );
    drop(writer);

    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let retry = receive(
        &router,
        &cmd(5),
        &supplier,
        lines,
        -60_000,
        SENT + 3_600_000,
    )
    .await;
    assert_eq!(retry.body, second.body);
    assert_eq!(ledger(&db_path).unwrap(), ledger_before);
    let reply = receive(
        &router,
        &cmd(7),
        &supplier,
        vec![
            receipt_line(&flour, 1, "g", 1, "2026-10-18"),
            receipt_line(&butter, 1, "g", 1, "2026-10-25"),
        ],
        0,
        SENT,
    )
    .await;
    let lot = reply.body["data"]["receipt"]["lines"][0]["lot_id"].clone();
    assert_eq!(lot, json!("RAW-FLOUR-20261006-004"));
    assert_eq!(
        reply.body["data"]["receipt"]["lines"][1]["lot_id"],
        json!("SEMI-BUTTER-20261006-003")
    );
    assert_eq!(
        reply.body["warnings"],
        json!([{
            "code": "EXPIRES_BEFORE_OLDER_STOCK",
            "message": reply.body["warnings"][0]["message"],
            "details": { "line": 0, "item_id": flour, "lot_id": lot },
        }])
    );
}

// domain「收货接口」：GOODS_RECEIVED@1 不支持。账本中有 @1 收货（golden 样本 GOODS_RECEIVED@1，由测试经 open_writer
// 直接写入账本）时重建返回错误；全部投影保持重建前的状态，账本、processed_commands、store_meta 不变。
#[tokio::test]
async fn rebuild_refuses_goods_received_v1() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let router = spec_support::router(&db_path, ManualClock::new(NOW).clock()).unwrap();
    let flour = write(
        &router,
        "POST",
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
    let supplier = write(
        &router,
        "POST",
        "/api/v1/suppliers",
        json!({ "command_id": cmd(2), "code": "S1", "name": "S1", "active": true }),
    )
    .await["supplier"]["supplier_id"]
        .as_str()
        .unwrap()
        .to_owned();
    receive(
        &router,
        &cmd(3),
        &supplier,
        vec![receipt_line(&flour, 5, "g", 1, "2026-10-20")],
        0,
        SENT,
    )
    .await;
    drop(router);

    // @1 样本的 item_id、supplier_id 换成本库的实际 ID，其余原样写入。
    let payload = include_str!("golden/GOODS_RECEIVED@1/WITH_MANUFACTURER_LOT_NO.json")
        .trim_end()
        .replace("01890a5d-ac96-774b-bcce-b302099a8501", &flour)
        .replace("01890a5d-ac96-774b-bcce-b302099a8601", &supplier);
    let mut writer = spec_support::writer(&db_path).unwrap();
    let tx = writer.transaction().unwrap();
    tx.execute(
        "INSERT INTO processed_commands (command_id, command_type, request, response, recorded_at)
         VALUES (?1, 'receipt.create', '{}', '{}', ?2)",
        rusqlite::params![cmd(90), NOW.0],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO store_events (id, event_type, schema_version, aggregate_type, aggregate_id,
             aggregate_version, command_id, actor_id, device_id, business_date, occurred_at,
             recorded_at, payload)
         VALUES ('01890a5d-ac96-774b-bcce-b30209a9f003', 'GOODS_RECEIVED', 1, 'RECEIPT',
             '01890a5d-ac96-774b-bcce-b30209a9f004', 1, ?1, ?2, ?3, '2026-10-06', ?4, ?4, ?5)",
        rusqlite::params![cmd(90), STAFF.employee_id, STAFF.device_id, NOW.0, payload],
    )
    .unwrap();
    tx.commit().unwrap();
    let online = projections(&db_path).unwrap();
    let ledger_before = ledger(&db_path).unwrap();

    assert!(spec_support::rebuild_projections(&mut writer).is_err());

    assert_eq!(projections(&db_path).unwrap(), online);
    assert_eq!(ledger(&db_path).unwrap(), ledger_before);
}

/// 报损请求的一行：`qty` 克，原因 EXPIRED，不指定批次；`lot_id`、`confirm` 由调用方给出。
fn waste_line(item_id: &str, qty: i64, lot_id: Option<&str>, confirm: bool) -> Value {
    let mut line = json!({
        "item_id": item_id,
        "input": { "qty": qty, "unit_code": "g", "base_qty_per_unit": 1 },
        "reason_code": "EXPIRED",
    });
    if let Some(lot_id) = lot_id {
        line["lot_id"] = json!(lot_id);
    }
    if confirm {
        line["confirm_shortage"] = json!(true);
    }
    line
}

/// 报损：平板在 `SENT` 录入、在 `sent_at` 发送。返回完整响应。
#[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试。
async fn waste(
    router: &axum::Router,
    command_id: &str,
    lines: Vec<Value>,
    sent_at: i64,
) -> spec_support::JsonReply {
    let body = json!({
        "command_id": command_id, "lines": lines, "captured_at": SENT, "sent_at": sent_at,
    });
    let reply = spec_support::post(router, "/api/v1/waste-records", Some(&STAFF), &body)
        .await
        .unwrap();
    assert_success(&reply);
    reply
}

/// 物料（RAW，基本单位 g）与报损原因 EXPIRED；返回物料 ID。
#[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态的请求失败时，后续步骤没有意义，直接终止测试。
async fn item_and_reason(
    router: &axum::Router,
    item_cmd: u16,
    reason_cmd: u16,
    code: &str,
) -> String {
    let item = write(
        router,
        "POST",
        "/api/v1/items",
        json!({
            "command_id": cmd(item_cmd), "code": code, "name": code, "base_unit": "g",
            "category": "RAW", "units": [], "active": true,
        }),
    )
    .await["item"]["item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    write(
        router,
        "POST",
        "/api/v1/waste-reasons",
        json!({ "command_id": cmd(reason_cmd), "code": "EXPIRED", "name": "过期", "active": true }),
    )
    .await;
    item
}

#[allow(clippy::unwrap_used)] // 同上。
async fn supplier(router: &axum::Router, n: u16) -> String {
    write(
        router,
        "POST",
        "/api/v1/suppliers",
        json!({ "command_id": cmd(n), "code": "S1", "name": "S1", "active": true }),
    )
    .await["supplier"]["supplier_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// 全部批次的 (lot_id, remaining_qty)，按批次号排列。
fn lot_balances(db_path: &Path) -> Result<Vec<(String, i64)>, Box<dyn Error>> {
    let reader = spec_support::reader(db_path)?;
    let mut statement =
        reader.prepare("SELECT lot_id, remaining_qty FROM inventory_lots ORDER BY lot_id")?;
    let rows = statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

// 报损的账本（不指定批次的 FIFO、指定批次使批次变负、账外缺口新建与累加、两个物料、一个命令多行）：在线写入后篡改库存投影
// （删账外缺口、改批次余量、删流水），重建后全部投影与在线写入逐行一致，账本、processed_commands 和 store_meta 不变；
// 重建可重复执行。
// 重建不影响幂等回执：之后用原 command_id、原内容（含确认标记，sent_at 不同）重发，原样返回首次响应，不新增事件。
// 重建后的投影能继续支撑新的写命令：面粉 A −2、B 10，净账面 8，报 8 g 不需要确认，FIFO 跳过 A，分配 B 8。
#[tokio::test]
async fn rebuild_restores_waste_projections() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(NOW);
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let flour = item_and_reason(&router, 1, 4, "FLOUR").await;
    let sugar = write(
        &router,
        "POST",
        "/api/v1/items",
        json!({
            "command_id": cmd(2), "code": "SUGAR", "name": "糖", "base_unit": "g",
            "category": "RAW", "units": [], "active": true,
        }),
    )
    .await["item"]["item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let supplier = supplier(&router, 3).await;
    let receipt = receive(
        &router,
        &cmd(5),
        &supplier,
        vec![
            receipt_line(&flour, 5, "g", 1, "2026-10-20"),
            receipt_line(&flour, 10, "g", 1, "2026-10-20"),
            receipt_line(&sugar, 4, "g", 1, "2026-10-20"),
        ],
        0,
        SENT,
    )
    .await;
    let lot = |i: usize| {
        receipt.body["data"]["receipt"]["lines"][i]["lot_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (a, b, s) = (lot(0), lot(1), lot(2));
    waste(
        &router,
        &cmd(6),
        vec![waste_line(&flour, 3, None, false)],
        SENT,
    )
    .await;
    let lines = vec![
        waste_line(&flour, 4, Some(&a), true),
        waste_line(&sugar, 6, None, true),
    ];
    let second = waste(&router, &cmd(7), lines.clone(), SENT).await;
    waste(
        &router,
        &cmd(8),
        vec![waste_line(&sugar, 1, None, true)],
        SENT,
    )
    .await;
    assert_eq!(
        lot_balances(&db_path).unwrap(),
        [(a.clone(), -2), (b.clone(), 10), (s.clone(), 0)]
    );

    let online = projections(&db_path).unwrap();
    assert!(
        online
            .iter()
            .any(|(name, content)| name == "inventory_unallocated" && content.len() == 1),
        "{online:?}"
    );
    let ledger_before = ledger(&db_path).unwrap();

    let mut writer = spec_support::writer(&db_path).unwrap();
    writer
        .execute_batch(&format!(
            "DELETE FROM inventory_unallocated;
             UPDATE inventory_lots SET remaining_qty = 99 WHERE lot_id = '{b}';
             DELETE FROM inventory_movements WHERE event_seq = 7 AND movement_no = 1;"
        ))
        .unwrap();
    assert_ne!(projections(&db_path).unwrap(), online);

    for _ in 0..2 {
        assert_eq!(spec_support::rebuild_projections(&mut writer).unwrap(), 8);
        assert_eq!(projections(&db_path).unwrap(), online);
        assert_eq!(ledger(&db_path).unwrap(), ledger_before);
    }
    drop(writer);

    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let retry = waste(&router, &cmd(7), lines, SENT + 3_600_000).await;
    assert_eq!(retry.body, second.body);
    assert_eq!(ledger(&db_path).unwrap(), ledger_before);
    let reply = waste(
        &router,
        &cmd(9),
        vec![waste_line(&flour, 8, None, false)],
        SENT,
    )
    .await;
    let line = &reply.body["data"]["waste_record"]["lines"][0];
    assert_eq!(line["item_book_qty"], json!(8));
    assert_eq!(
        line["alloc"],
        json!([{ "lot_id": b, "qty": 8, "source": "FIFO" }])
    );
    assert_eq!(reply.body["warnings"], json!([]));
}

// 验收用例「批次号重建」：同次收货 A（…-001）5 g、B（…-002）10 g，尚未扣减时清空投影、按 seq 重建，投影与重建前逐行一致；
// 之后报损面粉 8 g（不指定批次）按重建出的批次号分配 A 5 FIFO + B 3 FIFO，余量 A 0、B 7。
#[tokio::test]
async fn rebuilt_lots_keep_their_fifo_order() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(NOW);
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let flour = item_and_reason(&router, 1, 2, "FLOUR").await;
    let supplier = supplier(&router, 3).await;
    receive(
        &router,
        &cmd(4),
        &supplier,
        vec![
            receipt_line(&flour, 5, "g", 1, "2026-10-20"),
            receipt_line(&flour, 10, "g", 1, "2026-10-20"),
        ],
        0,
        SENT,
    )
    .await;
    drop(router);
    let online = projections(&db_path).unwrap();

    let mut writer = spec_support::writer(&db_path).unwrap();
    writer
        .execute_batch("DELETE FROM inventory_movements; DELETE FROM inventory_lots;")
        .unwrap();
    assert_eq!(spec_support::rebuild_projections(&mut writer).unwrap(), 4);
    assert_eq!(projections(&db_path).unwrap(), online);
    drop(writer);

    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    let reply = waste(
        &router,
        &cmd(5),
        vec![waste_line(&flour, 8, None, false)],
        SENT,
    )
    .await;
    assert_eq!(
        reply.body["data"]["waste_record"]["lines"][0]["alloc"],
        json!([
            { "lot_id": "RAW-FLOUR-20261006-001", "qty": 5, "source": "FIFO" },
            { "lot_id": "RAW-FLOUR-20261006-002", "qty": 3, "source": "FIFO" },
        ])
    );
    assert_eq!(
        lot_balances(&db_path).unwrap(),
        [
            ("RAW-FLOUR-20261006-001".to_owned(), 0),
            ("RAW-FLOUR-20261006-002".to_owned(), 7),
        ]
    );
}

/// 报损 golden 样本中的物料 ID 与吸收它的事件 ID：写入账本前换成本库的实际 ID。
const GOLDEN_WASTE_ITEM_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8501";
const GOLDEN_ABSORBER_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8701";

/// 建一个账本：面粉、供应商、报损原因 EXPIRED，一次收货面粉 25000 g（…-001）、3000 g（…-002），共 4 个事件。
/// 返回 (物料 ID, 收货事件 ID)；收货事件作为被吸收行的 absorbed_by_event_id（吸收它的盘点事件本期还不能写入）。
#[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态失败时，后续步骤没有意义，直接终止测试。
async fn waste_ledger(db_path: &Path) -> (String, String) {
    let router = spec_support::router(db_path, ManualClock::new(NOW).clock()).unwrap();
    let flour = item_and_reason(&router, 1, 2, "FLOUR").await;
    let supplier = supplier(&router, 3).await;
    receive(
        &router,
        &cmd(4),
        &supplier,
        vec![
            receipt_line(&flour, 25000, "g", 1, "2026-10-20"),
            receipt_line(&flour, 3000, "g", 1, "2026-10-20"),
        ],
        0,
        SENT,
    )
    .await;
    drop(router);
    let reader = spec_support::reader(db_path).unwrap();
    let receipt_id: String = reader
        .query_row("SELECT id FROM store_events WHERE seq = 4", [], |r| {
            r.get(0)
        })
        .unwrap();
    (flour, receipt_id)
}

/// 经 open_writer 直接写入一条 WASTE_LOGGED@1（本期没有写入口的结构分支）及其 processed_commands 行，不写 seq 列。
#[allow(clippy::unwrap_used)] // 测试夹具：写入失败时，后续步骤没有意义，直接终止测试。
fn insert_waste(writer: &mut Connection, n: u8, payload: &str) {
    let command_id = cmd(0x90 + u16::from(n));
    let tx = writer.transaction().unwrap();
    tx.execute(
        "INSERT INTO processed_commands (command_id, command_type, request, response, recorded_at)
         VALUES (?1, 'waste.log', '{}', '{}', ?2)",
        rusqlite::params![command_id, NOW.0],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO store_events (id, event_type, schema_version, aggregate_type, aggregate_id,
             aggregate_version, command_id, actor_id, device_id, business_date, occurred_at,
             recorded_at, payload)
         VALUES (?1, 'WASTE_LOGGED', 1, 'WASTE_RECORD', ?2, 1, ?3, ?4, ?5, '2026-10-06', ?6, ?6, ?7)",
        rusqlite::params![
            format!("01890a5d-ac96-774b-bcce-b30209a9f1{n:02x}"),
            format!("01890a5d-ac96-774b-bcce-b30209a9f2{n:02x}"),
            command_id,
            STAFF.employee_id,
            STAFF.device_id,
            NOW.0,
            payload
        ],
    )
    .unwrap();
    tx.commit().unwrap();
}

/// 报损流水：(event_seq, movement_no, item_id, lot_id, alloc_source, nominal_qty, qty_delta, absorbed_by_event_id,
/// physical_at, business_date)。
type WasteMovement = (
    i64,
    i64,
    String,
    Option<String>,
    String,
    i64,
    i64,
    Option<String>,
    i64,
    String,
);

fn waste_movements(db_path: &Path) -> Result<Vec<WasteMovement>, Box<dyn Error>> {
    let reader = spec_support::reader(db_path)?;
    let mut statement = reader.prepare(
        "SELECT event_seq, movement_no, item_id, lot_id, alloc_source, nominal_qty, qty_delta,
                absorbed_by_event_id, physical_at, business_date
         FROM inventory_movements WHERE kind = 'WASTE' ORDER BY event_seq, movement_no",
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
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

// domain「报损接口」投影与「盘点吸收」：被吸收的报损行（golden 样本 ABSORBED、ABSORBED_SPECIFIED_LOT；吸收判定随盘点切片实现，
// 本期没有写入口，经 open_writer 直接写入账本）照常重建：每个被吸收的行一条流水，kind WASTE、alloc_source ABSORBED、
// lot_id 为 NULL（指定了批次的行也一样）、nominal_qty = −qty、qty_delta = 0、absorbed_by_event_id 取自 payload，
// 不查其他事件；不改动批次余量，不产生账外缺口。
// 同一事件中被吸收的行和正常分配的行可以并存：流水按行序、alloc 顺序从 0 连续编号。重建可重复执行。
#[tokio::test]
async fn absorbed_waste_lines_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let (flour, absorber) = waste_ledger(&db_path).await;
    let golden = |text: &str| {
        text.trim_end()
            .replace(GOLDEN_WASTE_ITEM_ID, &flour)
            .replace(GOLDEN_ABSORBER_ID, &absorber)
    };
    let mixed = format!(
        r#"{{"lines":[{{"item_id":"{flour}","qty":1000,"input":{{"qty":1000,"unit_code":"g","base_qty_per_unit":1}},"reason_code":"EXPIRED","item_book_qty":28000,"absorbed_by_event_id":"{absorber}"}},{{"item_id":"{flour}","qty":500,"input":{{"qty":500,"unit_code":"g","base_qty_per_unit":1}},"reason_code":"EXPIRED","item_book_qty":28000,"alloc":[{{"lot_id":"RAW-FLOUR-20261006-001","qty":500,"source":"FIFO"}}]}}]}}"#
    );
    let mut writer = spec_support::writer(&db_path).unwrap();
    insert_waste(
        &mut writer,
        1,
        &golden(include_str!("golden/WASTE_LOGGED@1/ABSORBED.json")),
    );
    insert_waste(
        &mut writer,
        2,
        &golden(include_str!(
            "golden/WASTE_LOGGED@1/ABSORBED_SPECIFIED_LOT.json"
        )),
    );
    insert_waste(&mut writer, 3, &mixed);
    let ledger_before = ledger(&db_path).unwrap();

    for _ in 0..2 {
        assert_eq!(spec_support::rebuild_projections(&mut writer).unwrap(), 7);
        let absorbed = |seq: i64, no: i64, qty: i64| -> WasteMovement {
            (
                seq,
                no,
                flour.clone(),
                None,
                "ABSORBED".into(),
                -qty,
                0,
                Some(absorber.clone()),
                NOW.0,
                "2026-10-06".into(),
            )
        };
        assert_eq!(
            waste_movements(&db_path).unwrap(),
            [
                absorbed(5, 0, 2000),
                absorbed(6, 0, 4000),
                absorbed(7, 0, 1000),
                (
                    7,
                    1,
                    flour.clone(),
                    Some("RAW-FLOUR-20261006-001".into()),
                    "FIFO".into(),
                    -500,
                    -500,
                    None,
                    NOW.0,
                    "2026-10-06".into(),
                ),
            ]
        );
        assert_eq!(
            lot_balances(&db_path).unwrap(),
            [
                ("RAW-FLOUR-20261006-001".to_owned(), 24500),
                ("RAW-FLOUR-20261006-002".to_owned(), 3000),
            ]
        );
        let unallocated = spec_support::count(&writer, "inventory_unallocated").unwrap();
        assert_eq!(unallocated, 0);
        assert_eq!(ledger(&db_path).unwrap(), ledger_before);
    }
}

// docs/domain.md「事件目录」WASTE_LOGGED 的行结构：alloc 与 absorbed_by_event_id 恰有一个出现；lot_book_qty 与 lot_id
// 同时出现。每组用两个账本对照：只差这一处结构的合法报损照常重建（说明事件类型已被识别、引用都在），违反结构的报损
// （由测试经 open_writer 直接写入）重建返回错误，全部投影保持重建前的状态，账本不变。
#[tokio::test]
async fn rebuild_refuses_malformed_waste_lines() {
    let fifo = r#","alloc":[{"lot_id":"RAW-FLOUR-20261006-001","qty":1,"source":"FIFO"}]"#;
    let specified =
        r#","alloc":[{"lot_id":"RAW-FLOUR-20261006-001","qty":1,"source":"SPECIFIED"}]"#;
    let absorbed = r#","absorbed_by_event_id":"ABSORBER""#;
    let lot_id = r#","lot_id":"RAW-FLOUR-20261006-001""#;
    let lot_book = r#","lot_book_qty":25000"#;
    // (名称, 违反结构的 (lot 字段, lot_book_qty 字段, 结尾), 合法对照的 (lot 字段, lot_book_qty 字段, 结尾))
    let both = format!("{fifo}{absorbed}");
    let cases = [
        (
            "both alloc and absorbed",
            ("", "", both.as_str()),
            ("", "", fifo),
        ),
        (
            "neither alloc nor absorbed",
            ("", "", ""),
            ("", "", absorbed),
        ),
        (
            "lot_book_qty without lot_id",
            ("", lot_book, fifo),
            ("", "", fifo),
        ),
        (
            "lot_id without lot_book_qty",
            (lot_id, "", specified),
            (lot_id, lot_book, specified),
        ),
    ];
    for (name, malformed, valid) in cases {
        for ((lot, book, tail), ok) in [(valid, true), (malformed, false)] {
            let dir = tempfile::tempdir().unwrap();
            let db_path = dir.path().join("boh.db");
            let (flour, absorber) = waste_ledger(&db_path).await;
            let payload = format!(
                r#"{{"lines":[{{"item_id":"{flour}"{lot},"qty":1,"input":{{"qty":1,"unit_code":"g","base_qty_per_unit":1}},"reason_code":"EXPIRED","item_book_qty":28000{book}{tail}}}]}}"#
            )
            .replace("ABSORBER", &absorber);
            let mut writer = spec_support::writer(&db_path).unwrap();
            insert_waste(&mut writer, 1, &payload);
            let online = projections(&db_path).unwrap();
            let ledger_before = ledger(&db_path).unwrap();

            let result = spec_support::rebuild_projections(&mut writer);

            if ok {
                assert_eq!(result.unwrap(), 5, "{name} (valid): {payload}");
            } else {
                assert!(result.is_err(), "{name}: {payload}");
                assert_eq!(projections(&db_path).unwrap(), online, "{name}");
            }
            assert_eq!(ledger(&db_path).unwrap(), ledger_before, "{name}");
        }
    }
}
