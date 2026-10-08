//! 锁定测试：`boh-server init`。规则见 docs/domain.md「主数据」初始化，接口见 docs/interfaces.md「boh-server：init 子命令」。
//! 以子进程运行编译好的二进制，只通过退出码、对数据库的只读 SQL 和 `boh_app::test_router` 观察结果。

use std::convert::Infallible;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, Response, StatusCode, header};
use boh_domain::UnixMillis;
use boh_domain::time::{business_date, parse_business_day_cutoff, parse_timezone};
use boh_storage::clock::{Clock, ManualClock};
use boh_storage::rusqlite::types::Value as SqlValue;
use boh_storage::rusqlite::{self, Connection};
use serde_json::{Value, json};

const STORE_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8050";
const SYSTEM_ACTOR_ID: &str = "00000000-0000-7000-8000-000000000000";
const SYSTEM_DEVICE_ID: &str = "00000000-0000-7000-8000-000000000001";
/// domain.md「主数据」初始化的预置报损原因，按写入顺序。
const PRESET_WASTE_REASONS: [(&str, &str); 5] = [
    ("EXPIRED", "过期"),
    ("DAMAGED", "损坏"),
    ("PRODUCTION_DEFECT", "生产不良"),
    ("TASTING", "试吃"),
    ("OTHER", "其他"),
];

type Rows<T> = Result<T, Box<dyn Error>>;

struct Paths {
    _dir: tempfile::TempDir,
    config: PathBuf,
    db: PathBuf,
    backups: PathBuf,
}

#[allow(clippy::unwrap_used)] // 测试夹具：临时目录或配置文件写入失败时测试无法开始，直接终止。
fn paths() -> Paths {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("boh.db");
    let backups = dir.path().join("backups");
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "store_id = \"{STORE_ID}\"\ndb_path = {db:?}\nlisten_addr = \"127.0.0.1:0\"\n\
             timezone = \"Asia/Shanghai\"\nbusiness_day_cutoff = \"04:00\"\nbackup_dir = {backups:?}\n",
            db = db.display().to_string(),
            backups = backups.display().to_string(),
        ),
    )
    .unwrap();
    Paths {
        _dir: dir,
        config,
        db,
        backups,
    }
}

/// 运行 `boh-server init <配置文件>`，返回是否以 0 退出。
#[allow(clippy::unwrap_used)] // 测试夹具：无法启动子进程时测试无法进行，直接终止。
fn init(config: &Path) -> bool {
    Command::new(env!("CARGO_BIN_EXE_boh-server"))
        .arg("init")
        .arg(config)
        .env("RUST_LOG", "off")
        .output()
        .unwrap()
        .status
        .success()
}

#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
fn reader(db: &Path) -> Rows<Connection> {
    Ok(boh_storage::testing::open_reader(db)?)
}

fn rows(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<Vec<SqlValue>>> {
    let mut statement = conn.prepare(sql)?;
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns).map(|i| row.get::<_, SqlValue>(i)).collect()
        })?
        .collect()
}

/// 初始化写入的全部内容：store_meta、processed_commands、store_events、waste_reasons 的整表。
fn everything(db: &Path) -> Rows<Vec<Vec<Vec<SqlValue>>>> {
    let conn = reader(db)?;
    Ok(vec![
        rows(&conn, "SELECT * FROM store_meta")?,
        rows(
            &conn,
            "SELECT * FROM processed_commands ORDER BY command_id",
        )?,
        rows(&conn, "SELECT * FROM store_events ORDER BY seq")?,
        rows(&conn, "SELECT * FROM waste_reasons ORDER BY code")?,
    ])
}

