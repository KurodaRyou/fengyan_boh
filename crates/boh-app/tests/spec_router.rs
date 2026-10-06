//! 锁定测试：`test_router` 的测试节点初始化约定（docs/interfaces.md「Router 测试入口」）。
//! 这是测试入口的约定，不是业务命令的行为：store_meta 只在测试节点首次打开时写入一次。

mod spec_support;

use std::error::Error;
use std::path::Path;
use std::time::Duration;

use boh_domain::UnixMillis;
use boh_storage::clock::ManualClock;
use boh_storage::rusqlite::params;
use spec_support::TEST_STORE_ID;

const NOW: UnixMillis = UnixMillis(1_791_248_400_000); // 2026-10-06 09:00 +08:00
const OTHER_STORE_ID: &str = "01890a5d-ac96-774b-bcce-b302099a8051";

fn store_meta(db_path: &Path) -> Result<Vec<(String, i64)>, Box<dyn Error>> {
    let reader = spec_support::reader(db_path)?;
    let mut statement = reader.prepare("SELECT store_id, created_at FROM store_meta")?;
    let rows = statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

// store_meta 为空时，test_router 用固定 store_id 写入一行，created_at 取 clock.now()；
// 重新打开同一个库不再写入，created_at 保持首次的值；构造过程不写事件或 processed_commands。
#[tokio::test]
async fn test_router_initializes_store_meta_once() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let clock = ManualClock::new(NOW);

    drop(spec_support::router(&db_path, clock.clock()).unwrap());
    assert_eq!(
        store_meta(&db_path).unwrap(),
        [(TEST_STORE_ID.to_owned(), NOW.0)]
    );

    clock.advance(Duration::from_secs(3600));
    drop(spec_support::router(&db_path, clock.clock()).unwrap());
    assert_eq!(
        store_meta(&db_path).unwrap(),
        [(TEST_STORE_ID.to_owned(), NOW.0)]
    );

    let reader = spec_support::reader(&db_path).unwrap();
    assert_eq!(spec_support::count(&reader, "store_events").unwrap(), 0);
    assert_eq!(
        spec_support::count(&reader, "processed_commands").unwrap(),
        0
    );
}

// store_meta 已存在且 store_id 不同：test_router 返回 Err，不改动 store_meta。
#[tokio::test]
async fn test_router_rejects_a_database_of_another_store() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("boh.db");
    let mut writer = spec_support::writer(&db_path).unwrap();
    spec_support::migrate(&mut writer).unwrap();
    writer
        .execute(
            "INSERT INTO store_meta (id, store_id, created_at) VALUES (1, ?1, ?2)",
            params![OTHER_STORE_ID, NOW.0],
        )
        .unwrap();
    drop(writer);

    assert!(spec_support::router(&db_path, ManualClock::new(NOW).clock()).is_err());

    assert_eq!(
        store_meta(&db_path).unwrap(),
        [(OTHER_STORE_ID.to_owned(), NOW.0)]
    );
}
