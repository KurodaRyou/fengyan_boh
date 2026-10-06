//! HTTP 锁定测试的辅助代码：只经 `boh_app::test_router` 返回的 `Router`、对已冻结表的只读 SQL
//! 和 `boh_storage::testing` 访问系统。接口见 docs/interfaces.md。
#![allow(dead_code)] // 每个测试文件只用到其中一部分辅助函数。

use std::convert::Infallible;
use std::error::Error;
use std::path::Path;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, Response, StatusCode, header};
use boh_storage::StorageError;
use boh_storage::clock::Clock;
use boh_storage::rusqlite::types::Value as SqlValue;
use boh_storage::rusqlite::{self, Connection};
use serde_json::{Map, Value};

/// `test_router` 的固定 `store_id`（docs/interfaces.md「Router 测试入口」）。
pub const TEST_STORE_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8050";
/// domain.md「主数据」保留的系统操作人与系统设备。
pub const SYSTEM_ACTOR_ID: &str = "00000000-0000-7000-8000-000000000000";
pub const SYSTEM_DEVICE_ID: &str = "00000000-0000-7000-8000-000000000001";

/// 开发桩身份：对应请求头 `X-Dev-Employee-Id`、`X-Dev-Device-Id`、`X-Dev-Role`。
#[derive(Debug, Clone, Copy)]
pub struct Actor {
    pub employee_id: &'static str,
    pub device_id: &'static str,
    pub role: &'static str,
}

pub const MANAGER: Actor = Actor {
    employee_id: "01890a5d-ac96-774b-bcce-b302099a8101",
    device_id: "01890a5d-ac96-774b-bcce-b302099a8201",
    role: "MANAGER",
};

pub const STAFF: Actor = Actor {
    employee_id: "01890a5d-ac96-774b-bcce-b302099a8102",
    device_id: "01890a5d-ac96-774b-bcce-b302099a8202",
    role: "STAFF",
};

#[allow(clippy::disallowed_methods)] // 锁定测试经 test_router 构造 Router。
pub fn router(db_path: &Path, clock: Clock) -> Result<Router, StorageError> {
    boh_app::test_router(db_path, clock)
}

#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
pub fn reader(db_path: &Path) -> Result<Connection, StorageError> {
    boh_storage::testing::open_reader(db_path)
}

#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
pub fn writer(db_path: &Path) -> Result<Connection, StorageError> {
    boh_storage::testing::open_writer(db_path)
}

#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 迁移原始连接。
pub fn migrate(conn: &mut Connection) -> Result<(), StorageError> {
    boh_storage::testing::migrate(conn)
}

#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 触发重建。
pub fn rebuild_projections(conn: &mut Connection) -> Result<u64, StorageError> {
    boh_storage::testing::rebuild_projections(conn)
}

pub struct JsonReply {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: Value,
}

/// 构造请求：带 `actor` 时附开发桩请求头；带 `body` 时附 `Content-Type: application/json`。
pub fn request(
    method: Method,
    uri: &str,
    actor: Option<&Actor>,
    body: Option<&Value>,
) -> Result<Request<Body>, Box<dyn Error>> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(actor) = actor {
        builder = builder
            .header("X-Dev-Employee-Id", actor.employee_id)
            .header("X-Dev-Device-Id", actor.device_id)
            .header("X-Dev-Role", actor.role);
    }
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(body)?))?,
        None => builder.body(Body::empty())?,
    };
    Ok(request)
}

/// 构造原样发送请求体文本的请求，用于非法 JSON、缺少或错误的 `Content-Type`、同义的不同 JSON 写法。
pub fn raw_request(
    method: Method,
    uri: &str,
    actor: Option<&Actor>,
    content_type: Option<&str>,
    body: &str,
) -> Result<Request<Body>, Box<dyn Error>> {
    let mut request = request(method, uri, actor, None)?;
    if let Some(content_type) = content_type {
        request
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type.parse()?);
    }
    *request.body_mut() = Body::from(body.to_owned());
    Ok(request)
}

