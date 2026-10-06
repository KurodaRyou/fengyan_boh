//! 锁定测试：HTTP 响应信封与通用错误码。规则见 AGENTS.md「HTTP 约定」，接口见 docs/interfaces.md。

mod spec_support;

use axum::http::Method;
use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use serde_json::json;
use spec_support::{MANAGER, assert_error, assert_success, raw_request, request, send};

const NOW: UnixMillis = UnixMillis(1_791_248_400_000); // 2026-10-06 09:00 +08:00

// 「所有响应都是同一信封」：成功时 data 是对象，warnings 总是存在且没有警告时为空数组，error 为 null。
#[tokio::test]
async fn health_uses_the_success_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(NOW);
    let router = spec_support::router(&dir.path().join("boh.db"), clock.clock()).unwrap();

    let reply = spec_support::get(&router, "/health", None).await.unwrap();

    let data = assert_success(&reply);
    assert!(data.is_object(), "data = {data}");
    assert_eq!(reply.body["warnings"], json!([]));
}

// 「其他通用错误码」：未匹配的路由（业务前缀内外）为 404 ROUTE_NOT_FOUND，信封完整、details 为 {}。
#[tokio::test]
async fn unknown_route_returns_route_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let router =
        spec_support::router(&dir.path().join("boh.db"), ManualClock::new(NOW).clock()).unwrap();

    for uri in [
        "/api/v1/no-such-thing",
        "/no-such-thing",
        "/api/v2/equipment",
    ] {
        for actor in [None, Some(&MANAGER)] {
            let reply = spec_support::get(&router, uri, actor).await.unwrap();
            assert_eq!(
                assert_error(&reply, 404, "ROUTE_NOT_FOUND"),
                &json!({}),
                "{uri}"
            );
        }
    }
}

// 「其他通用错误码」：路由存在但方法不允许为 405 METHOD_NOT_ALLOWED。
#[tokio::test]
async fn wrong_method_returns_method_not_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let router =
        spec_support::router(&dir.path().join("boh.db"), ManualClock::new(NOW).clock()).unwrap();

    for (method, uri) in [
        (Method::POST, "/health"),
        (Method::DELETE, "/api/v1/equipment"),
        (Method::PATCH, "/api/v1/equipment"),
        (
            Method::DELETE,
            "/api/v1/equipment/01890a5d-ac96-774b-bcce-b302099a8301",
        ),
        (
            Method::PATCH,
            "/api/v1/equipment/01890a5d-ac96-774b-bcce-b302099a8301",
        ),
    ] {
        let reply = send(
            &router,
            request(method.clone(), uri, Some(&MANAGER), None).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            assert_error(&reply, 405, "METHOD_NOT_ALLOWED"),
            &json!({}),
            "{method} {uri}"
        );
    }
}

// 「身份与权限通过后，请求体不是合法 JSON、缺少 Content-Type……一律 400 VALIDATION_FAILED」：
// axum 默认的 415 / 422 和纯文本必须转成信封。每个接口用自己的合法请求体，只改变 Content-Type，
// 400 只能来自 Content-Type；修改接口在请求体无法解析时同样是 400，不先查设备是否存在。
#[tokio::test]
async fn malformed_bodies_return_validation_failed() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let router = spec_support::router(&db_path, ManualClock::new(NOW).clock()).unwrap();
    let create_uri = "/api/v1/equipment";
    let update_uri = "/api/v1/equipment/01890a5d-ac96-774b-bcce-b302099a8301";
    let create = json!({
        "command_id": "01890a5d-ac96-774b-bcce-b302099a8401",
        "code": "F1", "name": "Walk-in", "equipment_type": "FRIDGE", "active": true,
    })
    .to_string();
    let update = json!({
        "command_id": "01890a5d-ac96-774b-bcce-b302099a8402",
        "base_revision": 1, "name": "Walk-in", "equipment_type": "FRIDGE", "active": true,
    })
    .to_string();
    let json = Some("application/json");

    for (method, uri, valid) in [
        (Method::POST, create_uri, create.as_str()),
        (Method::PUT, update_uri, update.as_str()),
    ] {
        for (body, content_type) in [
            ("{", json),
            ("", json),
            ("[]", json),
            ("null", json),
            (valid, None),
            (valid, Some("text/plain")),
        ] {
            let req = raw_request(method.clone(), uri, Some(&MANAGER), content_type, body).unwrap();
            let reply = send(&router, req).await.unwrap();
            assert_error(&reply, 400, "VALIDATION_FAILED");
        }
    }
    let reader = spec_support::reader(&db_path).unwrap();
    assert_eq!(spec_support::count(&reader, "store_events").unwrap(), 0);
    assert_eq!(
        spec_support::count(&reader, "processed_commands").unwrap(),
        0
    );

    // 对照：同样的请求体带上正确的 Content-Type 即被受理（修改的目标设备不存在，所以是 404）。
    let reply = send(
        &router,
        raw_request(Method::POST, create_uri, Some(&MANAGER), json, &create).unwrap(),
    )
    .await
    .unwrap();
    assert_success(&reply);
    let reply = send(
        &router,
        raw_request(Method::PUT, update_uri, Some(&MANAGER), json, &update).unwrap(),
    )
    .await
    .unwrap();
    assert_error(&reply, 404, "REFERENCE_NOT_FOUND");
}
