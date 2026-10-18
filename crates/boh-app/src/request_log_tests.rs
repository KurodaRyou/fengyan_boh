//! Exercise the real middleware and extraction paths with captured output.
use super::{create, node, send};
use crate::test_support::Capture;
use axum::http::Method;
use serde_json::json;
use tracing::instrument::WithSubscriber;

fn access(capture: &Capture) -> String {
    let text = capture.text();
    let lines: Vec<_> = text
        .lines()
        .filter(|line| line.contains("HTTP request completed"))
        .collect();
    assert_eq!(lines.len(), 1, "{text}");
    lines[0].to_owned()
}

#[tokio::test]
async fn access_log_has_parsed_command_and_identity_without_body_or_response() {
    let node = node(true).await;
    let capture = Capture::default();
    let mut command = create(501, "PRIVATE_CODE");
    command["name"] = json!("PRIVATE_BODY");
    let id = command["command_id"].as_str().unwrap().to_owned();
    let result = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/equipment?token=PRIVATE_QUERY",
        command,
    )
    .with_subscriber(capture.subscriber())
    .await;
    assert_eq!(result.0, 200);
    let log = access(&capture);
    for part in [
        "INFO",
        "method=POST",
        "route=\"/api/v1/equipment\"",
        "status=200",
        "command_type=\"equipment.create\"",
        &id,
        "actor_id=",
        "device_id=",
        "elapsed_ms=",
    ] {
        assert!(log.contains(part), "missing {part}: {log}");
    }
    for part in [
        "PRIVATE_",
        "error_code=",
        "reason_codes=",
        "success",
        "equipment_id",
    ] {
        assert!(!log.contains(part), "{log}");
    }
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn access_log_omits_unresolved_identity_and_unparsed_commands() {
    for (stub, body, status, has_actor, has_command) in [
        (false, create(502, "PRIVATE_CODE"), 401, false, false),
        (true, json!({"command_id": "PRIVATE_ID"}), 400, true, false),
        (
            true,
            {
                let mut c = create(503, "PRIVATE_CODE");
                c["name"] = json!("");
                c
            },
            400,
            true,
            true,
        ),
    ] {
        let node = node(stub).await;
        let capture = Capture::default();
        let result = send(node.router.clone(), Method::POST, "/api/v1/equipment", body)
            .with_subscriber(capture.subscriber())
            .await;
        assert_eq!(result.0, status);
        let log = access(&capture);
        assert_eq!(log.contains("actor_id="), has_actor, "{log}");
        assert_eq!(log.contains("device_id="), has_actor, "{log}");
        assert_eq!(log.contains("command_id="), has_command, "{log}");
        assert_eq!(log.contains("command_type="), has_command, "{log}");
        assert!(
            log.contains(result.1["error"]["code"].as_str().unwrap()),
            "{log}"
        );
        assert!(!log.contains("PRIVATE_"), "{log}");
        node.storage.writer_handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn access_log_uses_templates_and_never_unmatched_paths_or_query_values() {
    let node = node(true).await;
    for (method, path, status, template) in [
        (Method::GET, "/PRIVATE_PATH?secret=PRIVATE_QUERY", 404, None),
        (
            Method::DELETE,
            "/api/v1/lots/PRIVATE_LOT?secret=PRIVATE_QUERY",
            405,
            Some("/api/v1/lots/{lot_id}"),
        ),
        (
            Method::GET,
            "/api/v1/lots/PRIVATE_LOT?secret=PRIVATE_QUERY",
            400,
            Some("/api/v1/lots/{lot_id}"),
        ),
        (
            Method::GET,
            "/health?secret=PRIVATE_QUERY",
            200,
            Some("/health"),
        ),
    ] {
        let capture = Capture::default();
        let result = send(node.router.clone(), method, path, json!({}))
            .with_subscriber(capture.subscriber())
            .await;
        assert_eq!(result.0, status);
        let log = access(&capture);
        assert!(!log.contains("PRIVATE_"), "{log}");
        assert_eq!(log.contains("route="), template.is_some(), "{log}");
        if let Some(template) = template {
            assert!(log.contains(template), "{log}");
        }
        assert!(!log.contains("command_id="), "{log}");
    }
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn access_log_of_internal_error_contains_only_the_public_error_code() {
    let node = node(true).await;
    node.storage.writer_handle.shutdown().await.unwrap();
    let capture = Capture::default();
    let result = send(
        node.router,
        Method::POST,
        "/api/v1/equipment",
        create(504, "PRIVATE_BODY"),
    )
    .with_subscriber(capture.subscriber())
    .await;
    assert_eq!(result.0, 500);
    let log = access(&capture);
    assert!(log.contains("INTERNAL_ERROR"), "{log}");
    for part in [
        "writer thread",
        "location=",
        "reason_codes=",
        "PRIVATE_",
        "error=",
    ] {
        assert!(!log.contains(part), "{log}");
    }
}

#[tokio::test]
async fn access_log_reason_codes_are_sorted_unique_and_500_hides_details() {
    use crate::{http::ApiError, request_log};
    use axum::{Router, http::StatusCode, middleware, routing::get};
    let router = Router::new()
        .route("/confirm", get(|| async {
            ApiError::new(StatusCode::CONFLICT, "CONFIRMATION_REQUIRED", "PRIVATE_MESSAGE")
                .with_details(json!({"confirmations": [
                    {"line": 1, "code": "STOCK_SHORTFALL", "details": {"secret": "PRIVATE_DETAILS"}},
                    {"line": 0, "code": "POSSIBLE_DUPLICATE", "details": {}},
                    {"line": 2, "code": "STOCK_SHORTFALL", "details": {}}
                ]}))
        }))
        .route("/error", get(|| async {
            ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "PRIVATE_DIAGNOSTIC", "PRIVATE_MESSAGE")
                .with_details(json!({"confirmations": [{"code": "PRIVATE_REASON"}]}))
        }))
        .layer(middleware::from_fn(request_log::log));
    for (path, status, code, reasons) in [
        ("/confirm", 409, "CONFIRMATION_REQUIRED", true),
        ("/error", 500, "INTERNAL_ERROR", false),
    ] {
        let capture = Capture::default();
        let result = send(router.clone(), Method::GET, path, json!({}))
            .with_subscriber(capture.subscriber())
            .await;
        assert_eq!(result.0, status);
        let log = access(&capture);
        assert!(log.contains(code), "{log}");
        assert_eq!(
            log.contains("reason_codes=\"POSSIBLE_DUPLICATE,STOCK_SHORTFALL\""),
            reasons,
            "{log}"
        );
        assert_eq!(
            log.matches("STOCK_SHORTFALL").count(),
            usize::from(reasons),
            "{log}"
        );
        for part in ["PRIVATE_", "actor_id=", "command_id="] {
            assert!(!log.contains(part), "{log}");
        }
    }
}
