//! Regression tests for the extraction's observable validation order.
use super::*;

const NOW: i64 = 1_791_248_400_000;
const UNKNOWN: &str = "01890a5d-ac96-774b-bcce-b302099affff";
const WASTE: &str = "/api/v1/waste-records";
const RECEIPTS: &str = "/api/v1/receipts";

async fn seed(node: &Node) -> (String, String) {
    let reason = send(node.router.clone(), Method::POST, "/api/v1/waste-reasons", json!({
        "command_id": master_command_id(10), "code": "EXPIRED", "name": "Expired", "active": true
    })).await;
    assert_eq!(reason.0, 200, "{reason:?}");
    let item = seed_item(node).await;
    let (status, reply) = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/suppliers",
        json!({
            "command_id": master_command_id(2), "code": "S", "name": "Supplier", "active": true
        }),
    )
    .await;
    assert_eq!(status, 200, "{reply}");
    (
        item,
        reply["data"]["supplier"]["supplier_id"]
            .as_str()
            .unwrap()
            .into(),
    )
}

fn receipt(item: &str, supplier: &str, count: usize) -> Value {
    let line = json!({"item_id": item, "input": {"qty":"5", "unit_code": "g", "base_qty_per_unit":"1"},
        "produced_on": "2026-10-01", "expires_on": "2026-10-20", "line_cost_cents": 0});
    json!({"command_id": master_command_id(3), "supplier_id": supplier, "lines": vec![line; count],
        "occurred_at": NOW, "clock_synced_at": NOW})
}

fn waste(item: &str) -> Value {
    json!({"item_id": item, "input": {"qty":"1", "unit_code": "g", "base_qty_per_unit":"1"}, "reason_code": "EXPIRED"})
}

async fn assert_waste_error(
    node: &Node,
    lines: Vec<Value>,
    status: u16,
    code: &str,
    details: Value,
) {
    for (uri, body) in [
        ("/api/v1/waste-records/precheck", json!({"lines": lines})),
        (
            WASTE,
            json!({"command_id": master_command_id(4), "lines": lines, "occurred_at": NOW, "clock_synced_at": NOW}),
        ),
    ] {
        let (actual, reply) = send(node.router.clone(), Method::POST, uri, body).await;
        assert_eq!(actual, status, "{uri}: {reply}");
        assert_eq!(reply["error"]["code"], code, "{uri}: {reply}");
        assert_eq!(reply["error"]["details"], details, "{uri}: {reply}");
    }
}

