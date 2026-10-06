//! Implementation tests for concurrency, failure atomicity and production identity defaults.

use std::convert::Infallible;
use std::num::NonZeroUsize;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request},
};
use boh_domain::time::{parse_business_day_cutoff, parse_timezone};
use boh_domain::{StoreId, UnixMillis};
use boh_storage::{Storage, StorageError, clock::ManualClock};
use serde_json::{Value, json};

use crate::{AppState, http};

struct Node {
    _dir: tempfile::TempDir,
    storage: Storage,
    router: Router,
    clock: ManualClock,
}

async fn node(dev_actor_stub: bool) -> Node {
    let dir = tempfile::tempdir().unwrap();
    let storage = boh_storage::open(&dir.path().join("boh.db"), NonZeroUsize::MIN).unwrap();
    let clock = ManualClock::new(UnixMillis(1_791_248_400_000));
    let store_id = StoreId::parse("01890a5d-ac96-774b-bcce-b302099a8050").unwrap();
    let now = clock.clock().now();
    storage
        .writer
        .call(move |tx| boh_storage::store::initialize(tx, store_id, now))
        .await
        .unwrap();
    let router = http::router(AppState {
        writer: storage.writer.clone(),
        readers: storage.readers.clone(),
        clock: clock.clock(),
        timezone: parse_timezone("Asia/Shanghai").unwrap(),
        business_day_cutoff: parse_business_day_cutoff("04:00").unwrap(),
        dev_actor_stub,
    });
    Node {
        _dir: dir,
        storage,
        router,
        clock,
    }
}

fn create(n: u16, code: &str) -> Value {
    json!({ "command_id": format!("01890a5d-ac96-774b-bcce-b30209ab{n:04x}"), "code": code,
        "name": "Fridge", "equipment_type": "FRIDGE", "active": true })
}

