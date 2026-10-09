//! 锁定测试：库存明细查询与批次查询。规则见 docs/domain.md「库存明细查询」「批次查询」「批次」「盘点」「投影表」「时间」，
//! AGENTS.md「HTTP 约定」「ID 与时间」，接口见 docs/interfaces.md。
//! 余量为 0 或为负的批次、账外缺口要由报损产生，随报损切片锁定。

mod spec_support;

use std::path::PathBuf;

use axum::Router;
use axum::http::Method;
use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use boh_storage::rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};
use spec_support::{
    Actor, JsonReply, MANAGER, STAFF, assert_error, assert_success, non_canonical_uuids, request,
    send,
};
use tempfile::TempDir;

const NOW: i64 = 1_791_248_400_000; // 2026-10-06 09:00 +08:00
const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;
const SENT: i64 = NOW + 7 * MINUTE;
const UNKNOWN_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8399";
const URI: &str = "/api/v1/inventory";

/// `expires_on` 在 Asia/Shanghai 的当日最后一毫秒。
const END_OF_10_15: i64 = 1_792_079_999_999;
const END_OF_10_20: i64 = 1_792_511_999_999;
const END_OF_10_25: i64 = 1_792_943_999_999;
const END_OF_10_30: i64 = 1_793_375_999_999;
/// 2026-10-07 04:00 +08:00：营业日 2026-10-07 的开始。
const AT_10_07_0400: i64 = NOW + 19 * HOUR;

struct Node {
    _dir: TempDir,
    db_path: PathBuf,
    clock: ManualClock,
    router: Router,
}

#[allow(clippy::unwrap_used)] // 测试夹具：临时目录或 Router 构造失败时测试无法开始，直接终止。
fn node() -> Node {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(UnixMillis(NOW));
    let router = spec_support::router(&db_path, clock.clock()).unwrap();
    Node {
        _dir: dir,
        db_path,
        clock,
        router,
    }
}

/// 第 `n` 个命令 ID（UUIDv7）。
fn cmd(n: u16) -> String {
    format!("01890a5d-ac96-774b-bcce-b30209c1{n:04x}")
}

/// 收货请求的一行：`qty` 克，到期日 `expires_on`，没有生产商批号。
fn rline(item_id: &str, qty: i64, expires_on: &str) -> Value {
    json!({
        "item_id": item_id,
        "input": { "qty": qty, "unit_code": "g", "base_qty_per_unit": 1 },
        "produced_on": "2026-10-01", "expires_on": expires_on, "line_cost_cents": 100,
    })
}

impl Node {
    #[allow(clippy::unwrap_used)] // 测试夹具：写入前置状态的请求失败时，后续步骤没有意义，直接终止测试。
    async fn write(&self, actor: &Actor, method: Method, uri: &str, body: Value) -> Value {
        let reply = send(
            &self.router,
            request(method, uri, Some(actor), Some(&body)).unwrap(),
        )
        .await
        .unwrap();
        assert_success(&reply).clone()
    }

    #[allow(clippy::unwrap_used)] // 同上。
    async fn item(&self, n: u16, code: &str) -> String {
        let data = self
            .write(
                &MANAGER,
                Method::POST,
                "/api/v1/items",
                json!({
                    "command_id": cmd(n), "code": code, "name": code, "base_unit": "g",
                    "category": "RAW", "units": [], "active": true,
                }),
            )
            .await;
        data["item"]["item_id"].as_str().unwrap().to_owned()
    }

    async fn deactivate_item(&self, n: u16, item_id: &str) {
        self.write(
            &MANAGER,
            Method::PUT,
            &format!("/api/v1/items/{item_id}"),
            json!({
                "command_id": cmd(n), "base_revision": 1, "name": "停用", "category": "RAW",
                "units": [], "active": false,
            }),
        )
        .await;
    }

    #[allow(clippy::unwrap_used)] // 同上。
    async fn supplier(&self, n: u16) -> String {
        let data = self
            .write(
                &MANAGER,
                Method::POST,
                "/api/v1/suppliers",
                json!({ "command_id": cmd(n), "code": "S1", "name": "S1", "active": true }),
            )
            .await;
        data["supplier"]["supplier_id"].as_str().unwrap().to_owned()
    }

