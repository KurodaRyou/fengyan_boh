//! 锁定测试：`Actor` 开发桩与权限。规则见 docs/domain.md「员工认证」开发桩、「主数据」，
//! AGENTS.md「HTTP 约定」（401 / 403、写命令处理顺序）。

mod spec_support;

use axum::http::Method;
use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use serde_json::{Value, json};
use spec_support::{
    Actor, MANAGER, STAFF, SYSTEM_ACTOR_ID, SYSTEM_DEVICE_ID, assert_error, assert_success,
    raw_request, request, send,
};

const NOW: UnixMillis = UnixMillis(1_791_248_400_000); // 2026-10-06 09:00 +08:00
/// (缺少的请求头，"all" 表示全部缺少; 被替换的请求头和新值)。
type HeaderCase = (Option<&'static str>, Option<(&'static str, &'static str)>);

const EQUIPMENT_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8301";

fn create_body(command_id: &str) -> Value {
    json!({
        "command_id": command_id,
        "code": "F1", "name": "Walk-in", "equipment_type": "FRIDGE", "active": true,
    })
}

fn update_body(command_id: &str) -> Value {
    json!({
        "command_id": command_id,
        "base_revision": 1, "name": "Walk-in", "equipment_type": "FRIDGE", "active": true,
    })
}

/// 业务接口的每一种调用：(方法, 路径, 请求体)。
fn business_calls() -> Vec<(Method, String, Option<Value>)> {
    vec![
        (Method::GET, "/api/v1/equipment".into(), None),
        (
            Method::POST,
            "/api/v1/equipment".into(),
            Some(create_body("01890a5d-ac96-774b-bcce-b302099a8401")),
        ),
        (
            Method::PUT,
            format!("/api/v1/equipment/{EQUIPMENT_ID}"),
            Some(update_body("01890a5d-ac96-774b-bcce-b302099a8402")),
        ),
    ]
}

// 开发桩：缺少任一请求头、取值非法或使用保留系统 ID，一律 401 UNAUTHENTICATED，不入账。
#[tokio::test]
async fn missing_or_invalid_identity_is_unauthenticated() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let router = spec_support::router(&db_path, ManualClock::new(NOW).clock()).unwrap();

    let headers = [
        ("X-Dev-Employee-Id", MANAGER.employee_id),
        ("X-Dev-Device-Id", MANAGER.device_id),
        ("X-Dev-Role", MANAGER.role),
    ];
    let mut cases: Vec<HeaderCase> = vec![(Some("all"), None)];
    for (name, _) in headers {
        cases.push((Some(name), None));
    }
    for replacement in [
        ("X-Dev-Employee-Id", "not-a-uuid"),
        ("X-Dev-Employee-Id", "01890a5d-ac96-474b-bcce-b302099a8101"), // v4
        ("X-Dev-Employee-Id", SYSTEM_ACTOR_ID),
        ("X-Dev-Employee-Id", SYSTEM_DEVICE_ID),
        ("X-Dev-Device-Id", "not-a-uuid"),
        ("X-Dev-Device-Id", "01890a5d-ac96-474b-bcce-b302099a8201"), // v4
        ("X-Dev-Device-Id", SYSTEM_DEVICE_ID),
        ("X-Dev-Device-Id", SYSTEM_ACTOR_ID),
        ("X-Dev-Role", "ADMIN"),
        ("X-Dev-Role", "manager"),
        ("X-Dev-Role", ""),
    ] {
        cases.push((None, Some(replacement)));
    }

    for (method, uri, body) in business_calls() {
        for (missing, replacement) in &cases {
            let mut req = request(method.clone(), &uri, None, body.as_ref()).unwrap();
            for (name, value) in headers {
                if *missing == Some("all") || *missing == Some(name) {
                    continue;
                }
                let value = match replacement {
                    Some((replaced, new_value)) if *replaced == name => *new_value,
                    _ => value,
                };
                req.headers_mut().insert(name, value.parse().unwrap());
            }
            let reply = send(&router, req).await.unwrap();
            assert_error(&reply, 401, "UNAUTHENTICATED");
        }
    }

    let reader = spec_support::reader(&db_path).unwrap();
    assert_eq!(spec_support::count(&reader, "store_events").unwrap(), 0);
    assert_eq!(
        spec_support::count(&reader, "processed_commands").unwrap(),
        0
    );
}

