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
        db_path: dir.path().join("boh.db"),
        backup_health: Default::default(),
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

fn temperature(n: u16, equipment_id: &str) -> Value {
    json!({
        "command_id": format!("01890a5d-ac96-774b-bcce-b30209ab{n:04x}"),
        "equipment_id": equipment_id, "celsius_x10": 38,
        "captured_at": 1_791_248_460_000_i64, "sent_at": 1_791_248_400_000_i64
    })
}

async fn temperature_counts(node: &Node) -> (i64, i64, i64, i64) {
    node.storage
        .readers
        .call(|conn| -> Result<_, StorageError> {
            Ok(conn.query_row(
                "SELECT (SELECT count(*) FROM store_events),
                        (SELECT count(*) FROM processed_commands),
                        (SELECT count(*) FROM temperature_readings),
                        (SELECT coalesce(max(seq), 0) FROM store_events)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn concurrent_temperature_retries_share_one_reading_and_saved_warning() {
    let node = node(true).await;
    let (status, equipment) = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/equipment",
        create(1, "F1"),
    )
    .await;
    assert_eq!(status, 200);
    let id = equipment["data"]["equipment"]["equipment_id"]
        .as_str()
        .unwrap();
    let request = temperature(2, id);
    let path = "/api/v1/temperature-readings";
    let (first, second) = tokio::join!(
        send(node.router.clone(), Method::POST, path, request.clone()),
        send(node.router.clone(), Method::POST, path, request.clone()),
    );
    assert_eq!(first, second);
    assert_eq!(first.0, 200);
    assert_eq!(first.1["warnings"][0]["code"], "CAPTURE_TIME_ADJUSTED");
    assert_eq!(temperature_counts(&node).await, (2, 2, 1, 2));
    node.clock.set(UnixMillis(i64::MAX));
    assert_eq!(
        send(node.router.clone(), Method::POST, path, request).await,
        first
    );
    assert_eq!(temperature_counts(&node).await, (2, 2, 1, 2));
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn temperature_projection_and_receipt_failures_rollback_and_allow_original_retry() {
    let node = node(true).await;
    let (status, equipment) = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/equipment",
        create(1, "F1"),
    )
    .await;
    assert_eq!(status, 200);
    let id = equipment["data"]["equipment"]["equipment_id"]
        .as_str()
        .unwrap();
    let request = temperature(2, id);
    let path = "/api/v1/temperature-readings";
    for table in ["temperature_readings", "processed_commands"] {
        node.storage
            .writer
            .call(move |tx| -> Result<(), StorageError> {
                tx.execute_batch(&format!(
                    "CREATE TRIGGER injected_temperature_failure BEFORE INSERT ON {table}
                     BEGIN SELECT RAISE(ABORT, 'private temperature failure'); END;"
                ))?;
                Ok(())
            })
            .await
            .unwrap();
        assert_internal_error(send(node.router.clone(), Method::POST, path, request.clone()).await);
        assert_eq!(temperature_counts(&node).await, (1, 1, 0, 1));
        node.storage
            .writer
            .call(|tx| -> Result<(), StorageError> {
                tx.execute_batch("DROP TRIGGER injected_temperature_failure")?;
                Ok(())
            })
            .await
            .unwrap();
    }
    let (status, logged) = send(node.router.clone(), Method::POST, path, request).await;
    assert_eq!(status, 200);
    assert_eq!(logged["warnings"][0]["code"], "CAPTURE_TIME_ADJUSTED");
    assert_eq!(temperature_counts(&node).await, (2, 2, 1, 2));
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn health_reports_wal_io_failure_and_missing_wal_without_exposing_paths() {
    let node = node(true).await;
    let parent = node._dir.path().join("plain-file");
    std::fs::write(&parent, "file").unwrap();
    for (path, expected_status) in [
        (parent.join("boh.db"), 500),
        (node._dir.path().join("missing.db"), 200),
    ] {
        let router = http::router(AppState {
            writer: node.storage.writer.clone(),
            readers: node.storage.readers.clone(),
            clock: node.clock.clock(),
            timezone: parse_timezone("Asia/Shanghai").unwrap(),
            business_day_cutoff: parse_business_day_cutoff("04:00").unwrap(),
            dev_actor_stub: false,
            db_path: path,
            backup_health: Default::default(),
        });
        let (status, body) = send(router, Method::GET, "/health", json!({})).await;
        assert_eq!(status, expected_status);
        if status == 500 {
            assert_eq!(
                body,
                json!({
                    "success": false, "data": null, "warnings": [],
                    "error": {"code": "INTERNAL_ERROR", "message": "internal error", "details": {}}
                })
            );
        } else {
            assert_eq!(body["data"]["wal_size_bytes"], json!(0));
        }
    }
    node.storage.writer_handle.shutdown().await.unwrap();
}

fn master_command_id(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209ad{n:04x}")
}

fn item_body(n: u16) -> Value {
    json!({"command_id": master_command_id(n), "code": "FLOUR", "name": "面粉", "base_unit": "g",
    "category": "RAW", "active": true, "units": [
        {"unit_code": "bag", "base_qty_per_unit": 25000},
        {"unit_code": "cup", "base_qty_per_unit": 120}
    ]})
}

async fn seed_item(node: &Node) -> String {
    let (status, body) = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/items",
        item_body(1),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    body["data"]["item"]["item_id"].as_str().unwrap().to_owned()
}

fn recipe_body(item_id: &str) -> Value {
    json!({"command_id": master_command_id(2), "code": "R1", "name": "配方", "active": true,
        "output_item_id": item_id, "output_qty_per_batch": 2,
        "lines": [{"item_id": item_id, "qty_per_batch": 4}]})
}

#[tokio::test]
async fn child_projection_failures_restore_complete_snapshots_and_allow_original_retry() {
    let node = node(true).await;
    let item_id = seed_item(&node).await;
    let original = send(
        node.router.clone(),
        Method::GET,
        "/api/v1/items",
        Value::Null,
    )
    .await;
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute_batch(
                "CREATE TRIGGER injected_item_failure BEFORE INSERT ON item_units
            WHEN NEW.unit_code = 'box' BEGIN SELECT RAISE(ABORT, 'private child failure'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let update = json!({"command_id": master_command_id(3), "base_revision": 1,
    "name": "新面粉", "category": "SEMI", "active": false, "units": [
        {"unit_code": "bag", "base_qty_per_unit": 20000},
        {"unit_code": "box", "base_qty_per_unit": 1000}
    ]});
    let path = format!("/api/v1/items/{item_id}");
    assert_internal_error(send(node.router.clone(), Method::PUT, &path, update.clone()).await);
    assert_eq!(counts(&node).await, (1, 1, 0));
    assert_eq!(
        send(
            node.router.clone(),
            Method::GET,
            "/api/v1/items",
            Value::Null
        )
        .await,
        original
    );
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute_batch("DROP TRIGGER injected_item_failure")?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        send(node.router.clone(), Method::PUT, &path, update)
            .await
            .0,
        200
    );

    let (status, recipe) = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/recipes",
        recipe_body(&item_id),
    )
    .await;
    assert_eq!(status, 200, "{recipe}");
    let recipe_id = recipe["data"]["recipe"]["recipe_id"].as_str().unwrap();
    let original = send(
        node.router.clone(),
        Method::GET,
        "/api/v1/recipes",
        Value::Null,
    )
    .await;
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute_batch(
                "CREATE TRIGGER injected_recipe_failure BEFORE INSERT ON recipe_lines
            WHEN NEW.version = 2 BEGIN SELECT RAISE(ABORT, 'private child failure'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let version = json!({"command_id": master_command_id(4), "base_revision": 1,
        "output_qty_per_batch": 3, "lines": [{"item_id": item_id, "qty_per_batch": 6}]});
    let path = format!("/api/v1/recipes/{recipe_id}/versions");
    assert_internal_error(send(node.router.clone(), Method::POST, &path, version.clone()).await);
    assert_eq!(counts(&node).await, (3, 3, 0));
    assert_eq!(
        send(
            node.router.clone(),
            Method::GET,
            "/api/v1/recipes",
            Value::Null
        )
        .await,
        original
    );
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute_batch("DROP TRIGGER injected_recipe_failure")?;
            Ok(())
        })
        .await
        .unwrap();
    let (status, body) = send(node.router.clone(), Method::POST, &path, version).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["data"]["recipe"]["versions"].as_array().unwrap().len(),
        2
    );
    assert_eq!(counts(&node).await, (4, 4, 0));
    node.storage
        .readers
        .call(|conn| -> Result<(), StorageError> {
            let (n, max): (i64, i64) =
                conn.query_row("SELECT count(*), max(seq) FROM store_events", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            assert_eq!((n, max), (4, 4));
            Ok(())
        })
        .await
        .unwrap();
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_recipe_versions_serialize_and_retries_keep_the_original_snapshot() {
    let node = node(true).await;
    let item_id = seed_item(&node).await;
    let (status, recipe) = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/recipes",
        recipe_body(&item_id),
    )
    .await;
    assert_eq!(status, 200);
    let recipe_id = recipe["data"]["recipe"]["recipe_id"].as_str().unwrap();
    let path = format!("/api/v1/recipes/{recipe_id}/versions");
    let version = json!({"command_id": master_command_id(3), "base_revision": 1,
        "output_qty_per_batch": 3, "lines": [{"item_id": item_id, "qty_per_batch": 6}]});
    let (a, b) = tokio::join!(
        send(node.router.clone(), Method::POST, &path, version.clone()),
        send(node.router.clone(), Method::POST, &path, version.clone()),
    );
    assert_eq!(a, b);
    assert_eq!(a.0, 200);
    assert_eq!(counts(&node).await, (3, 3, 0));
    let competing = |n| {
        json!({"command_id": master_command_id(n), "base_revision": 2,
        "output_qty_per_batch": n, "lines": [{"item_id": item_id, "qty_per_batch": 8}]})
    };
    let (c, d) = tokio::join!(
        send(node.router.clone(), Method::POST, &path, competing(4)),
        send(node.router.clone(), Method::POST, &path, competing(5)),
    );
    assert!(matches!((c.0, d.0), (200, 409) | (409, 200)));
    let rejected = if c.0 == 409 { c.1 } else { d.1 };
    assert_eq!(rejected["error"]["code"], "REVISION_CONFLICT");
    assert_eq!(rejected["error"]["details"], json!({"current_revision": 3}));
    assert_eq!(
        send(node.router.clone(), Method::POST, &path, version).await,
        a
    );
    assert_eq!(counts(&node).await, (4, 4, 0));
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn remaining_master_data_revision_overflows_do_not_commit_partial_changes() {
    for (table, url, key) in [
        ("items", "/api/v1/items", "item"),
        ("recipes", "/api/v1/recipes", "recipe"),
        ("suppliers", "/api/v1/suppliers", "supplier"),
        ("waste_reasons", "/api/v1/waste-reasons", "waste_reason"),
    ] {
        let node = node(true).await;
        let item_id = seed_item(&node).await;
        let body = match key {
            "item" => {
                let mut body = item_body(2);
                body["code"] = json!("SECOND");
                body
            }
            "recipe" => recipe_body(&item_id),
            _ => {
                json!({"command_id": master_command_id(2), "code": "C1", "name": "原名", "active": true})
            }
        };
        let (status, created) = send(node.router.clone(), Method::POST, url, body).await;
        assert_eq!(status, 200, "{created}");
        let id = created["data"][key][format!("{key}_id")].as_str().unwrap();
        let id_owned = id.to_owned();
        node.storage
            .writer
            .call(move |tx| -> Result<(), StorageError> {
                tx.execute(
                    &format!("UPDATE {table} SET revision = ?1 WHERE id = ?2"),
                    boh_storage::rusqlite::params![i64::MAX, id_owned],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let mut update = json!({"command_id": master_command_id(3), "base_revision": i64::MAX,
            "name": "新名", "active": false});
        if key == "item" {
            update["category"] = json!("RAW");
            update["units"] = json!([]);
        }
        assert_internal_error(
            send(
                node.router.clone(),
                Method::PUT,
                &format!("{url}/{id}"),
                update,
            )
            .await,
        );
        if key == "recipe" {
            // Appending a version must also check revision before changing history.
            assert_internal_error(send(node.router.clone(), Method::POST, &format!("{url}/{id}/versions"),
                json!({"command_id": master_command_id(4), "base_revision": i64::MAX,
                    "output_qty_per_batch": 3, "lines": [{"item_id": item_id, "qty_per_batch": 6}]})).await);
        }
        assert_eq!(counts(&node).await, (2, 2, 0));
        let (_, listed) = send(node.router.clone(), Method::GET, url, Value::Null).await;
        let mut expected = created["data"][key].clone();
        expected["revision"] = json!(i64::MAX);
        let rows = listed["data"][table].as_array().unwrap();
        assert!(rows.contains(&expected), "{listed}");
        node.storage.writer_handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn imported_snapshots_require_objects_in_nested_arrays_and_fail_atomically() {
    use boh_domain::{AggregateId, CommandId, EventId};
    use boh_storage::ledger::{self, Command, Event, ExecuteError};

    let node = node(true).await;
    let item_id = seed_item(&node).await;
    let (status, recipe) = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/recipes",
        recipe_body(&item_id),
    )
    .await;
    assert_eq!(status, 200);
    let recipe_id = recipe["data"]["recipe"]["recipe_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let items_before = send(
        node.router.clone(),
        Method::GET,
        "/api/v1/items",
        Value::Null,
    )
    .await;
    let recipes_before = send(
        node.router.clone(),
        Method::GET,
        "/api/v1/recipes",
        Value::Null,
    )
    .await;
    let mut item = items_before.1["data"]["items"][0].clone();
    item.as_object_mut().unwrap().remove("item_id");
    item.as_object_mut().unwrap().remove("revision");
    item["name"] = json!("tampered");
    item["units"][0] = json!(item["units"][0].to_string());
    let mut version_string = recipe["data"]["recipe"].clone();
    version_string.as_object_mut().unwrap().remove("recipe_id");
    version_string.as_object_mut().unwrap().remove("revision");
    version_string["name"] = json!("tampered");
    let mut line_string = version_string.clone();
    version_string["versions"][0] = json!(version_string["versions"][0].to_string());
    line_string["versions"][0]["lines"][0] =
        json!(line_string["versions"][0]["lines"][0].to_string());
    for (n, (entity, id, snapshot)) in (3..).zip([
        ("ITEM", item_id.clone(), item),
        ("RECIPE", recipe_id.clone(), version_string),
        ("RECIPE", recipe_id, line_string),
    ]) {
        let command_id = CommandId::parse(&master_command_id(n)).unwrap();
        let id = AggregateId::parse(&id).unwrap();
        let payload =
            json!({"entity": entity, "source": "HQ_PACKAGE", "snapshot": snapshot}).to_string();
        let recorded_at = node.clock.clock().now();
        let result = node
            .storage
            .writer
            .call(move |tx| -> Result<String, ExecuteError> {
                ledger::execute(
                    tx,
                    Command {
                        id: command_id,
                        command_type: "test.import",
                        request: "{}",
                        recorded_at,
                    },
                    |ledger| {
                        ledger.append(&Event {
                            id: EventId::from_parts(recorded_at, ledger::entropy(tx)?)
                                .map_err(StorageError::from)?,
                            event_type: "MASTER_DATA_CHANGED".into(),
                            schema_version: 1,
                            aggregate_type: entity.into(),
                            aggregate_id: id,
                            aggregate_version: 2,
                            command_id,
                            actor_id: AggregateId::parse("00000000-0000-7000-8000-000000000000")
                                .map_err(StorageError::from)?,
                            device_id: AggregateId::parse("00000000-0000-7000-8000-000000000001")
                                .map_err(StorageError::from)?,
                            business_date: "2026-10-06".into(),
                            occurred_at: recorded_at,
                            recorded_at,
                            payload,
                        })?;
                        Ok("{}".into())
                    },
                )
            })
            .await;
        assert!(result.is_err());
        assert_eq!(counts(&node).await, (2, 2, 0));
        assert_eq!(
            send(
                node.router.clone(),
                Method::GET,
                "/api/v1/items",
                Value::Null
            )
            .await,
            items_before
        );
        assert_eq!(
            send(
                node.router.clone(),
                Method::GET,
                "/api/v1/recipes",
                Value::Null
            )
            .await,
            recipes_before
        );
    }
    node.storage.writer_handle.shutdown().await.unwrap();
}