    /// 收货：平板在 `SENT − lag` 录入、在 `SENT` 发送；返回各行的 `lot_id`。
    #[allow(clippy::unwrap_used)] // 同上。
    async fn receive(&self, n: u16, supplier: &str, lines: Vec<Value>, lag: i64) -> Vec<String> {
        let data = self
            .write(
                &STAFF,
                Method::POST,
                "/api/v1/receipts",
                json!({
                    "command_id": cmd(n), "supplier_id": supplier, "lines": lines,
                    "captured_at": SENT - lag, "sent_at": SENT,
                }),
            )
            .await;
        data["receipt"]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line["lot_id"].as_str().unwrap().to_owned())
            .collect()
    }

    #[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试。
    async fn query(&self, actor: Option<&Actor>, query: &str) -> JsonReply {
        let uri = if query.is_empty() {
            URI.to_owned()
        } else {
            format!("{URI}?{query}")
        };
        spec_support::get(&self.router, &uri, actor).await.unwrap()
    }

    /// 查询成功时的 `data`；断言 warnings 为空。
    async fn detail(&self, query: &str) -> Value {
        let reply = self.query(Some(&STAFF), query).await;
        let data = assert_success(&reply).clone();
        assert_eq!(reply.body["warnings"], json!([]));
        data
    }

    /// 批次查询；`lot_id` 原样放进路径（调用方负责百分号编码）。
    #[allow(clippy::unwrap_used)] // 测试夹具：响应体不是 JSON 已违反信封约定，直接终止测试。
    async fn lot(&self, actor: Option<&Actor>, lot_id: &str) -> JsonReply {
        spec_support::get(&self.router, &format!("/api/v1/lots/{lot_id}"), actor)
            .await
            .unwrap()
    }

    /// 全部表的内容，用于断言查询不写任何东西。
    #[allow(clippy::unwrap_used)] // 测试夹具：只读查询失败时无法比对状态，直接终止测试。
    fn state(&self) -> Vec<Vec<Vec<SqlValue>>> {
        let reader = spec_support::reader(&self.db_path).unwrap();
        [
            "SELECT * FROM store_events ORDER BY seq",
            "SELECT * FROM processed_commands ORDER BY command_id",
            "SELECT * FROM inventory_lots ORDER BY lot_id",
            "SELECT * FROM inventory_movements ORDER BY event_seq, movement_no",
            "SELECT * FROM inventory_unallocated ORDER BY item_id",
        ]
        .iter()
        .map(|sql| spec_support::rows(&reader, sql).unwrap())
        .collect()
    }
}

/// 库存明细中的一个批次行（不含 `item_id`）；没有生产商批号时不带该键。
fn lot_row(lot_id: &str, remaining_qty: i64, source_occurred_at: i64, expires_at: i64) -> Value {
    json!({
        "lot_id": lot_id, "origin": "RECEIPT", "remaining_qty": remaining_qty,
        "source_occurred_at": source_occurred_at, "expires_at": expires_at,
    })
}

/// 批次查询返回的批次行：库存明细中的批次行加上 `item_id`。
fn with_item(mut row: Value, item_id: &str) -> Value {
    row["item_id"] = json!(item_id);
    row
}

// ---------------------------------------------------------------------------------------------
// 库存明细查询
// ---------------------------------------------------------------------------------------------

