//! 锁定测试：HTTP 响应信封。规则见 AGENTS.md「HTTP 约定」，接口见 docs/interfaces.md。

mod spec_support;

use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use serde_json::{Value, json};

const NOW: UnixMillis = UnixMillis(1_791_248_400_000); // 2026-10-06 09:00 +08:00

// 「所有响应都是同一信封」：成功时 data 是对象，warnings 总是存在且没有警告时为空数组，error 为 null。
#[tokio::test]
async fn health_uses_the_success_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let clock = ManualClock::new(NOW);
    let router = boh_app::test_router(&dir.path().join("boh.db"), clock.clock()).unwrap();

    let reply = spec_support::get(router, "/health").await.unwrap();

    assert_eq!(reply.status, 200);
    assert_eq!(reply.content_type.as_deref(), Some("application/json"));
    let envelope = reply
        .body
        .as_object()
        .expect("envelope must be a JSON object");
    let mut keys: Vec<&str> = envelope.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["data", "error", "success", "warnings"]);
    assert_eq!(envelope["success"], json!(true));
    assert!(envelope["data"].is_object(), "data = {}", envelope["data"]);
    assert_eq!(envelope["warnings"], json!([]));
    assert_eq!(envelope["error"], Value::Null);
}
