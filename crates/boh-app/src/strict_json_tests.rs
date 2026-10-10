//! HTTP regression tests for shared domain JSON contracts.

use super::*;

fn assert_validation_error((status, body): (u16, Value)) {
    assert_eq!(status, 400, "{body}");
    assert_eq!(
        body,
        json!({
            "success": false, "data": null, "warnings": [],
            "error": { "code": "VALIDATION_FAILED", "message": "invalid request", "details": {} }
        })
    );
}

// Check both rejection without a receipt and rejection before replaying a
// saved response. Correcting a rejected request can keep its command_id.
async fn reject_then_accept(
    node: &Node,
    method: Method,
    path: &str,
    good: Value,
    bad: &[Value],
) -> Value {
    let before = counts(node).await;
    for body in bad {
        assert_validation_error(
            send(node.router.clone(), method.clone(), path, body.clone()).await,
        );
        assert_eq!(counts(node).await, before);
    }
    let original = send(node.router.clone(), method.clone(), path, good.clone()).await;
    assert_eq!(original.0, 200, "{original:?}");
    let after = counts(node).await;
    assert_eq!(
        send(node.router.clone(), method.clone(), path, good).await,
        original
    );
    for body in bad {
        assert_validation_error(
            send(node.router.clone(), method.clone(), path, body.clone()).await,
        );
        assert_eq!(counts(node).await, after);
    }
    original.1
}

fn tagged_fields(good: &Value, fields: &[&str]) -> Vec<Value> {
    fields
        .iter()
        .map(|field| {
            let mut bad = good.clone();
            bad[field] = json!({ good[field].as_str().unwrap(): null });
            bad
        })
        .collect()
}

#[tokio::test]
async fn http_enum_fields_require_strings_before_idempotency() {
    let node = node(true).await;
    let equipment = create(1, "F1");
    let created = reject_then_accept(
        &node,
        Method::POST,
        "/api/v1/equipment",
        equipment.clone(),
        &tagged_fields(&equipment, &["equipment_type"]),
    )
    .await;
    let id = created["data"]["equipment"]["equipment_id"]
        .as_str()
        .unwrap();
    let update = json!({
        "command_id": master_command_id(3), "base_revision": 1,
        "name": "Oven", "equipment_type": "OVEN", "active": true
    });
    reject_then_accept(
        &node,
        Method::PUT,
        &format!("/api/v1/equipment/{id}"),
        update.clone(),
        &tagged_fields(&update, &["equipment_type"]),
    )
    .await;

    let item = item_body(1);
    let created = reject_then_accept(
        &node,
        Method::POST,
        "/api/v1/items",
        item.clone(),
        &tagged_fields(&item, &["base_unit", "category"]),
    )
    .await;
    let id = created["data"]["item"]["item_id"].as_str().unwrap();
    let update = json!({
        "command_id": master_command_id(4), "base_revision": 1,
        "name": "Finished flour", "category": "FINISHED", "units": [], "active": true
    });
    reject_then_accept(
        &node,
        Method::PUT,
        &format!("/api/v1/items/{id}"),
        update.clone(),
        &tagged_fields(&update, &["category"]),
    )
    .await;
    node.storage.writer_handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn http_shared_structs_require_objects_before_idempotency() {
    let node = node(true).await;
    let item = item_body(1);
    let mut bad_item = item.clone();
    bad_item["units"][0] = json!(["bag", 25000]);
    let created = reject_then_accept(&node, Method::POST, "/api/v1/items", item, &[bad_item]).await;
    let item_id = created["data"]["item"]["item_id"].as_str().unwrap();
    let recipe = recipe_body(item_id);
    let mut bad_recipe = recipe.clone();
    bad_recipe["lines"][0] = json!([item_id, 4]);
    let created = reject_then_accept(
        &node,
        Method::POST,
        "/api/v1/recipes",
        recipe,
        &[bad_recipe],
    )
    .await;
    let recipe_id = created["data"]["recipe"]["recipe_id"].as_str().unwrap();
    let version = json!({
        "command_id": master_command_id(3), "base_revision": 1,
        "output_qty_per_batch": 3, "lines": [{"item_id": item_id, "qty_per_batch": 6}]
    });
    let mut bad_version = version.clone();
    bad_version["lines"][0] = json!([item_id, 6]);
    reject_then_accept(
        &node,
        Method::POST,
        &format!("/api/v1/recipes/{recipe_id}/versions"),
        version,
        &[bad_version],
    )
    .await;

    let (status, supplier) = send(
        node.router.clone(), Method::POST, "/api/v1/suppliers",
        json!({"command_id": master_command_id(4), "code": "S1", "name": "Supplier", "active": true}),
    )
    .await;
    assert_eq!(status, 200, "{supplier}");
    let supplier_id = supplier["data"]["supplier"]["supplier_id"]
        .as_str()
        .unwrap();
    let receipt = json!({
        "command_id": master_command_id(5), "supplier_id": supplier_id,
        "captured_at": 1_791_248_400_000_i64, "sent_at": 1_791_248_400_000_i64,
        "lines": [{
            "item_id": item_id, "input": {"qty": 5, "unit_code": "g", "base_qty_per_unit": 1},
            "produced_on": "2026-10-01", "expires_on": "2026-10-20", "line_cost_cents": 1000
        }]
    });
    let mut bad_receipt = receipt.clone();
    bad_receipt["lines"][0]["input"] = json!([5, "g", 1]);
    reject_then_accept(
        &node,
        Method::POST,
        "/api/v1/receipts",
        receipt,
        &[bad_receipt],
    )
    .await;
    node.storage.writer_handle.shutdown().await.unwrap();
}