// 库存明细查询：data 为 {business_date, items}。
// - items 每个物料一项，含停用的和没有库存的，按 code 的字节序升序（"BUTTER" 在 "B_2" 之前：'U' < '_'）；
//   每项 {item_id, on_hand_qty, unallocated_qty, lots}，没有库存时 on_hand_qty、unallocated_qty 为 0，lots 为空数组。
// - lots 按 FIFO（批次日期、流水号）排列，不按到期日或入账先后：补录的 10-04 批次 D 排最前，之后是 10-06 的 A、B、C。
//   每行 {lot_id, origin, remaining_qty, source_occurred_at, expires_at, manufacturer_lot_no?}，不含 item_id；
//   source_occurred_at 是收货的 occurred_at；manufacturer_lot_no 出现时照原样返回，没有时省略该键。
// - 带 item_id 时 items 只含该物料一项（停用、没有库存的也照常返回）。
#[tokio::test]
async fn detail_lists_every_item_with_lots_in_fifo_order() {
    let node = node();
    let flour = node.item(1, "FLOUR").await;
    let butter = node.item(2, "BUTTER").await;
    let sugar = node.item(3, "SUGAR").await;
    let b2 = node.item(4, "B_2").await;
    let egg = node.item(5, "EGG").await;
    node.deactivate_item(6, &egg).await;
    let supplier = node.supplier(7).await;
    let mut b_line = rline(&flour, 10, "2026-10-25");
    b_line["manufacturer_lot_no"] = json!("批号 L-1");
    let first = node
        .receive(
            8,
            &supplier,
            vec![
                rline(&flour, 5, "2026-10-20"),
                b_line,
                rline(&butter, 3000, "2026-10-20"),
            ],
            10 * MINUTE,
        )
        .await;
    let c = node
        .receive(9, &supplier, vec![rline(&flour, 20, "2026-10-15")], 0)
        .await
        .remove(0);
    let d = node
        .receive(
            10,
            &supplier,
            vec![rline(&flour, 7, "2026-10-30")],
            48 * HOUR,
        )
        .await
        .remove(0);
    let s = node
        .receive(11, &supplier, vec![rline(&sugar, 10, "2026-10-20")], 0)
        .await
        .remove(0);
    assert_eq!(
        [&first[0], &first[1], &first[2], &c, &d, &s],
        [
            "RAW-FLOUR-20261006-001",
            "RAW-FLOUR-20261006-002",
            "RAW-BUTTER-20261006-001",
            "RAW-FLOUR-20261006-003",
            "RAW-FLOUR-20261004-001",
            "RAW-SUGAR-20261006-001",
        ]
    );

    let data = node.detail("").await;

    let mut b_row = lot_row(&first[1], 10, NOW - 10 * MINUTE, END_OF_10_25);
    b_row["manufacturer_lot_no"] = json!("批号 L-1");
    assert_eq!(
        data,
        json!({
            "business_date": "2026-10-06",
            "items": [
                {
                    "item_id": butter, "on_hand_qty": 3000, "unallocated_qty": 0,
                    "lots": [lot_row(&first[2], 3000, NOW - 10 * MINUTE, END_OF_10_20)],
                },
                { "item_id": b2, "on_hand_qty": 0, "unallocated_qty": 0, "lots": [] },
                { "item_id": egg, "on_hand_qty": 0, "unallocated_qty": 0, "lots": [] },
                {
                    "item_id": flour, "on_hand_qty": 42, "unallocated_qty": 0,
                    "lots": [
                        lot_row(&d, 7, NOW - 48 * HOUR, END_OF_10_30),
                        lot_row(&first[0], 5, NOW - 10 * MINUTE, END_OF_10_20),
                        b_row,
                        lot_row(&c, 20, NOW, END_OF_10_15),
                    ],
                },
                {
                    "item_id": sugar, "on_hand_qty": 10, "unallocated_qty": 0,
                    "lots": [lot_row(&s, 10, NOW, END_OF_10_20)],
                },
            ],
        })
    );
    for (id, index) in [(&flour, 3), (&egg, 2), (&b2, 1)] {
        assert_eq!(
            node.detail(&format!("item_id={id}")).await,
            json!({ "business_date": "2026-10-06", "items": [data["items"][index].clone()] }),
            "{id}"
        );
    }
}

// 库存明细查询：item_id 合法但不存在（含其他实体的 ID）时 items 为空数组，business_date 照常返回。
#[tokio::test]
async fn unknown_item_id_returns_no_items() {
    let node = node();
    let flour = node.item(1, "FLOUR").await;
    let supplier = node.supplier(2).await;
    node.receive(3, &supplier, vec![rline(&flour, 5, "2026-10-20")], 0)
        .await;

    for id in [UNKNOWN_ID, supplier.as_str()] {
        assert_eq!(
            node.detail(&format!("item_id={id}")).await,
            json!({ "business_date": "2026-10-06", "items": [] }),
            "{id}"
        );
    }
}