fn is_uuid_v7(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, &b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
        && bytes[14] == b'7'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

// domain「主数据」初始化：一个写事务写入 store_meta、预置报损原因（按规定顺序，各一条 aggregate_version 1 的
// MASTER_DATA_CHANGED，系统 actor_id / device_id，source = LOCAL，全部启用）和一行 store.init 命令记录。
// 所有事件共用同一个 command_id 和同一个 now()：store_meta.created_at、processed_commands.recorded_at 与事件的
// recorded_at、occurred_at 相等；营业日由 occurred_at 按配置的时区和日切计算。投影 waste_reasons 与快照一致。
// init 不创建备份目录。
#[test]
fn init_writes_store_meta_and_preset_waste_reasons() {
    let paths = paths();
    let clock = Clock::system();
    let before = clock.now();

    assert!(init(&paths.config));

    let after = clock.now();
    let conn = reader(&paths.db).unwrap();
    let (store_id, created_at): (String, i64) = conn
        .query_row("SELECT store_id, created_at FROM store_meta", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(store_id, STORE_ID);
    assert!(
        (before.0..=after.0).contains(&created_at),
        "{created_at} not in {before:?}..={after:?}"
    );

    let commands: Vec<(String, String, String, String, i64)> = conn
        .prepare(
            "SELECT command_id, command_type, request, response, recorded_at FROM processed_commands",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(commands.len(), 1, "{commands:?}");
    let (command_id, command_type, request, response, recorded_at) = &commands[0];
    assert!(is_uuid_v7(command_id), "{command_id}");
    assert_eq!(command_type, "store.init");
    assert_eq!(
        serde_json::from_str::<Value>(request).unwrap(),
        json!({ "store_id": STORE_ID })
    );
    assert_eq!(serde_json::from_str::<Value>(response).unwrap(), json!({}));
    assert_eq!(*recorded_at, created_at);

    let expected_date = business_date(
        UnixMillis(created_at),
        &parse_timezone("Asia/Shanghai").unwrap(),
        parse_business_day_cutoff("04:00").unwrap(),
    )
    .unwrap()
    .to_string();
    type EventRow = (
        i64,
        String,
        String,
        i64,
        String,
        String,
        i64,
        String,
        String,
        String,
        i64,
        i64,
        String,
    );
    let events: Vec<EventRow> = conn
        .prepare(
            "SELECT seq, id, event_type, schema_version, aggregate_type, aggregate_id,
                    aggregate_version, command_id, actor_id, device_id, occurred_at, recorded_at,
                    business_date || '|' || payload
             FROM store_events ORDER BY seq",
        )
        .unwrap()
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
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(events.len(), PRESET_WASTE_REASONS.len());
    let mut reason_ids = Vec::new();
    for (event, (seq, (code, name))) in events.iter().zip((1..).zip(PRESET_WASTE_REASONS)) {
        assert!(is_uuid_v7(&event.1), "{}", event.1);
        assert!(is_uuid_v7(&event.5), "{}", event.5);
        let (date, payload) = event.12.split_once('|').unwrap();
        assert_eq!(
            (
                event.0,
                event.2.as_str(),
                event.3,
                event.4.as_str(),
                event.6,
                event.7.as_str(),
                event.8.as_str(),
                event.9.as_str(),
                event.10,
                event.11,
                date,
            ),
            (
                seq,
                "MASTER_DATA_CHANGED",
                1,
                "WASTE_REASON",
                1,
                command_id.as_str(),
                SYSTEM_ACTOR_ID,
                SYSTEM_DEVICE_ID,
                created_at,
                created_at,
                expected_date.as_str(),
            )
        );
        assert_eq!(
            payload,
            format!(
                r#"{{"entity":"WASTE_REASON","source":"LOCAL","snapshot":{{"code":"{code}","name":"{name}","active":true}}}}"#
            )
        );
        reason_ids.push((code, event.5.clone()));
    }

    let mut projected: Vec<(String, String, String, i64, i64)> = conn
        .prepare("SELECT id, code, name, active, revision FROM waste_reasons ORDER BY rowid")
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    projected.sort_by(|a, b| a.1.cmp(&b.1));
    let mut expected: Vec<(String, String, String, i64, i64)> = reason_ids
        .iter()
        .zip(PRESET_WASTE_REASONS)
        .map(|((code, id), (_, name))| (id.clone(), (*code).to_owned(), name.to_owned(), 1, 1))
        .collect();
    expected.sort_by(|a, b| a.1.cmp(&b.1));
    assert_eq!(projected, expected);
    assert!(!paths.backups.exists());
}

// domain「主数据」初始化：store_meta 已存在时 init 拒绝执行，以非 0 退出码结束，不改动任何数据。
#[test]
fn init_refuses_an_initialized_database() {
    let paths = paths();
    assert!(init(&paths.config));
    let before = everything(&paths.db).unwrap();

    assert!(!init(&paths.config));

    assert_eq!(everything(&paths.db).unwrap(), before);
}

// domain「主数据」初始化：「任一步失败则全部回滚，可以重新执行」。waste_reasons 中已有 code = 'OTHER' 的行，
// 最后一条预置原因写不进去：init 以非 0 退出，store_meta、processed_commands、store_events 仍为空，已有的行不变。
// 删掉这一行后 init 成功。
#[test]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 建库并放入冲突的投影行。
fn init_rolls_back_everything_when_a_step_fails() {
    let paths = paths();
    let mut conn = boh_storage::testing::open_writer(&paths.db).unwrap();
    boh_storage::testing::migrate(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO waste_reasons (id, code, name, active, revision)
         VALUES ('01890a5d-ac96-774b-bcce-b30209b20001', 'OTHER', '其他', 1, 1)",
        [],
    )
    .unwrap();
    drop(conn);
    let planted = everything(&paths.db).unwrap();
    assert!(planted[0..3].iter().all(Vec::is_empty));
    assert_eq!(planted[3].len(), 1);

    assert!(!init(&paths.config));

    assert_eq!(everything(&paths.db).unwrap(), planted);
    let conn = boh_storage::testing::open_writer(&paths.db).unwrap();
    conn.execute("DELETE FROM waste_reasons", []).unwrap();
    drop(conn);
    assert!(init(&paths.config));
    let reasons = &everything(&paths.db).unwrap()[3];
    assert_eq!(reasons.len(), PRESET_WASTE_REASONS.len());
}

#[allow(clippy::disallowed_methods)] // 锁定测试经 test_router 访问已初始化的数据库。
fn router(db: &Path, clock: Clock) -> Rows<Router> {
    Ok(boh_app::test_router(db, clock)?)
}

/// 经开发桩身份发送 JSON 请求，返回状态码和信封。
async fn send(
    router: &Router,
    method: Method,
    uri: &str,
    role: &str,
    body: Option<&Value>,
) -> Rows<(StatusCode, Value)> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Dev-Employee-Id", "01890a5d-ac96-774b-bcce-b302099a8101")
        .header("X-Dev-Device-Id", "01890a5d-ac96-774b-bcce-b302099a8201")
        .header("X-Dev-Role", role);
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(body)?))?,
        None => builder.body(Body::empty())?,
    };
    let response = call(router.clone(), request).await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, serde_json::from_slice(&bytes)?))
}

