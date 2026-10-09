//! Additional receipt tests for transaction failures, concurrency and configured timezones.

use super::*;

const URI: &str = "/api/v1/receipts";
const NOW: i64 = 1_791_248_400_000;

async fn seed(node: &Node) -> (String, String) {
    let item_id = seed_item(node).await;
    let (status, response) = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/suppliers",
        json!({
            "command_id": master_command_id(2), "code": "S1", "name": "Supplier", "active": true
        }),
    )
    .await;
    assert_eq!(status, 200, "{response}");
    let supplier_id = response["data"]["supplier"]["supplier_id"]
        .as_str()
        .unwrap()
        .to_owned();
    (item_id, supplier_id)
}

fn request(n: u16, supplier_id: &str, item_id: &str, expiries: &[&str]) -> Value {
    let lines: Vec<_> = expiries
        .iter()
        .map(|expires_on| {
            json!({
                "item_id": item_id, "input": {"qty": 5, "unit_code": "g", "base_qty_per_unit": 1},
                "produced_on": "2026-10-01", "expires_on": expires_on, "line_cost_cents": 1000
            })
        })
        .collect();
    json!({
        "command_id": master_command_id(n), "supplier_id": supplier_id, "lines": lines,
        "captured_at": NOW + 60_000, "sent_at": NOW
    })
}