// 库存明细查询的 business_date 由门店节点的当前时间按 Asia/Shanghai 与日切 04:00 计算（盘点草稿用它核对营业日），
// 不取最近一个事件或批次的日期：当地 10-07 03:59:59.999 为 2026-10-06，04:00 为 2026-10-07。
// - 没有物料时 items 为空数组。
// - 10-06 收货之后不再写入，时钟推进到 10-07 04:00 时为 2026-10-07，回拨到 03:59:59.999 时又为 2026-10-06；
//   带不带 item_id 都一样，库存内容不变。
#[tokio::test]
async fn business_date_follows_the_node_clock() {
    let node = node();

    node.clock.set(UnixMillis(AT_10_07_0400 - 1));
    assert_eq!(
        node.detail("").await,
        json!({ "business_date": "2026-10-06", "items": [] })
    );
    node.clock.set(UnixMillis(AT_10_07_0400));
    assert_eq!(
        node.detail("").await,
        json!({ "business_date": "2026-10-07", "items": [] })
    );

    node.clock.set(UnixMillis(NOW));
    let flour = node.item(1, "FLOUR").await;
    let supplier = node.supplier(2).await;
    let lot = node
        .receive(3, &supplier, vec![rline(&flour, 5, "2026-10-20")], 0)
        .await
        .remove(0);
    let items = json!([{
        "item_id": flour, "on_hand_qty": 5, "unallocated_qty": 0,
        "lots": [lot_row(&lot, 5, NOW, END_OF_10_20)],
    }]);
    for (now, business_date) in [
        (NOW, "2026-10-06"),
        (AT_10_07_0400, "2026-10-07"),
        (AT_10_07_0400 - 1, "2026-10-06"),
    ] {
        node.clock.set(UnixMillis(now));
        for query in [String::new(), format!("item_id={flour}")] {
            assert_eq!(
                node.detail(&query).await,
                json!({ "business_date": business_date, "items": items }),
                "{now} {query}"
            );
        }
    }
}

// 库存明细查询与 AGENTS「HTTP 约定」「ID 与时间」：item_id 为空、不是规范形式的 UUIDv7（含批次号），未知参数（含其他查询接口的
// business_date）、重复参数，一律 400 VALIDATION_FAILED。
#[tokio::test]
async fn invalid_detail_queries_are_validation_failed() {
    let node = node();
    let flour = node.item(1, "FLOUR").await;

    for query in [
        "item_id=".to_owned(),
        "item_id=not-a-uuid".into(),
        "item_id=RAW-FLOUR-20261006-001".into(),
        "item_id=01890a5d-ac96-474b-bcce-b302099a8399".into(),
        "foo=1".into(),
        "business_date=2026-10-06".into(),
        format!("item_id={flour}&foo=1"),
        format!("item_id={flour}&item_id={flour}"),
        format!("item_id={flour}&item_id={UNKNOWN_ID}"),
    ]
    .into_iter()
    .chain(non_canonical_uuids(&flour).map(|text| {
        let text = text.replace('{', "%7B").replace('}', "%7D");
        format!("item_id={text}")
    })) {
        let reply = node.query(Some(&STAFF), &query).await;
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }
}

// domain「员工认证」开发桩与 AGENTS「HTTP 约定」：查询允许已认证员工（STAFF、MANAGER）；缺少身份时 401 UNAUTHENTICATED，
// 身份检查先于查询参数。查询走读连接，不写任何内容。只有 GET，其他方法 405 METHOD_NOT_ALLOWED。
#[tokio::test]
async fn detail_requires_identity_and_does_not_write() {
    let node = node();
    let flour = node.item(1, "FLOUR").await;
    let supplier = node.supplier(2).await;
    node.receive(3, &supplier, vec![rline(&flour, 5, "2026-10-20")], 0)
        .await;
    let before = node.state();

    for query in ["", "item_id=not-a-uuid"] {
        let reply = node.query(None, query).await;
        assert_error(&reply, 401, "UNAUTHENTICATED");
    }
    for actor in [&STAFF, &MANAGER] {
        let reply = node.query(Some(actor), "").await;
        assert_success(&reply);
    }
    for method in [Method::POST, Method::PUT, Method::DELETE] {
        let reply = send(
            &node.router,
            request(method.clone(), URI, Some(&MANAGER), None).unwrap(),
        )
        .await
        .unwrap();
        assert_error(&reply, 405, "METHOD_NOT_ALLOWED");
    }

    assert_eq!(node.state(), before);
}

// ---------------------------------------------------------------------------------------------
// 批次查询
// ---------------------------------------------------------------------------------------------