async fn send(router: Router, method: Method, path: &str, body: Value) -> (u16, Value) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("X-Dev-Employee-Id", "01890a5d-ac96-774b-bcce-b302099a8101")
        .header("X-Dev-Device-Id", "01890a5d-ac96-774b-bcce-b302099a8201")
        .header("X-Dev-Role", "MANAGER")
        .body(Body::from(body.to_string()))
        .unwrap();
    async fn call<S>(mut service: S, request: Request<Body>) -> axum::response::Response
    where
        S: axum::ServiceExt<Request<Body>, Response = axum::response::Response, Error = Infallible>,
    {
        let Ok(()) = std::future::poll_fn(|cx| service.poll_ready(cx)).await;
        let Ok(response) = service.call(request).await;
        response
    }
    let response = call(router, request).await;
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn counts(node: &Node) -> (i64, i64, i64) {
    node.storage
        .readers
        .call(|conn| -> Result<_, StorageError> {
            Ok(conn.query_row(
                "SELECT (SELECT count(*) FROM store_events),
            (SELECT count(*) FROM processed_commands), (SELECT count(*) FROM equipment)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
        .await
        .unwrap()
}

fn assert_internal_error((status, body): (u16, Value)) {
    assert_eq!(status, 500);
    assert_eq!(
        body,
        json!({
            "success": false, "data": null, "warnings": [],
            "error": { "code": "INTERNAL_ERROR", "message": "internal error", "details": {} }
        })
    );
}

#[tokio::test]
async fn revision_overflow_is_internal_and_does_not_commit() {
    let node = node(true).await;
    let (status, original) = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/equipment",
        create(1, "F1"),
    )
    .await;
    assert_eq!(status, 200);
    let id = original["data"]["equipment"]["equipment_id"]
        .as_str()
        .unwrap();
    let path = format!("/api/v1/equipment/{id}");
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute("UPDATE equipment SET revision = ?1", [i64::MAX])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_internal_error(
        send(
            node.router.clone(),
            Method::PUT,
            &path,
            json!({
                "command_id": "01890a5d-ac96-774b-bcce-b30209ab0002",
                "base_revision": i64::MAX, "name": "Changed",
                "equipment_type": "OVEN", "active": false
            }),
        )
        .await,
    );
    assert_eq!(counts(&node).await, (1, 1, 1));
    let (status, body) = send(
        node.router.clone(),
        Method::GET,
        "/api/v1/equipment",
        Value::Null,
    )
    .await;
    assert_eq!(status, 200);
    let mut expected = original["data"]["equipment"].clone();
    expected["revision"] = json!(i64::MAX);
    assert_eq!(body["data"]["equipment"][0], expected);
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn server_business_date_failure_is_internal_and_allows_original_retry() {
    let node = node(true).await;
    let now = node.clock.clock().now();
    let original = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/equipment",
        create(1, "F1"),
    )
    .await;
    assert_eq!(original.0, 200);
    let id = original.1["data"]["equipment"]["equipment_id"]
        .as_str()
        .unwrap();
    let path = format!("/api/v1/equipment/{id}");
    let update = json!({
        "command_id": "01890a5d-ac96-774b-bcce-b30209ab0002",
        "base_revision": 1, "name": "Changed",
        "equipment_type": "OVEN", "active": false
    });
    node.clock.set(UnixMillis(i64::MAX));
    assert_eq!(
        send(
            node.router.clone(),
            Method::POST,
            "/api/v1/equipment",
            create(1, "F1")
        )
        .await,
        original
    );
    assert_internal_error(send(node.router.clone(), Method::PUT, &path, update.clone()).await);
    assert_eq!(counts(&node).await, (1, 1, 1));
    let (_, body) = send(
        node.router.clone(),
        Method::GET,
        "/api/v1/equipment",
        Value::Null,
    )
    .await;
    assert_eq!(
        body["data"]["equipment"][0],
        original.1["data"]["equipment"]
    );
    node.clock.set(now);
    let (status, body) = send(node.router.clone(), Method::PUT, &path, update).await;
    assert_eq!(status, 200);
    assert_eq!(body["data"]["equipment"]["revision"], 2);
    assert_eq!(counts(&node).await, (2, 2, 1));
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_commands_share_idempotency_and_enforce_business_conflicts() {
    let node = node(true).await;
    let (a, b) = tokio::join!(
        send(
            node.router.clone(),
            Method::POST,
            "/api/v1/equipment",
            create(1, "F1")
        ),
        send(
            node.router.clone(),
            Method::POST,
            "/api/v1/equipment",
            create(1, "F1")
        ),
    );
    assert_eq!(a, b);
    assert_eq!(a.0, 200);
    assert_eq!(counts(&node).await, (1, 1, 1));
    let id = a.1["data"]["equipment"]["equipment_id"].as_str().unwrap();
    let path = format!("/api/v1/equipment/{id}");
    let update = |n: u16, name: &str| {
        json!({ "command_id": format!("01890a5d-ac96-774b-bcce-b30209ab{n:04x}"),
        "base_revision": 1, "name": name, "equipment_type": "OVEN", "active": false })
    };
    let (a, b) = tokio::join!(
        send(node.router.clone(), Method::PUT, &path, update(2, "A")),
        send(node.router.clone(), Method::PUT, &path, update(3, "B")),
    );
    assert!(matches!((a.0, b.0), (200, 409) | (409, 200)));
    let rejected = if a.0 == 409 { a.1 } else { b.1 };
    assert_eq!(rejected["error"]["code"], "REVISION_CONFLICT");
    let (a, b) = tokio::join!(
        send(
            node.router.clone(),
            Method::POST,
            "/api/v1/equipment",
            create(4, "F2")
        ),
        send(
            node.router.clone(),
            Method::POST,
            "/api/v1/equipment",
            create(5, "F2")
        ),
    );
    assert!(matches!((a.0, b.0), (200, 409) | (409, 200)));
    let rejected = if a.0 == 409 { a.1 } else { b.1 };
    assert_eq!(rejected["error"]["code"], "CODE_ALREADY_EXISTS");
    assert_eq!(counts(&node).await, (3, 3, 2));
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn projection_failure_rolls_back_event_and_command_then_allows_original_retry() {
    let node = node(true).await;
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute_batch(
                "CREATE TRIGGER injected_failure BEFORE INSERT ON equipment
            BEGIN SELECT RAISE(ABORT, 'private SQL failure'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (status, body) = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/equipment",
        create(1, "F1"),
    )
    .await;
    assert_eq!(status, 500);
    assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
    assert!(!body.to_string().contains("private"));
    assert_eq!(counts(&node).await, (0, 0, 0));
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute_batch("DROP TRIGGER injected_failure")?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        send(
            node.router.clone(),
            Method::POST,
            "/api/v1/equipment",
            create(1, "F1")
        )
        .await
        .0,
        200
    );
    node.storage
        .readers
        .call(|conn| -> Result<(), StorageError> {
            assert_eq!(
                conn.query_row("SELECT seq FROM store_events", [], |r| r.get::<_, i64>(0))?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(counts(&node).await, (1, 1, 1));
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn disabled_dev_identity_rejects_headers_even_for_invalid_bodies() {
    let node = node(false).await;
    for (method, path) in [
        (Method::POST, "/api/v1/equipment"),
        (Method::PUT, "/api/v1/equipment/bad-id"),
        (Method::GET, "/api/v1/equipment"),
    ] {
        let (status, body) = send(node.router.clone(), method, path, json!({})).await;
        assert_eq!(status, 401);
        assert_eq!(body["error"]["code"], "UNAUTHENTICATED");
    }
    assert_eq!(counts(&node).await, (0, 0, 0));
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn public_rebuild_matches_online_state_and_keeps_original_replies() {
    let node = node(true).await;
    let original = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/equipment",
        create(1, "F1"),
    )
    .await;
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute("UPDATE equipment SET name = 'corrupted'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(node.storage.writer.rebuild_projections().await.unwrap(), 1);
    let (_, body) = send(
        node.router.clone(),
        Method::GET,
        "/api/v1/equipment",
        Value::Null,
    )
    .await;
    assert_eq!(
        body["data"]["equipment"][0],
        original.1["data"]["equipment"]
    );
    let repeated = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/equipment",
        create(1, "F1"),
    )
    .await;
    assert_eq!(repeated, original);
    assert_eq!(counts(&node).await, (1, 1, 1));
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn clock_regression_keeps_raw_time_and_does_not_break_retries() {
    let node = node(true).await;
    let original = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/equipment",
        create(1, "F1"),
    )
    .await;
    node.clock.set(UnixMillis(-1));
    assert_eq!(
        send(
            node.router.clone(),
            Method::POST,
            "/api/v1/equipment",
            create(1, "F1")
        )
        .await,
        original
    );
    assert_eq!(
        send(
            node.router.clone(),
            Method::POST,
            "/api/v1/equipment",
            create(2, "F2")
        )
        .await
        .0,
        200
    );
    node.storage
        .readers
        .call(|conn| -> Result<(), StorageError> {
            assert_eq!(
                conn.query_row(
                    "SELECT recorded_at FROM store_events WHERE seq = 2",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                -1
            );
            Ok(())
        })
        .await
        .unwrap();
    node.storage.writer_handle.shutdown().await.unwrap();
}