async fn receipt_counts(node: &Node) -> (i64, i64, i64, i64, i64) {
    node.storage
        .readers
        .call(|conn| -> Result<_, StorageError> {
            conn.query_row(
                "SELECT (SELECT count(*) FROM store_events),
                        (SELECT count(*) FROM processed_commands),
                        (SELECT count(*) FROM inventory_lots),
                        (SELECT count(*) FROM inventory_movements),
                        (SELECT coalesce(max(seq), 0) FROM store_events)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .map_err(|error| StorageError::sqlite("查询事件账本", error))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn concurrent_retries_share_lots_and_preserve_warnings_at_an_invalid_clock() {
    let node = node(true).await;
    let (item, supplier) = seed(&node).await;
    let original = request(3, &supplier, &item, &["2026-10-20", "2026-10-15"]);
    let (first, second) = tokio::join!(
        send(node.router.clone(), Method::POST, URI, original.clone()),
        send(node.router.clone(), Method::POST, URI, original.clone()),
    );
    assert_eq!(first.0, 200, "{first:?}");
    assert_eq!(first, second);
    assert_eq!(first.1["warnings"].as_array().unwrap().len(), 2);
    assert_eq!(receipt_counts(&node).await, (3, 3, 2, 2, 3));
    node.clock.set(UnixMillis(i64::MAX));
    let mut retry = original;
    retry["sent_at"] = json!(i64::MIN);
    assert_eq!(
        send(node.router.clone(), Method::POST, URI, retry).await,
        first
    );
    assert_eq!(receipt_counts(&node).await, (3, 3, 2, 2, 3));
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn failures_after_partial_projection_or_at_command_save_roll_back_every_row() {
    let node = node(true).await;
    let (item, supplier) = seed(&node).await;
    let original = request(3, &supplier, &item, &["2026-10-20", "2026-10-15"]);
    for (table, condition) in [
        ("inventory_lots", "NEW.source_line_no = 1"),
        ("inventory_movements", "NEW.movement_no = 1"),
        ("processed_commands", "1"),
    ] {
        node.storage
            .writer
            .call(move |tx| -> Result<(), StorageError> {
                tx.execute_batch(&format!(
                    "CREATE TRIGGER injected_receipt_failure BEFORE INSERT ON {table}
                     WHEN {condition} BEGIN SELECT RAISE(ABORT, 'private receipt failure'); END;"
                ))
                .map_err(|error| StorageError::sqlite("执行数据库", error))?;
                Ok(())
            })
            .await
            .unwrap();
        assert_internal_error(send(node.router.clone(), Method::POST, URI, original.clone()).await);
        assert_eq!(receipt_counts(&node).await, (2, 2, 0, 0, 2), "{table}");
        node.storage
            .writer
            .call(|tx| -> Result<(), StorageError> {
                tx.execute_batch("DROP TRIGGER injected_receipt_failure")
                    .map_err(|error| StorageError::sqlite("执行数据库", error))?;
                Ok(())
            })
            .await
            .unwrap();
    }
    let (status, response) = send(node.router.clone(), Method::POST, URI, original).await;
    assert_eq!(status, 200, "{response}");
    assert_eq!(receipt_counts(&node).await, (3, 3, 2, 2, 3));
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn expiry_warnings_skip_depleted_and_undated_lots_and_do_not_look_ahead() {
    let node = node(true).await;
    let (item, supplier) = seed(&node).await;
    let first = send(
        node.router.clone(),
        Method::POST,
        URI,
        request(3, &supplier, &item, &["2026-10-25"]),
    )
    .await;
    assert_eq!(first.0, 200);
    let old_lot = first.1["data"]["receipt"]["lines"][0]["lot_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let depleted = old_lot.clone();
    node.storage
        .writer
        .call(move |tx| -> Result<(), StorageError> {
            // Projection fixtures for branches not yet reachable through stocktake/waste commands.
            tx.execute(
                "UPDATE inventory_lots SET remaining_qty = 0 WHERE lot_id = ?1",
                [depleted],
            )
            .map_err(|error| StorageError::sqlite("更新库存批次", error))?;
            Ok(())
        })
        .await
        .unwrap();
    let next = send(
        node.router.clone(),
        Method::POST,
        URI,
        request(4, &supplier, &item, &["2026-10-20"]),
    )
    .await;
    assert_eq!(next.0, 200);
    assert_eq!(next.1["warnings"].as_array().unwrap().len(), 1);
    assert_eq!(next.1["warnings"][0]["code"], "CAPTURE_TIME_ADJUSTED");
    node.storage
        .writer
        .call(move |tx| -> Result<(), StorageError> {
            tx.execute(
                "UPDATE inventory_lots SET remaining_qty = 5, expires_at = NULL WHERE lot_id = ?1",
                [old_lot],
            )
            .map_err(|error| StorageError::sqlite("更新库存批次", error))?;
            Ok(())
        })
        .await
        .unwrap();
    let last = send(
        node.router.clone(),
        Method::POST,
        URI,
        request(
            5,
            &supplier,
            &item,
            &["2026-10-20", "2026-10-25", "2026-10-22"],
        ),
    )
    .await;
    assert_eq!(last.0, 200);
    assert_eq!(last.1["warnings"].as_array().unwrap().len(), 2);
    assert_eq!(last.1["warnings"][1]["details"]["line"], 2);
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn receipt_dates_use_configured_timezone_and_replay_preserves_the_saved_expiry() {
    let node = node(true).await;
    let (item, supplier) = seed(&node).await;
    let router = http::router(AppState {
        writer: node.storage.writer.clone(),
        readers: node.storage.readers.clone(),
        clock: node.clock.clock(),
        timezone: parse_timezone("America/Los_Angeles").unwrap(),
        business_day_cutoff: parse_business_day_cutoff("04:00").unwrap(),
        dev_actor_stub: true,
        db_path: node._dir.path().join("boh.db"),
        backup_health: Default::default(),
    });
    let mut body = request(3, &supplier, &item, &["2026-10-05"]);
    body["lines"][0]["produced_on"] = json!("2026-10-05");
    let (status, reply) = send(router, Method::POST, URI, body).await;
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["data"]["receipt"]["business_date"], "2026-10-05");
    // NOW is October 6 01:00 UTC; the next Los Angeles midnight is 07:00 UTC.
    let expires_at = NOW + 6 * 3_600_000 - 1;
    assert_eq!(
        reply["data"]["receipt"]["lines"][0]["expires_at"],
        expires_at
    );
    assert_eq!(node.storage.writer.rebuild_projections().await.unwrap(), 3);
    node.storage
        .readers
        .call(move |conn| -> Result<(), StorageError> {
            assert_eq!(
                conn.query_row("SELECT expires_at FROM inventory_lots", [], |row| row
                    .get::<_, i64>(0))
                    .map_err(|error| StorageError::sqlite("查询库存批次", error))?,
                expires_at
            );
            Ok(())
        })
        .await
        .unwrap();
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn expiry_past_9998_is_a_value_error_checked_before_idempotency() {
    let node = node(true).await;
    let (item, supplier) = seed(&node).await;
    let accepted = request(3, &supplier, &item, &["9998-12-31"]);
    let (status, response) = send(node.router.clone(), Method::POST, URI, accepted.clone()).await;
    assert_eq!(status, 200, "{response}");
    let counts = receipt_counts(&node).await;
    for command in [
        request(3, &supplier, &item, &["9999-01-01"]),
        request(4, &supplier, &item, &["9999-12-31"]),
    ] {
        let (status, response) = send(node.router.clone(), Method::POST, URI, command).await;
        assert_eq!(status, 400, "{response}");
        assert_eq!(response["error"]["code"], "VALIDATION_FAILED");
        assert_eq!(response["error"]["details"], json!({}));
    }
    assert_eq!(receipt_counts(&node).await, counts);
    node.storage.writer_handle.shutdown().await.unwrap();
}