// 批次查询：data 为 {lot: 批次行}，批次行 {lot_id, item_id, origin, remaining_qty, source_occurred_at, expires_at,
// manufacturer_lot_no?}，与库存明细中的批次行相同，另加 item_id；没有生产商批号时省略该键。物料停用后照常查到。
#[tokio::test]
async fn lot_lookup_returns_the_lot_row() {
    let node = node();
    let flour = node.item(1, "FLOUR").await;
    let supplier = node.supplier(2).await;
    let mut b_line = rline(&flour, 10, "2026-10-25");
    b_line["manufacturer_lot_no"] = json!("批号 \"L-1\"");
    let lots = node
        .receive(
            3,
            &supplier,
            vec![rline(&flour, 5, "2026-10-20"), b_line],
            10 * MINUTE,
        )
        .await;
    node.deactivate_item(4, &flour).await;
    let mut b_row = with_item(
        lot_row(&lots[1], 10, NOW - 10 * MINUTE, END_OF_10_25),
        &flour,
    );
    b_row["manufacturer_lot_no"] = json!("批号 \"L-1\"");

    for (lot_id, row) in [
        (
            &lots[0],
            with_item(
                lot_row(&lots[0], 5, NOW - 10 * MINUTE, END_OF_10_20),
                &flour,
            ),
        ),
        (&lots[1], b_row),
    ] {
        let reply = node.lot(Some(&STAFF), lot_id).await;
        assert_eq!(assert_success(&reply), &json!({ "lot": row }), "{lot_id}");
        assert_eq!(reply.body["warnings"], json!([]));
    }
}

// 批次查询：路径中的 lot_id 不符合批次号格式时 400 VALIDATION_FAILED——类型不是 RAW / SEMI / FINISHED（区分大小写），
// 编码为空或含 A–Z、0–9、_ 以外的字符（小写、-、空格、中文），日期不是 8 位真实日期，流水号不是 001～999 的三位数，
// 多余或缺少的段，首尾空白，UUID；2027-02-29 不是真实日期。格式合法但不存在时 404 REFERENCE_NOT_FOUND，details 为
// {entity: "LOT", id}：不存在的流水号或日期（含闰日 2028-02-29）、同号但类型不同（按完整批次号查，不按物料编码、日期、流水号拼凑）。
#[tokio::test]
async fn lot_lookup_rejects_malformed_and_unknown_lot_numbers() {
    let node = node();
    let flour = node.item(1, "FLOUR").await;
    let supplier = node.supplier(2).await;
    let lots = node
        .receive(3, &supplier, vec![rline(&flour, 5, "2026-10-20")], 0)
        .await;
    assert_eq!(lots, ["RAW-FLOUR-20261006-001"]);
    let before = node.state();

    for path in [
        "raw-FLOUR-20261006-001",
        "Raw-FLOUR-20261006-001",
        "PKG-FLOUR-20261006-001",
        "RAW-flour-20261006-001",
        "RAW--20261006-001",
        "RAW-FL-OUR-20261006-001",
        "RAW-FL%20OUR-20261006-001",
        "RAW-%E9%9D%A2%E7%B2%89-20261006-001",
        "RAW-FLOUR-20260230-001",
        "RAW-FLOUR-20270229-001",
        "RAW-FLOUR-20261306-001",
        "RAW-FLOUR-2026100A-001",
        "RAW-FLOUR-2026106-001",
        "RAW-FLOUR-2026-10-06-001",
        "RAW-FLOUR-20261006-000",
        "RAW-FLOUR-20261006-01",
        "RAW-FLOUR-20261006-0001",
        "RAW-FLOUR-20261006-1",
        "RAW-FLOUR-20261006",
        "FLOUR-20261006-001",
        "RAW-FLOUR-20261006-001-1",
        "%20RAW-FLOUR-20261006-001",
        "RAW-FLOUR-20261006-001%20",
        "01890a5d-ac96-774b-bcce-b302099a8399",
    ] {
        let reply = node.lot(Some(&STAFF), path).await;
        assert_eq!(reply.status.as_u16(), 400, "{path}: {}", reply.body);
        assert_error(&reply, 400, "VALIDATION_FAILED");
    }

    for lot_id in [
        "RAW-FLOUR-20261006-002",
        "RAW-FLOUR-20261005-001",
        "SEMI-FLOUR-20261006-001",
        "FINISHED-NOPE-99991231-999",
        "RAW-FLOUR-20280229-001",
    ] {
        let reply = node.lot(Some(&STAFF), lot_id).await;
        assert_eq!(
            assert_error(&reply, 404, "REFERENCE_NOT_FOUND"),
            &json!({ "entity": "LOT", "id": lot_id }),
            "{lot_id}"
        );
    }

    assert_eq!(node.state(), before);
}

