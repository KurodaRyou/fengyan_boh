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