/// 发送请求，响应体按 JSON 解析。
pub async fn send(router: &Router, request: Request<Body>) -> Result<JsonReply, Box<dyn Error>> {
    let response = call(router.clone(), request).await;
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|value| value.to_str())
        .transpose()?
        .map(str::to_owned);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body = serde_json::from_slice(&bytes)?;
    Ok(JsonReply {
        status,
        content_type,
        body,
    })
}

pub async fn get(
    router: &Router,
    uri: &str,
    actor: Option<&Actor>,
) -> Result<JsonReply, Box<dyn Error>> {
    send(router, request(Method::GET, uri, actor, None)?).await
}

pub async fn post(
    router: &Router,
    uri: &str,
    actor: Option<&Actor>,
    body: &Value,
) -> Result<JsonReply, Box<dyn Error>> {
    send(router, request(Method::POST, uri, actor, Some(body))?).await
}

pub async fn put(
    router: &Router,
    uri: &str,
    actor: Option<&Actor>,
    body: &Value,
) -> Result<JsonReply, Box<dyn Error>> {
    send(router, request(Method::PUT, uri, actor, Some(body))?).await
}

/// axum 不重新导出 tower 的 `Service`；经它的父 trait `axum::ServiceExt` 调用，测试不新增依赖。
async fn call<S>(mut service: S, request: Request<Body>) -> Response<Body>
where
    S: axum::ServiceExt<Request<Body>, Response = Response<Body>, Error = Infallible>,
{
    let Ok(()) = std::future::poll_fn(|cx| service.poll_ready(cx)).await;
    let Ok(response) = service.call(request).await;
    response
}

fn envelope(reply: &JsonReply) -> &Map<String, Value> {
    assert_eq!(reply.content_type.as_deref(), Some("application/json"));
    let envelope = reply
        .body
        .as_object()
        .unwrap_or_else(|| panic!("envelope must be a JSON object: {}", reply.body));
    let mut keys: Vec<&str> = envelope.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["data", "error", "success", "warnings"],
        "{}",
        reply.body
    );
    envelope
}

/// AGENTS.md「HTTP 约定」的成功信封：返回 `data`。`warnings` 由调用方检查。
pub fn assert_success(reply: &JsonReply) -> &Value {
    let envelope = envelope(reply);
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(envelope["success"], Value::Bool(true));
    assert!(envelope["warnings"].is_array(), "{}", reply.body);
    assert_eq!(envelope["error"], Value::Null);
    &envelope["data"]
}

/// AGENTS.md「HTTP 约定」的错误信封：返回 `details`。
pub fn assert_error<'a>(reply: &'a JsonReply, status: u16, code: &str) -> &'a Value {
    let envelope = envelope(reply);
    assert_eq!(reply.status.as_u16(), status, "{}", reply.body);
    assert_eq!(envelope["success"], Value::Bool(false));
    assert_eq!(envelope["data"], Value::Null);
    assert_eq!(envelope["warnings"], Value::Array(Vec::new()));
    let error = envelope["error"]
        .as_object()
        .unwrap_or_else(|| panic!("error must be an object: {}", reply.body));
    let mut keys: Vec<&str> = error.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["code", "details", "message"], "{}", reply.body);
    assert_eq!(
        error["code"],
        Value::String(code.to_owned()),
        "{}",
        reply.body
    );
    assert!(error["message"].is_string(), "{}", reply.body);
    assert!(error["details"].is_object(), "{}", reply.body);
    &error["details"]
}

/// 小写、带连字符、版本位为 7 的 UUID 文本。
pub fn is_uuid_v7(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, &b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
        && bytes[14] == b'7'
}

/// 执行查询，按列取出全部行。
pub fn rows(conn: &Connection, sql: &str) -> rusqlite::Result<Vec<Vec<SqlValue>>> {
    let mut statement = conn.prepare(sql)?;
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns).map(|i| row.get::<_, SqlValue>(i)).collect()
        })?
        .collect()
}

pub fn count(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
}