// domain「物料」编码与「批次」「批次查询」：物料编码可以含数字、下划线，也可以只有一个字符（A_1、_、0），批次号的编码段原样取它。
// 收货生成的批次号在库存明细（物料按 code 字节序："0" < "A_1" < "_"）和批次查询中原样返回；
// 这些编码格式合法但不存在的批次号 404 REFERENCE_NOT_FOUND，不是 400。
#[tokio::test]
async fn lot_numbers_carry_digit_and_underscore_codes() {
    let node = node();
    let a1 = node.item(1, "A_1").await;
    let underscore = node.item(2, "_").await;
    let zero = node.item(3, "0").await;
    let supplier = node.supplier(4).await;
    let lots = node
        .receive(
            5,
            &supplier,
            vec![
                rline(&a1, 1, "2026-10-20"),
                rline(&underscore, 2, "2026-10-20"),
                rline(&zero, 3, "2026-10-20"),
            ],
            0,
        )
        .await;
    assert_eq!(
        lots,
        [
            "RAW-A_1-20261006-001",
            "RAW-_-20261006-001",
            "RAW-0-20261006-001"
        ]
    );

    let item = |item_id: &str, lot_id: &str, qty: i64| {
        json!({
            "item_id": item_id, "on_hand_qty": qty, "unallocated_qty": 0,
            "lots": [lot_row(lot_id, qty, NOW, END_OF_10_20)],
        })
    };
    assert_eq!(
        node.detail("").await,
        json!({
            "business_date": "2026-10-06",
            "items": [
                item(&zero, &lots[2], 3),
                item(&a1, &lots[0], 1),
                item(&underscore, &lots[1], 2),
            ],
        })
    );
    for (item_id, lot_id, qty) in [
        (&a1, &lots[0], 1),
        (&underscore, &lots[1], 2),
        (&zero, &lots[2], 3),
    ] {
        assert_eq!(
            node.detail(&format!("item_id={item_id}")).await,
            json!({ "business_date": "2026-10-06", "items": [item(item_id, lot_id, qty)] }),
            "{lot_id}"
        );
        let reply = node.lot(Some(&STAFF), lot_id).await;
        assert_eq!(
            assert_success(&reply),
            &json!({ "lot": with_item(lot_row(lot_id, qty, NOW, END_OF_10_20), item_id) }),
            "{lot_id}"
        );
    }

    for lot_id in [
        "RAW-A_1-20261006-002",
        "RAW-_-20261005-001",
        "RAW-0-20261006-999",
        "SEMI-0-20261006-001",
    ] {
        let reply = node.lot(Some(&STAFF), lot_id).await;
        assert_eq!(
            assert_error(&reply, 404, "REFERENCE_NOT_FOUND"),
            &json!({ "entity": "LOT", "id": lot_id }),
            "{lot_id}"
        );
    }
}

// domain「员工认证」开发桩与 AGENTS「HTTP 约定」：批次查询允许已认证员工（STAFF、MANAGER）；缺少身份时 401 UNAUTHENTICATED，
// 身份检查先于路径参数校验和存在性检查。查询不写任何内容。只有 GET，其他方法 405 METHOD_NOT_ALLOWED。
#[tokio::test]
async fn lot_lookup_requires_identity_and_does_not_write() {
    let node = node();
    let flour = node.item(1, "FLOUR").await;
    let supplier = node.supplier(2).await;
    let lot = node
        .receive(3, &supplier, vec![rline(&flour, 5, "2026-10-20")], 0)
        .await
        .remove(0);
    let before = node.state();

    for path in [lot.as_str(), "raw-flour", "RAW-FLOUR-20261006-002"] {
        let reply = node.lot(None, path).await;
        assert_error(&reply, 401, "UNAUTHENTICATED");
    }
    for actor in [&STAFF, &MANAGER] {
        let reply = node.lot(Some(actor), &lot).await;
        assert_success(&reply);
    }
    for method in [Method::POST, Method::PUT, Method::DELETE] {
        let reply = send(
            &node.router,
            request(
                method.clone(),
                &format!("/api/v1/lots/{lot}"),
                Some(&MANAGER),
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
        assert_error(&reply, 405, "METHOD_NOT_ALLOWED");
    }

    assert_eq!(node.state(), before);
}