async fn call<S>(mut service: S, request: Request<Body>) -> Response<Body>
where
    S: axum::ServiceExt<Request<Body>, Response = Response<Body>, Error = Infallible>,
{
    let Ok(()) = std::future::poll_fn(|cx| service.poll_ready(cx)).await;
    let Ok(response) = service.call(request).await;
    response
}

// domain「主数据」报损原因：「预置的报损原因同样可以修改和停用」。init 之后，接口按 code 字节序列出 5 条预置原因
// （revision 1、启用）；店长修改并停用其中一条，接着 init 的事件追加 seq 6、aggregate_version 2。
#[tokio::test]
async fn preset_waste_reasons_are_listed_and_editable_over_http() {
    let paths = paths();
    assert!(init(&paths.config));
    let conn = reader(&paths.db).unwrap();
    let mut ids: Vec<(String, String)> = conn
        .prepare("SELECT code, id FROM waste_reasons")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    ids.sort();
    let names: Vec<(&str, &str)> = {
        let mut names = PRESET_WASTE_REASONS.to_vec();
        names.sort();
        names
    };
    let rows = |expired: Value| -> Value {
        Value::Array(
            ids.iter()
                .zip(&names)
                .map(|((code, id), (_, name))| {
                    if code == "EXPIRED" {
                        expired.clone()
                    } else {
                        json!({ "waste_reason_id": id, "code": code, "name": name, "active": true, "revision": 1 })
                    }
                })
                .collect(),
        )
    };
    let expired_id = ids
        .iter()
        .find(|(code, _)| code == "EXPIRED")
        .unwrap()
        .1
        .clone();

    let created_at: i64 = conn
        .query_row("SELECT created_at FROM store_meta", [], |r| r.get(0))
        .unwrap();
    let clock = ManualClock::new(UnixMillis(created_at + 60_000));
    let router = router(&paths.db, clock.clock()).unwrap();

    let (status, body) = send(&router, Method::GET, "/api/v1/waste-reasons", "STAFF", None)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"],
        json!({ "waste_reasons": rows(json!({
            "waste_reason_id": expired_id, "code": "EXPIRED", "name": "过期", "active": true, "revision": 1,
        })) })
    );

    let updated = json!({
        "waste_reason_id": expired_id, "code": "EXPIRED", "name": "超过保质期", "active": false,
        "revision": 2,
    });
    let (status, body) = send(
        &router,
        Method::PUT,
        &format!("/api/v1/waste-reasons/{expired_id}"),
        "MANAGER",
        Some(&json!({
            "command_id": "01890a5d-ac96-774b-bcce-b30209b10001", "base_revision": 1,
            "name": "超过保质期", "active": false,
        })),
    )
    .await
    .unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], json!(true));
    assert_eq!(body["data"], json!({ "waste_reason": updated }));

    let last: (i64, String, i64) = conn
        .query_row(
            "SELECT seq, aggregate_id, aggregate_version FROM store_events ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(last, (6, expired_id.clone(), 2));

    let (status, body) = send(&router, Method::GET, "/api/v1/waste-reasons", "STAFF", None)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"], json!({ "waste_reasons": rows(updated) }));
}