// domain「主数据」：主数据写接口只允许 MANAGER，STAFF 为 403 FORBIDDEN；查询任何已认证员工可用。
#[tokio::test]
async fn staff_cannot_write_master_data_but_can_read_it() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let router = spec_support::router(&db_path, ManualClock::new(NOW).clock()).unwrap();

    for (method, uri, body) in business_calls() {
        let reply = send(
            &router,
            request(method.clone(), &uri, Some(&STAFF), body.as_ref()).unwrap(),
        )
        .await
        .unwrap();
        if method == Method::GET {
            assert_eq!(assert_success(&reply), &json!({ "equipment": [] }));
        } else {
            assert_error(&reply, 403, "FORBIDDEN");
        }
    }

    let reader = spec_support::reader(&db_path).unwrap();
    assert_eq!(spec_support::count(&reader, "store_events").unwrap(), 0);
    assert_eq!(
        spec_support::count(&reader, "processed_commands").unwrap(),
        0
    );
}

// AGENTS「HTTP 约定」处理顺序：身份与权限先于请求结构校验（合法 JSON、字段非法；含无法解析的路径参数）。
#[tokio::test]
async fn identity_and_role_are_checked_before_the_request_body() {
    let dir = tempfile::tempdir().unwrap();
    let router =
        spec_support::router(&dir.path().join("boh.db"), ManualClock::new(NOW).clock()).unwrap();
    let invalid = json!({ "command_id": "not-a-uuid", "code": "" });

    let cases: [(Option<&Actor>, u16, &str); 2] = [
        (None, 401, "UNAUTHENTICATED"),
        (Some(&STAFF), 403, "FORBIDDEN"),
    ];
    for (actor, status, code) in cases {
        for (method, uri) in [
            (Method::POST, "/api/v1/equipment".to_owned()),
            (Method::PUT, format!("/api/v1/equipment/{EQUIPMENT_ID}")),
            (Method::PUT, "/api/v1/equipment/not-a-uuid".to_owned()),
        ] {
            let reply = send(
                &router,
                request(method, &uri, actor, Some(&invalid)).unwrap(),
            )
            .await
            .unwrap();
            assert_error(&reply, status, code);
        }
    }
}

// AGENTS「HTTP 约定」处理顺序：请求体根本无法解析（非法 JSON、空体、缺少或错误的 Content-Type）时，
// 仍先检查身份与权限：无身份 401，STAFF 403；不入账。
#[tokio::test]
async fn identity_and_role_are_checked_before_parsing_the_body() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let router = spec_support::router(&db_path, ManualClock::new(NOW).clock()).unwrap();
    let create = create_body("01890a5d-ac96-774b-bcce-b302099a8401").to_string();
    let update = update_body("01890a5d-ac96-774b-bcce-b302099a8402").to_string();
    let json = Some("application/json");

    let cases: [(Option<&Actor>, u16, &str); 2] = [
        (None, 401, "UNAUTHENTICATED"),
        (Some(&STAFF), 403, "FORBIDDEN"),
    ];
    for (actor, status, code) in cases {
        for (method, uri, valid) in [
            (
                Method::POST,
                "/api/v1/equipment".to_owned(),
                create.as_str(),
            ),
            (
                Method::PUT,
                format!("/api/v1/equipment/{EQUIPMENT_ID}"),
                update.as_str(),
            ),
        ] {
            for (body, content_type) in [
                ("{", json),
                ("", json),
                (valid, None),
                (valid, Some("text/plain")),
            ] {
                let req = raw_request(method.clone(), &uri, actor, content_type, body).unwrap();
                let reply = send(&router, req).await.unwrap();
                assert_error(&reply, status, code);
            }
        }
    }

    let reader = spec_support::reader(&db_path).unwrap();
    assert_eq!(spec_support::count(&reader, "store_events").unwrap(), 0);
    assert_eq!(
        spec_support::count(&reader, "processed_commands").unwrap(),
        0
    );
}

// /health 不属于业务接口，不需要身份。
#[tokio::test]
async fn health_does_not_require_identity() {
    let dir = tempfile::tempdir().unwrap();
    let router =
        spec_support::router(&dir.path().join("boh.db"), ManualClock::new(NOW).clock()).unwrap();

    let reply = spec_support::get(&router, "/health", None).await.unwrap();

    assert_success(&reply);
}