#[tokio::test]
async fn waste_checks_each_line_before_loading_later_balances_or_confirming_shortage() {
    let node = node(true).await;
    let (item, supplier) = seed(&node).await;
    let mut no_lots = waste(&item);
    let mut unknown_item = waste(UNKNOWN);
    unknown_item["reason_code"] = json!("MISSING");
    assert_waste_error(
        &node,
        vec![no_lots.clone(), unknown_item.clone()],
        409,
        "ITEM_HAS_NO_LOTS",
        json!({"line": 0, "item_id": item}),
    )
    .await;
    // On one line: item/unit, then reason, then specified-lot reference.
    no_lots["input"]["unit_code"] = json!("missing");
    no_lots["reason_code"] = json!("MISSING");
    no_lots["lot_id"] = json!("RAW-FLOUR-20261006-999");
    assert_waste_error(
        &node,
        vec![no_lots.clone()],
        400,
        "UNKNOWN_UNIT",
        json!({"line": 0, "item_id": item, "unit_code": "missing"}),
    )
    .await;
    no_lots["input"]["unit_code"] = json!("g");
    assert_waste_error(
        &node,
        vec![no_lots.clone()],
        404,
        "REFERENCE_NOT_FOUND",
        json!({"entity": "WASTE_REASON", "code": "MISSING"}),
    )
    .await;
    no_lots["reason_code"] = json!("EXPIRED");
    assert_waste_error(
        &node,
        vec![no_lots],
        404,
        "REFERENCE_NOT_FOUND",
        json!({"entity": "LOT", "id": "RAW-FLOUR-20261006-999"}),
    )
    .await;
    let received = send(
        node.router.clone(),
        Method::POST,
        RECEIPTS,
        receipt(&item, &supplier, 1),
    )
    .await;
    assert_eq!(received.0, 200, "{received:?}");
    let mut shortage = waste(&item);
    shortage["input"]["qty"] = json!("6");
    assert_waste_error(
        &node,
        vec![shortage, unknown_item],
        404,
        "REFERENCE_NOT_FOUND",
        json!({"entity": "ITEM", "id": UNKNOWN}),
    )
    .await;
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn waste_references_precede_stock_limit_and_stock_limit_precedes_later_lines() {
    let node = node(true).await;
    let (item, supplier) = seed(&node).await;
    assert_eq!(
        send(
            node.router.clone(),
            Method::POST,
            RECEIPTS,
            receipt(&item, &supplier, 2)
        )
        .await
        .0,
        200
    );
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute(
                "UPDATE inventory_lots SET remaining_qty = CASE WHEN lot_serial = 1 THEN -1000000000000 ELSE 0 END",
                [],
            )
            .map_err(|e| StorageError::sqlite("设置库存下限夹具", e))?;
            Ok(())
        })
        .await
        .unwrap();
    let mut first = waste(&item);
    first["reason_code"] = json!("MISSING");
    assert_waste_error(
        &node,
        vec![first.clone()],
        404,
        "REFERENCE_NOT_FOUND",
        json!({"entity": "WASTE_REASON", "code": "MISSING"}),
    )
    .await;
    first["reason_code"] = json!("EXPIRED");
    first["lot_id"] = json!("RAW-FLOUR-20261006-999");
    assert_waste_error(
        &node,
        vec![first],
        404,
        "REFERENCE_NOT_FOUND",
        json!({"entity": "LOT", "id": "RAW-FLOUR-20261006-999"}),
    )
    .await;
    assert_waste_error(
        &node,
        vec![waste(&item), waste(UNKNOWN)],
        409,
        "STOCK_LIMIT_EXCEEDED",
        json!({"line": 0, "item_id": item}),
    )
    .await;
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn stored_state_outside_the_stock_limit_is_an_internal_error_not_a_line_conflict() {
    let node = node(true).await;
    let (item, supplier) = seed(&node).await;
    assert_eq!(
        send(
            node.router.clone(),
            Method::POST,
            RECEIPTS,
            receipt(&item, &supplier, 2)
        )
        .await
        .0,
        200
    );
    // Each lot is within its CHECK constraint, but the net book is 2L. Only a
    // projection fault can store that; no request line caused it.
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute(
                "UPDATE inventory_lots SET remaining_qty = 1000000000000",
                [],
            )
            .map_err(|e| StorageError::sqlite("设置越界投影夹具", e))?;
            Ok(())
        })
        .await
        .unwrap();
    assert_waste_error(&node, vec![waste(&item)], 500, "INTERNAL_ERROR", json!({})).await;
    let mut again = receipt(&item, &supplier, 1);
    again["command_id"] = json!(master_command_id(5));
    let (status, reply) = send(node.router.clone(), Method::POST, RECEIPTS, again).await;
    assert_eq!(status, 500, "{reply}");
    assert_eq!(reply["error"]["code"], "INTERNAL_ERROR", "{reply}");
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn waste_accepts_intermediate_sum_above_stock_limit_and_updates_the_next_line() {
    let node = node(true).await;
    let (item, supplier) = seed(&node).await;
    assert_eq!(
        send(
            node.router.clone(),
            Method::POST,
            RECEIPTS,
            receipt(&item, &supplier, 3)
        )
        .await
        .0,
        200
    );
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            // Only the final net book is limited; summing the positive lots
            // temporarily exceeds L before the negative lot cancels it.
            for (serial, qty) in [(1, 1_000_000_000_000_i64), (2, 1000), (3, -1000)] {
                tx.execute(
                    "UPDATE inventory_lots SET remaining_qty = ?2 WHERE lot_serial = ?1",
                    [serial, qty],
                )
                .map_err(|e| StorageError::sqlite("设置库存上限夹具", e))?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let lines = vec![waste(&item), waste(&item)];
    let precheck = send(
        node.router.clone(),
        Method::POST,
        "/api/v1/waste-records/precheck",
        json!({"lines": lines}),
    )
    .await;
    assert_eq!(precheck.0, 200, "{precheck:?}");
    let logged = send(node.router.clone(), Method::POST, WASTE, json!({"command_id": master_command_id(4), "lines": lines, "occurred_at": NOW, "clock_synced_at": NOW})).await;
    assert_eq!(logged.0, 200, "{logged:?}");
    for (index, qty) in ["1000000000", "999999999"].into_iter().enumerate() {
        assert_eq!(precheck.1["data"]["lines"][index]["item_book_qty"], qty);
        assert_eq!(
            logged.1["data"]["waste_record"]["lines"][index]["item_book_qty"],
            qty
        );
        assert_eq!(
            precheck.1["data"]["lines"][index]["alloc"],
            logged.1["data"]["waste_record"]["lines"][index]["alloc"]
        );
    }
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn receipt_validation_order_and_command_local_serial_exhaustion_are_preserved() {
    let node = node(true).await;
    let (item, supplier) = seed(&node).await;
    assert_eq!(
        send(
            node.router.clone(),
            Method::POST,
            RECEIPTS,
            receipt(&item, &supplier, 1)
        )
        .await
        .0,
        200
    );
    node.storage
        .writer
        .call(|tx| -> Result<(), StorageError> {
            tx.execute(
                "UPDATE inventory_movements SET lot_id = 'RAW-FLOUR-20261006-998'",
                [],
            )
            .map_err(|e| StorageError::sqlite("设置流水外键夹具", e))?;
            tx.execute(
                "UPDATE inventory_lots SET lot_id = 'RAW-FLOUR-20261006-998', lot_serial = 998",
                [],
            )
            .map_err(|e| StorageError::sqlite("设置流水号边界夹具", e))?;
            Ok(())
        })
        .await
        .unwrap();
    let mut body = receipt(&item, &supplier, 2);
    body["command_id"] = json!(master_command_id(4));
    body["lines"][1]["item_id"] = json!(UNKNOWN);
    body["lines"][1]["produced_on"] = json!("2026-10-10");
    for (code, status, details) in [
        (
            "REFERENCE_NOT_FOUND",
            404,
            json!({"entity": "ITEM", "id": UNKNOWN}),
        ),
        (
            "INVALID_LOT_DATES",
            400,
            json!({"line": 1, "item_id": item, "reason": "PRODUCED_IN_FUTURE"}),
        ),
        (
            "LOT_SERIAL_EXHAUSTED",
            409,
            json!({"line": 1, "item_id": item, "lot_date": "2026-10-06"}),
        ),
    ] {
        let reply = send(node.router.clone(), Method::POST, RECEIPTS, body.clone()).await;
        assert_eq!(reply.0, status, "{reply:?}");
        assert_eq!(reply.1["error"]["code"], code);
        assert_eq!(reply.1["error"]["details"], details);
        if code == "REFERENCE_NOT_FOUND" {
            body["lines"][1]["item_id"] = json!(item);
        } else {
            body["lines"][1]["produced_on"] = json!("2026-10-01");
        }
    }
    body["supplier_id"] = json!(UNKNOWN);
    let reply = send(node.router.clone(), Method::POST, RECEIPTS, body.clone()).await;
    assert_eq!(
        reply.1["error"]["details"],
        json!({"entity": "SUPPLIER", "id": UNKNOWN})
    );
    body["occurred_at"] = json!(NOW - 259_200_001);
    let reply = send(node.router.clone(), Method::POST, RECEIPTS, body).await;
    assert_eq!(reply.1["error"]["code"], "CAPTURE_TOO_OLD");
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn interleaved_items_keep_separate_balances_and_serials() {
    let node = node(true).await;
    let (flour, supplier) = seed(&node).await;
    let mut sugar = item_body(11);
    sugar["code"] = json!("SUGAR");
    let reply = send(node.router.clone(), Method::POST, "/api/v1/items", sugar).await;
    assert_eq!(reply.0, 200);
    let sugar = reply.1["data"]["item"]["item_id"].as_str().unwrap();
    let mut command = receipt(&flour, &supplier, 3);
    command["lines"][1]["item_id"] = json!(sugar);
    let reply = send(node.router.clone(), Method::POST, RECEIPTS, command).await;
    assert_eq!(reply.0, 200, "{reply:?}");
    for (index, lot) in [
        "RAW-FLOUR-20261006-001",
        "RAW-SUGAR-20261006-001",
        "RAW-FLOUR-20261006-002",
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(reply.1["data"]["receipt"]["lines"][index]["lot_id"], lot);
    }
    let mut first = waste(&flour);
    first["lot_id"] = json!("RAW-FLOUR-20261006-001");
    first["input"]["qty"] = json!("6");
    first["acknowledged"] = json!(["STOCK_SHORTFALL"]);
    let command = json!({"command_id": master_command_id(4), "lines": [first, waste(sugar), waste(&flour)], "occurred_at": NOW, "clock_synced_at": NOW});
    let reply = send(node.router.clone(), Method::POST, WASTE, command).await;
    assert_eq!(reply.0, 200, "{reply:?}");
    for (index, book) in ["10", "5", "4"].into_iter().enumerate() {
        assert_eq!(
            reply.1["data"]["waste_record"]["lines"][index]["item_book_qty"],
            book
        );
    }
    assert_eq!(
        reply.1["data"]["waste_record"]["lines"][0]["lot_book_qty"],
        "5"
    );
    assert_eq!(
        reply.1["data"]["waste_record"]["lines"][2]["alloc"],
        json!([{"lot_id": "RAW-FLOUR-20261006-002", "qty":"1", "source": "FIFO"}])
    );
    node.storage.writer_handle.shutdown().await.unwrap();
}

async fn saved_command(node: &Node, command: String) -> (String, String) {
    node.storage
        .readers
        .call(move |conn| -> Result<_, StorageError> {
            conn.query_row(
                "SELECT request, response FROM processed_commands WHERE command_id = ?1",
                [command],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| StorageError::sqlite("读取幂等文本", error))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn normalized_requests_and_saved_responses_keep_their_exact_text() {
    let node = node(true).await;
    let (item, supplier) = seed(&node).await;
    let mut command = receipt(&item, &supplier, 2);
    command["occurred_at"] = json!(NOW + 60_000);
    command["lines"][1]["expires_on"] = json!("2026-10-15");
    let received = send(node.router.clone(), Method::POST, RECEIPTS, command.clone()).await;
    assert_eq!(received.0, 200, "{received:?}");
    let saved = saved_command(&node, master_command_id(3)).await;
    let request = format!(
        r#"{{"lines":[{{"expires_on":"2026-10-20","input":{{"base_qty_per_unit":"1","qty":"5","unit_code":"g"}},"item_id":"{item}","line_cost_cents":0,"produced_on":"2026-10-01"}},{{"expires_on":"2026-10-15","input":{{"base_qty_per_unit":"1","qty":"5","unit_code":"g"}},"item_id":"{item}","line_cost_cents":0,"produced_on":"2026-10-01"}}],"occurred_at":{},"supplier_id":"{supplier}"}}"#,
        NOW + 60_000
    );
    assert_eq!(saved.0, request);
    let warnings = format!(
        r#"[{{"code":"EXPIRES_BEFORE_OLDER_STOCK","message":"new lot expires before stock used first","details":{{"item_id":"{item}","line":1,"lot_id":"RAW-FLOUR-20261006-002"}}}}]"#
    );
    assert_eq!(
        saved.1,
        format!(
            r#"{{"success":true,"data":{},"warnings":{warnings},"error":null}}"#,
            received.1["data"]
        )
    );
    command["clock_synced_at"] = json!(i64::MIN);
    assert_eq!(
        send(node.router.clone(), Method::POST, RECEIPTS, command).await,
        received
    );
    assert_eq!(saved_command(&node, master_command_id(3)).await, saved);

    let mut line = waste(&item);
    line["acknowledged"] = json!([]);
    let mut command = json!({"command_id": master_command_id(4), "lines": [line], "occurred_at": NOW, "clock_synced_at": NOW});
    let logged = send(node.router.clone(), Method::POST, WASTE, command.clone()).await;
    assert_eq!(logged.0, 200, "{logged:?}");
    let saved = saved_command(&node, master_command_id(4)).await;
    assert_eq!(
        saved.0,
        format!(
            r#"{{"lines":[{{"input":{{"base_qty_per_unit":"1","qty":"1","unit_code":"g"}},"item_id":"{item}","reason_code":"EXPIRED"}}],"occurred_at":{NOW}}}"#
        )
    );
    assert_eq!(
        saved.1,
        format!(
            r#"{{"success":true,"data":{},"warnings":[],"error":null}}"#,
            logged.1["data"]
        )
    );
    command["lines"][0]
        .as_object_mut()
        .unwrap()
        .remove("acknowledged");
    command["clock_synced_at"] = json!(i64::MIN);
    assert_eq!(
        send(node.router.clone(), Method::POST, WASTE, command).await,
        logged
    );
    assert_eq!(saved_command(&node, master_command_id(4)).await, saved);
    node.storage.writer_handle.shutdown().await.unwrap();
}
