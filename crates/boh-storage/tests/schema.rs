//! 锁定迁移 001、002、003、004、005、006、007 的表结构；每项测试注明对应的规则（AGENTS.md / docs/domain.md）或迁移文件。

use std::fmt::Debug;

use boh_storage::rusqlite::{self, Connection, TransactionBehavior, ffi, params};
use boh_storage::testing::{migrate, open_reader, open_writer};
use boh_storage::{LATEST_SCHEMA_VERSION, StorageError, schema_version};
use tempfile::TempDir;

const MIGRATION_001: &str = include_str!("../../../migrations/001_initial_schema.sql");
const MIGRATION_001_FNV1A_64: u64 = 0xfdd0_ce57_922a_6b5f;
const MIGRATION_002: &str = include_str!("../../../migrations/002_equipment.sql");
const MIGRATION_002_FNV1A_64: u64 = 0x5b5d_70a4_7c72_9418;
const MIGRATION_003: &str = include_str!("../../../migrations/003_temperature_readings.sql");
const MIGRATION_003_FNV1A_64: u64 = 0x68f1_2381_a56a_04c3;
const MIGRATION_004: &str = include_str!("../../../migrations/004_store_events_recorded_at.sql");
const MIGRATION_004_FNV1A_64: u64 = 0x401b_6d6e_ba06_70d2;
const MIGRATION_005: &str = include_str!("../../../migrations/005_master_data_inventory.sql");
const MIGRATION_005_FNV1A_64: u64 = 0x4191_3449_6be8_86c9;
const MIGRATION_006: &str =
    include_str!("../../../migrations/006_inventory_lots_manufacturer_lot_no.sql");
const MIGRATION_006_FNV1A_64: u64 = 0x3773_286d_a4b7_9106;
const MIGRATION_007: &str = include_str!("../../../migrations/007_lot_numbers.sql");
const MIGRATION_007_FNV1A_64: u64 = 0x0676_d9dd_8cea_52c1;
const STORE: &str = "01890a5d-ac96-774b-bcce-b302099a8050";
const ACTOR: &str = "01890a5d-ac96-774b-bcce-b302099a8051";
const CMD: &str = "01890a5d-ac96-774b-bcce-b302099a8057";
const EVT: &str = "01890a5d-ac96-774b-bcce-b302099a8058";
const EVT2: &str = "01890a5d-ac96-774b-bcce-b302099a8059";
const EVT3: &str = "01890a5d-ac96-774b-bcce-b302099a805a";
const TS: i64 = 1_791_158_400_000;
const EQUIPMENT_TYPES: [&str; 7] = [
    "FRIDGE",
    "FREEZER",
    "BLAST_FREEZER",
    "OVEN",
    "PROOFER",
    "MIXER",
    "OTHER",
];
const INVALID_IDS: [&str; 4] = [
    "01890a5d-ac96-474b-bcce-b302099a8057",  // v4
    "01890A5D-AC96-774B-BCCE-B302099A8057",  // 大写
    "01890a5d-ac96-774b-bcce-b302099a805",   // 太短
    "01890a5d-ac96-774b-bcce-b302099a80570", // 太长
];
const INSERT_EVENT: &str =
    "INSERT INTO store_events (id, event_type, schema_version, aggregate_type, aggregate_id,
         aggregate_version, command_id, actor_id, device_id, business_date,
         occurred_at, recorded_at, payload)
     VALUES (?1, 'WASTE_LOGGED', ?2, ?3, ?4, ?5, ?6, ?7, 'device-1', ?8, ?9, ?10, ?11)";

struct Event<'a> {
    id: &'a str,
    schema_version: i64,
    aggregate_type: &'a str,
    aggregate_id: &'a str,
    aggregate_version: i64,
    command_id: &'a str,
    actor_id: &'a str,
    business_date: &'a str,
    occurred_at: i64,
    recorded_at: i64,
    payload: &'a str,
}

impl<'a> Event<'a> {
    fn new(id: &'a str) -> Self {
        Self {
            id,
            schema_version: 1,
            aggregate_type: "WASTE_RECORD",
            aggregate_id: id,
            aggregate_version: 1,
            command_id: CMD,
            actor_id: ACTOR,
            business_date: "2026-10-05",
            occurred_at: TS - 1000,
            recorded_at: TS,
            payload: r#"{"lines":[]}"#,
        }
    }

    fn insert(&self, conn: &Connection) -> rusqlite::Result<usize> {
        self.execute_insert(conn, INSERT_EVENT)
    }

    fn execute_insert(&self, conn: &Connection, sql: &str) -> rusqlite::Result<usize> {
        conn.execute(
            sql,
            params![
                self.id,
                self.schema_version,
                self.aggregate_type,
                self.aggregate_id,
                self.aggregate_version,
                self.command_id,
                self.actor_id,
                self.business_date,
                self.occurred_at,
                self.recorded_at,
                self.payload,
            ],
        )
    }
}

#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
#[allow(clippy::unwrap_used)] // 测试夹具：建库或迁移失败时测试无法开始，直接终止。
fn fresh_db() -> (TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = open_writer(&dir.path().join("boh.db")).unwrap();
    migrate(&mut conn).unwrap();
    (dir, conn)
}

fn insert_meta(conn: &Connection, id: i64, store_id: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO store_meta (id, store_id, created_at) VALUES (?1, ?2, ?3)",
        params![id, store_id, TS],
    )
}

fn insert_command(conn: &Connection, command_id: &str, request: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO processed_commands (command_id, command_type, request, response, recorded_at)
         VALUES (?1, 'waste.log', ?2, '{\"warnings\":[]}', ?3)",
        params![command_id, request, TS],
    )
}

fn event_seqs(conn: &Connection) -> rusqlite::Result<Vec<i64>> {
    conn.prepare("SELECT seq FROM store_events ORDER BY seq")?
        .query_map([], |row| row.get(0))?
        .collect()
}

#[allow(clippy::expect_used)] // 断言辅助函数：SQL 未被拒绝本身就是断言失败，应直接终止测试。
fn assert_sqlite_error<T: Debug>(result: rusqlite::Result<T>, extended_code: i32) -> String {
    match result.expect_err("SQL must be rejected") {
        rusqlite::Error::SqliteFailure(error, message) => {
            assert_eq!(error.extended_code, extended_code, "{message:?}");
            message.unwrap_or_default()
        }
        error => panic!("expected SQLite error {extended_code}, got {error:?}"),
    }
}

fn assert_trigger_reject<T: Debug>(result: rusqlite::Result<T>, message: &str) {
    assert_eq!(
        assert_sqlite_error(result, ffi::SQLITE_CONSTRAINT_TRIGGER),
        message
    );
}

fn fnv1a_64(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

// AGENTS「迁移」不可修改：按 UTF-8 原始字节锁定 001 完整 SQL（包括注释）。
#[test]
fn migration_001_matches_frozen_checksum() {
    assert_eq!(fnv1a_64(MIGRATION_001), MIGRATION_001_FNV1A_64);
}

// AGENTS「迁移」不可修改：按 UTF-8 原始字节锁定 002 完整 SQL（包括注释）。
#[test]
fn migration_002_matches_frozen_checksum() {
    assert_eq!(fnv1a_64(MIGRATION_002), MIGRATION_002_FNV1A_64);
}

// AGENTS「迁移」不可修改：按 UTF-8 原始字节锁定 003 完整 SQL（包括注释）。
#[test]
fn migration_003_matches_frozen_checksum() {
    assert_eq!(fnv1a_64(MIGRATION_003), MIGRATION_003_FNV1A_64);
}

// AGENTS「迁移」不可修改：按 UTF-8 原始字节锁定 004 完整 SQL（包括注释）。
#[test]
fn migration_004_matches_frozen_checksum() {
    assert_eq!(fnv1a_64(MIGRATION_004), MIGRATION_004_FNV1A_64);
}

// AGENTS「迁移」不可修改：按 UTF-8 原始字节锁定 005 完整 SQL（包括注释）。
#[test]
fn migration_005_matches_frozen_checksum() {
    assert_eq!(fnv1a_64(MIGRATION_005), MIGRATION_005_FNV1A_64);
}

// AGENTS「迁移」不可修改：按 UTF-8 原始字节锁定 006 完整 SQL（包括注释）。
#[test]
fn migration_006_matches_frozen_checksum() {
    assert_eq!(fnv1a_64(MIGRATION_006), MIGRATION_006_FNV1A_64);
}

// AGENTS「迁移」不可修改：按 UTF-8 原始字节锁定 007 完整 SQL（包括注释）。
#[test]
fn migration_007_matches_frozen_checksum() {
    assert_eq!(fnv1a_64(MIGRATION_007), MIGRATION_007_FNV1A_64);
}

// AGENTS「迁移」：程序支持的版本就是已锁定迁移文件的个数。
#[test]
fn latest_schema_version_counts_locked_migrations() {
    assert_eq!(LATEST_SCHEMA_VERSION, 7);
}

// AGENTS「迁移」：重复执行迁移不改变版本，也不丢失已有数据。
#[test]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
fn migrate_reaches_latest_and_is_idempotent() {
    let (_dir, mut conn) = fresh_db();
    insert_meta(&conn, 1, STORE).unwrap();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
    migrate(&mut conn).unwrap();
    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
    let stored: (String, String, String) = conn
        .query_row(
            "SELECT store_meta.store_id, processed_commands.command_id, store_events.id FROM store_meta
             CROSS JOIN processed_commands CROSS JOIN store_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(stored, (STORE.into(), CMD.into(), EVT.into()));
    assert_eq!(event_seqs(&conn).unwrap(), [1]);
}

// AGENTS「迁移」：数据库版本高于程序支持的版本时拒绝，不能向下覆盖。
#[test]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
fn refuses_schema_newer_than_binary() {
    let (_dir, mut conn) = fresh_db();
    let future = LATEST_SCHEMA_VERSION + 1;
    conn.pragma_update(None, "user_version", future).unwrap();
    assert!(matches!(
        migrate(&mut conn),
        Err(StorageError::UnsupportedSchemaVersion { found, supported })
            if found == future && supported == LATEST_SCHEMA_VERSION
    ));
    assert_eq!(schema_version(&conn).unwrap(), future);
}

// AGENTS「SQLite」：写、读连接都设置规定的 PRAGMA；recursive_triggers 防止 REPLACE 绕过触发器。
#[test]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
fn writer_and_reader_apply_required_pragmas() {
    let (dir, writer) = fresh_db();
    let reader = open_reader(&dir.path().join("boh.db")).unwrap();
    for (conn, query_only) in [(&writer, 0), (&reader, 1)] {
        let journal: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        for (pragma, expected) in [
            ("synchronous", 2),
            ("foreign_keys", 1),
            ("busy_timeout", 5000),
            ("recursive_triggers", 1),
            ("query_only", query_only),
        ] {
            let actual: i64 = conn
                .pragma_query_value(None, pragma, |row| row.get(0))
                .unwrap();
            assert_eq!(actual, expected, "PRAGMA {pragma}");
        }
    }
}

// 001「门店身份（单行）」：唯一可用的 id 为 1。
#[test]
fn store_meta_accepts_only_id_one() {
    let (_dir, conn) = fresh_db();
    for id in [-1, 0, 2] {
        assert_sqlite_error(insert_meta(&conn, id, STORE), ffi::SQLITE_CONSTRAINT_CHECK);
    }
    insert_meta(&conn, 1, STORE).unwrap();
}

// 001「门店身份（单行）」：显式重复键和自动分配第二行都被拒绝。
#[test]
fn store_meta_rejects_a_second_row() {
    let (_dir, conn) = fresh_db();
    insert_meta(&conn, 1, STORE).unwrap();
    assert_sqlite_error(
        insert_meta(&conn, 1, ACTOR),
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
    );
    assert_sqlite_error(
        conn.execute(
            "INSERT INTO store_meta (store_id, created_at) VALUES (?1, ?2)",
            params![ACTOR, TS],
        ),
        ffi::SQLITE_CONSTRAINT_CHECK,
    );
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM store_meta", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

// AGENTS「只追加」：store_meta 禁止修改和删除，REPLACE / UPSERT 也被触发器拒绝。
#[test]
fn store_meta_is_immutable() {
    let (_dir, conn) = fresh_db();
    insert_meta(&conn, 1, STORE).unwrap();
    for sql in [
        "UPDATE store_meta SET created_at = created_at + 1",
        "DELETE FROM store_meta",
        "INSERT OR REPLACE INTO store_meta SELECT id, store_id, created_at + 1 FROM store_meta",
        "INSERT INTO store_meta SELECT id, store_id, created_at + 1 FROM store_meta WHERE id = 1
         ON CONFLICT(id) DO UPDATE SET created_at = excluded.created_at",
    ] {
        assert_trigger_reject(conn.execute(sql, []), "store_meta is immutable");
        let stored: (i64, String, i64) = conn
            .query_row("SELECT id, store_id, created_at FROM store_meta", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(stored, (1, STORE.into(), TS));
    }
}

// AGENTS「ID 与时间」：store_id 是 UUIDv7。
#[test]
fn store_id_rejects_v4_uppercase_and_wrong_length() {
    let (_dir, conn) = fresh_db();
    for invalid in INVALID_IDS {
        assert_sqlite_error(insert_meta(&conn, 1, invalid), ffi::SQLITE_CONSTRAINT_CHECK);
    }
    insert_meta(&conn, 1, STORE).unwrap();
}

// AGENTS「ID 与时间」：processed_commands.command_id 是 UUIDv7。
#[test]
fn command_id_rejects_v4_uppercase_and_wrong_length() {
    let (_dir, conn) = fresh_db();
    for invalid in INVALID_IDS {
        assert_sqlite_error(
            insert_command(&conn, invalid, "{}"),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }
    insert_command(&conn, CMD, "{}").unwrap();
}

// AGENTS「ID 与时间」：store_events.id / aggregate_id / actor_id 是 UUIDv7。
#[test]
fn event_uuid_columns_reject_v4_uppercase_and_wrong_length() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    for invalid in INVALID_IDS {
        for column in ["id", "aggregate_id", "actor_id"] {
            let mut event = Event::new(EVT);
            match column {
                "id" => event.id = invalid,
                "aggregate_id" => event.aggregate_id = invalid,
                "actor_id" => event.actor_id = invalid,
                _ => unreachable!(),
            }
            assert_sqlite_error(event.insert(&conn), ffi::SQLITE_CONSTRAINT_CHECK);
        }
    }
    Event::new(EVT).insert(&conn).unwrap();
}

// 001：事件的 command_id 通过延迟外键引用已经校验 UUIDv7 的命令键。
#[test]
fn event_command_id_cannot_reference_an_invalid_uuid() {
    let (_dir, mut conn) = fresh_db();
    for invalid in INVALID_IDS {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let mut event = Event::new(EVT);
        event.command_id = invalid;
        event.insert(&tx).unwrap();
        assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    }
    assert!(event_seqs(&conn).unwrap().is_empty());
}

// AGENTS「幂等」：规范化请求必须是 JSON 对象，数组、标量和非法 JSON 都被拒绝。
#[test]
fn request_must_be_a_json_object() {
    let (_dir, conn) = fresh_db();
    for request in ["[]", "[1]", "null", "true", "42", "\"text\""] {
        assert_sqlite_error(
            insert_command(&conn, CMD, request),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }
    assert_sqlite_error(insert_command(&conn, CMD, "{invalid"), ffi::SQLITE_ERROR);
    insert_command(&conn, CMD, r#"{"lines":[]}"#).unwrap();
}

// 001：payload 必须是 JSON 对象，数组、标量和非法 JSON 都被拒绝。
#[test]
fn payload_must_be_a_json_object() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    let mut event = Event::new(EVT);
    for payload in ["[]", "[1]", "null", "true", "42", "\"text\""] {
        event.payload = payload;
        assert_sqlite_error(event.insert(&conn), ffi::SQLITE_CONSTRAINT_CHECK);
    }
    event.payload = "{invalid";
    assert_sqlite_error(event.insert(&conn), ffi::SQLITE_ERROR);
    event.payload = "{}";
    event.insert(&conn).unwrap();
}

// 001：response 必须是有效 JSON。
#[test]
fn response_must_be_valid_json() {
    let (_dir, conn) = fresh_db();
    assert_sqlite_error(conn.execute(
        "INSERT INTO processed_commands (command_id, command_type, request, response, recorded_at)
         VALUES (?1, 'waste.log', '{}', '{invalid', ?2)",
        params![CMD, TS],
    ), ffi::SQLITE_CONSTRAINT_CHECK);
}

// AGENTS「ID 与时间」：营业日是 'YYYY-MM-DD' 且真实存在；非法日期不能因 NULL 而绕过 CHECK。
#[test]
fn business_date_must_be_a_real_iso_date() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    let mut event = Event::new(EVT);
    for date in [
        "2026-02-30",
        "2026-13-01",
        "2026-1-5",
        "2026/10/05",
        "20261005",
    ] {
        event.business_date = date;
        assert_sqlite_error(event.insert(&conn), ffi::SQLITE_CONSTRAINT_CHECK);
    }
    event.business_date = "2026-10-05";
    event.insert(&conn).unwrap();
}

// 001「DEFERRABLE INITIALLY DEFERRED」：同一事务允许先事件、后命令。
#[test]
fn deferred_command_fk_allows_event_before_command() {
    let (_dir, mut conn) = fresh_db();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    Event::new(EVT).insert(&tx).unwrap();
    insert_command(&tx, CMD, "{}").unwrap();
    tx.commit().unwrap();
    assert_eq!(event_seqs(&conn).unwrap(), [1]);
}

// 001：命令始终不存在时在提交阶段拒绝，并回滚事件。
#[test]
fn deferred_command_fk_rejects_missing_command_at_commit() {
    let (_dir, mut conn) = fresh_db();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    Event::new(EVT).insert(&tx).unwrap();
    assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    assert!(event_seqs(&conn).unwrap().is_empty());
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    assert_eq!(event_seqs(&conn).unwrap(), [1]);
}

// AGENTS「只追加」：省略 seq，由 SQLite 从 1 连续分配。
#[test]
fn automatically_assigned_seq_is_contiguous() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    for id in [EVT, EVT2, EVT3] {
        Event::new(id).insert(&conn).unwrap();
    }
    assert_eq!(event_seqs(&conn).unwrap(), [1, 2, 3]);
}

// AGENTS「只追加」：包含多个事件的事务回滚后，下一个 seq 仍是已提交的 max + 1。
#[test]
fn rolled_back_events_do_not_leave_seq_gaps() {
    let (_dir, mut conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    Event::new(EVT2).insert(&tx).unwrap();
    Event::new(EVT3).insert(&tx).unwrap();
    assert_eq!(event_seqs(&tx).unwrap(), [1, 2, 3]);
    tx.rollback().unwrap();
    assert_eq!(event_seqs(&conn).unwrap(), [1]);
    Event::new(EVT2).insert(&conn).unwrap();
    Event::new(EVT3).insert(&conn).unwrap();
    assert_eq!(event_seqs(&conn).unwrap(), [1, 2, 3]);
}

// AGENTS「只追加」：连续性触发器拒绝跳号的显式 seq（含 seq = 10）。
#[test]
fn explicit_seq_cannot_skip_numbers() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    let sql = "INSERT INTO store_events (seq, id, event_type, schema_version, aggregate_type,
                   aggregate_id, aggregate_version, command_id, actor_id, device_id,
                   business_date, occurred_at, recorded_at, payload)
               SELECT ?1, ?2, event_type, schema_version, aggregate_type,
                   ?2, aggregate_version, command_id, actor_id, device_id,
                   business_date, occurred_at, recorded_at, payload FROM store_events WHERE seq = 1";
    for seq in [-1, 0, 3, 10] {
        assert_trigger_reject(
            conn.execute(sql, params![seq, EVT2]),
            "store_events.seq must be contiguous",
        );
        assert_eq!(event_seqs(&conn).unwrap(), [1]);
    }
    conn.execute(sql, params![2, EVT2]).unwrap();
    assert_eq!(event_seqs(&conn).unwrap(), [1, 2]);
}

// AGENTS「幂等」：command_id 主键唯一。
#[test]
fn duplicate_command_id_is_rejected() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    assert_sqlite_error(
        insert_command(&conn, CMD, "{}"),
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
    );
}

// 001：外部事件 id 唯一，不能因采用 seq 主键而失去 UUID 去重约束。
#[test]
fn duplicate_event_id_is_rejected() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    let mut duplicate = Event::new(EVT);
    duplicate.aggregate_id = EVT2;
    assert_sqlite_error(duplicate.insert(&conn), ffi::SQLITE_CONSTRAINT_UNIQUE);
}

// 001：UNIQUE(aggregate_type, aggregate_id, aggregate_version) 三列共同生效。
#[test]
fn duplicate_aggregate_version_is_rejected() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    let mut event = Event::new(EVT2);
    event.aggregate_id = EVT;
    assert_sqlite_error(event.insert(&conn), ffi::SQLITE_CONSTRAINT_UNIQUE);
    event.aggregate_version = 2;
    event.insert(&conn).unwrap();
    event.id = EVT3;
    event.aggregate_version = 1;
    event.aggregate_type = "OTHER_RECORD";
    event.insert(&conn).unwrap();
    assert_eq!(event_seqs(&conn).unwrap(), [1, 2, 3]);
}

// 001：schema_version 和 aggregate_version 都从 1 开始。
#[test]
fn event_versions_must_be_positive() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    for invalid in [-1, 0] {
        let mut event = Event::new(EVT);
        event.schema_version = invalid;
        assert_sqlite_error(event.insert(&conn), ffi::SQLITE_CONSTRAINT_CHECK);
        event.schema_version = 1;
        event.aggregate_version = invalid;
        assert_sqlite_error(event.insert(&conn), ffi::SQLITE_CONSTRAINT_CHECK);
    }
}

// AGENTS「SQLite」所有表用 STRICT：非数字文本不能存入 001 三张表的 INTEGER 时间列。
#[test]
fn strict_tables_reject_text_in_integer_columns() {
    let (_dir, conn) = fresh_db();
    assert_sqlite_error(
        conn.execute(
            "INSERT INTO store_meta (id, store_id, created_at) VALUES (1, ?1, 'yesterday')",
            params![STORE],
        ),
        ffi::SQLITE_CONSTRAINT_DATATYPE,
    );
    assert_sqlite_error(conn.execute(
        "INSERT INTO processed_commands (command_id, command_type, request, response, recorded_at)
         VALUES (?1, 'waste.log', '{}', '{}', 'yesterday')",
        params![CMD],
    ), ffi::SQLITE_CONSTRAINT_DATATYPE);
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    for column in ["occurred_at", "recorded_at"] {
        assert_sqlite_error(
            conn.execute(
                &format!(
                    "INSERT INTO store_events (id, event_type, schema_version, aggregate_type,
                aggregate_id, aggregate_version, command_id, actor_id, device_id, business_date,
                occurred_at, recorded_at, payload)
             SELECT ?1, event_type, schema_version, aggregate_type, ?1, aggregate_version,
                command_id, actor_id, device_id, business_date, {}, {}, payload
             FROM store_events WHERE seq = 1",
                    if column == "occurred_at" {
                        "'yesterday'"
                    } else {
                        "occurred_at"
                    },
                    if column == "recorded_at" {
                        "'yesterday'"
                    } else {
                        "recorded_at"
                    }
                ),
                params![EVT2],
            ),
            ffi::SQLITE_CONSTRAINT_DATATYPE,
        );
    }
}

// AGENTS「只追加」：UPDATE / DELETE / REPLACE / UPSERT 都必须由触发器拒绝。
#[test]
fn processed_commands_are_append_only() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, r#"{"lines":[]}"#).unwrap();
    for sql in [
        "UPDATE processed_commands SET response = '{}'",
        "DELETE FROM processed_commands",
        "INSERT OR REPLACE INTO processed_commands
         SELECT command_id, command_type, request, '{}', recorded_at FROM processed_commands",
        "INSERT INTO processed_commands
         SELECT command_id, command_type, request, '{}', recorded_at FROM processed_commands WHERE 1
         ON CONFLICT(command_id) DO UPDATE SET response = excluded.response",
    ] {
        assert_trigger_reject(conn.execute(sql, []), "processed_commands is append-only");
        let stored: (i64, String, String, i64) = conn
            .query_row(
                "SELECT COUNT(*), request, response, recorded_at FROM processed_commands",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            stored,
            (1, r#"{"lines":[]}"#.into(), r#"{"warnings":[]}"#.into(), TS)
        );
    }
}

// AGENTS「只追加」：覆盖 seq、事件 id 和聚合版本三个冲突入口，避免只测到唯一约束。
#[test]
fn store_events_are_append_only() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    for sql in [
        "UPDATE store_events SET payload = '{}'",
        "DELETE FROM store_events",
        "INSERT OR REPLACE INTO store_events SELECT * FROM store_events",
        "INSERT INTO store_events SELECT * FROM store_events WHERE 1
         ON CONFLICT(seq) DO UPDATE SET payload = '{}'",
    ] {
        assert_trigger_reject(conn.execute(sql, []), "store_events is append-only");
    }
    let replacement_sql = INSERT_EVENT.replacen("INSERT INTO", "INSERT OR REPLACE INTO", 1);
    let mut replacement = Event::new(EVT);
    replacement.aggregate_id = EVT2;
    replacement.payload = "{}";
    // 不提供 seq；仅冲突于 id。
    assert_trigger_reject(
        replacement.execute_insert(&conn, &replacement_sql),
        "store_events is append-only",
    );
    let upsert_id_sql =
        format!("{INSERT_EVENT} ON CONFLICT(id) DO UPDATE SET payload = excluded.payload");
    assert_trigger_reject(
        replacement.execute_insert(&conn, &upsert_id_sql),
        "store_events is append-only",
    );
    // 新事件 id；仅冲突于聚合三元组。
    replacement.id = EVT2;
    replacement.aggregate_id = EVT;
    assert_trigger_reject(
        replacement.execute_insert(&conn, &replacement_sql),
        "store_events is append-only",
    );
    let upsert_aggregate_sql = format!(
        "{INSERT_EVENT} ON CONFLICT(aggregate_type, aggregate_id, aggregate_version)
        DO UPDATE SET payload = excluded.payload"
    );
    assert_trigger_reject(
        replacement.execute_insert(&conn, &upsert_aggregate_sql),
        "store_events is append-only",
    );
    let stored: (String, String) = conn
        .query_row(
            "SELECT id, payload FROM store_events WHERE seq = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, (EVT.into(), r#"{"lines":[]}"#.into()));
    assert_eq!(event_seqs(&conn).unwrap(), [1]);
}

fn insert_equipment(
    conn: &Connection,
    id: &str,
    code: &str,
    name: &str,
    equipment_type: &str,
    active: i64,
    revision: i64,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO equipment (id, code, name, equipment_type, active, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, code, name, equipment_type, active, revision],
    )
}

// AGENTS「迁移」：已有 001 数据的库升级到最新版本，数据不丢失，新表为空。
#[test]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
fn upgrades_a_version_1_database_without_losing_data() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = open_writer(&dir.path().join("boh.db")).unwrap();
    conn.execute_batch(MIGRATION_001).unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    insert_meta(&conn, 1, STORE).unwrap();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();

    migrate(&mut conn).unwrap();

    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
    assert_eq!(event_seqs(&conn).unwrap(), [1]);
    let counts: (i64, i64, i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM store_meta), (SELECT COUNT(*) FROM processed_commands),
                    (SELECT COUNT(*) FROM equipment), (SELECT COUNT(*) FROM temperature_readings)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(counts, (1, 1, 0, 0));
}

// 002 + AGENTS「ID 与时间」：设备主键是 UUIDv7。
#[test]
fn equipment_id_rejects_v4_uppercase_and_wrong_length() {
    let (_dir, conn) = fresh_db();
    for invalid in INVALID_IDS {
        assert_sqlite_error(
            insert_equipment(&conn, invalid, "F1", "Walk-in", "FRIDGE", 1, 1),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }
    insert_equipment(&conn, EVT, "F1", "Walk-in", "FRIDGE", 1, 1).unwrap();
}

// domain「主数据」：code 在同一实体内唯一，停用的设备也占用 code。
#[test]
fn equipment_code_is_unique() {
    let (_dir, conn) = fresh_db();
    insert_equipment(&conn, EVT, "F1", "Walk-in", "FRIDGE", 0, 1).unwrap();
    assert_sqlite_error(
        insert_equipment(&conn, EVT2, "F1", "Reach-in", "FREEZER", 1, 1),
        ffi::SQLITE_CONSTRAINT_UNIQUE,
    );
    insert_equipment(&conn, EVT2, "F2", "Reach-in", "FREEZER", 1, 1).unwrap();
}

// domain「主数据」：code、name 非空。
#[test]
fn equipment_code_and_name_must_be_non_empty() {
    let (_dir, conn) = fresh_db();
    assert_sqlite_error(
        insert_equipment(&conn, EVT, "", "Walk-in", "FRIDGE", 1, 1),
        ffi::SQLITE_CONSTRAINT_CHECK,
    );
    assert_sqlite_error(
        insert_equipment(&conn, EVT, "F1", "", "FRIDGE", 1, 1),
        ffi::SQLITE_CONSTRAINT_CHECK,
    );
    insert_equipment(&conn, EVT, "F1", "Walk-in", "FRIDGE", 1, 1).unwrap();
}

// domain「主数据」EQUIPMENT 的 equipment_type 枚举：全部取值可用，其他取值（含小写）被拒绝。
#[test]
fn equipment_type_must_be_a_known_value() {
    let (_dir, conn) = fresh_db();
    for invalid in ["fridge", "COOLER", ""] {
        assert_sqlite_error(
            insert_equipment(&conn, EVT, "X", "X", invalid, 1, 1),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }
    let ids = [
        "01890a5d-ac96-774b-bcce-b302099a8060",
        "01890a5d-ac96-774b-bcce-b302099a8061",
        "01890a5d-ac96-774b-bcce-b302099a8062",
        "01890a5d-ac96-774b-bcce-b302099a8063",
        "01890a5d-ac96-774b-bcce-b302099a8064",
        "01890a5d-ac96-774b-bcce-b302099a8065",
        "01890a5d-ac96-774b-bcce-b302099a8066",
    ];
    for (id, equipment_type) in ids.into_iter().zip(EQUIPMENT_TYPES) {
        insert_equipment(&conn, id, equipment_type, "X", equipment_type, 1, 1).unwrap();
    }
}

// domain「主数据」：active 是布尔值（0 / 1），revision 从 1 开始。
#[test]
fn equipment_active_is_boolean_and_revision_is_positive() {
    let (_dir, conn) = fresh_db();
    for active in [-1, 2] {
        assert_sqlite_error(
            insert_equipment(&conn, EVT, "F1", "Walk-in", "FRIDGE", active, 1),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }
    for revision in [-1, 0] {
        assert_sqlite_error(
            insert_equipment(&conn, EVT, "F1", "Walk-in", "FRIDGE", 1, revision),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }
    insert_equipment(&conn, EVT, "F1", "Walk-in", "FRIDGE", 0, 1).unwrap();
}

// AGENTS「SQLite」所有表用 STRICT：equipment 的 INTEGER 列不接受文本。
#[test]
fn equipment_is_strict() {
    let (_dir, conn) = fresh_db();
    for (active, revision) in [("'true'", "1"), ("1", "'one'")] {
        assert_sqlite_error(
            conn.execute(
                &format!(
                    "INSERT INTO equipment (id, code, name, equipment_type, active, revision)
                     VALUES (?1, 'F1', 'Walk-in', 'FRIDGE', {active}, {revision})"
                ),
                params![EVT],
            ),
            ffi::SQLITE_CONSTRAINT_DATATYPE,
        );
    }
}

// AGENTS「只追加」：equipment 是投影，不是账本；重建时必须能清空并重写，所以没有只追加触发器。
#[test]
fn equipment_projection_can_be_cleared_and_rewritten() {
    let (_dir, conn) = fresh_db();
    insert_equipment(&conn, EVT, "F1", "Walk-in", "FRIDGE", 1, 1).unwrap();
    conn.execute(
        "UPDATE equipment SET name = 'Walk-in 2', revision = 2 WHERE id = ?1",
        params![EVT],
    )
    .unwrap();
    assert_eq!(conn.execute("DELETE FROM equipment", []).unwrap(), 1);
    insert_equipment(&conn, EVT, "F1", "Walk-in", "FRIDGE", 1, 1).unwrap();
}

const EQUIPMENT: &str = "01890a5d-ac96-774b-bcce-b302099a8301";
const EQUIPMENT2: &str = "01890a5d-ac96-774b-bcce-b302099a8302";
const READING: &str = "01890a5d-ac96-774b-bcce-b302099a8401";
const READING2: &str = "01890a5d-ac96-774b-bcce-b302099a8402";
const READING3: &str = "01890a5d-ac96-774b-bcce-b302099a8403";
const DEVICE: &str = "01890a5d-ac96-774b-bcce-b302099a8201";
const INSERT_READING: &str =
    "INSERT INTO temperature_readings (id, event_seq, equipment_id, celsius_x10, note,
         actor_id, device_id, business_date, occurred_at, recorded_at)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

struct Reading<'a> {
    id: &'a str,
    event_seq: i64,
    equipment_id: &'a str,
    celsius_x10: i64,
    note: Option<&'a str>,
    actor_id: &'a str,
    device_id: &'a str,
    business_date: &'a str,
    occurred_at: i64,
    recorded_at: i64,
}

impl<'a> Reading<'a> {
    fn new(id: &'a str, event_seq: i64) -> Self {
        Self {
            id,
            event_seq,
            equipment_id: EQUIPMENT,
            celsius_x10: 38,
            note: None,
            actor_id: ACTOR,
            device_id: DEVICE,
            business_date: "2026-10-05",
            occurred_at: TS - 1000,
            recorded_at: TS,
        }
    }

    fn insert(&self, conn: &Connection) -> rusqlite::Result<usize> {
        conn.execute(
            INSERT_READING,
            params![
                self.id,
                self.event_seq,
                self.equipment_id,
                self.celsius_x10,
                self.note,
                self.actor_id,
                self.device_id,
                self.business_date,
                self.occurred_at,
                self.recorded_at,
            ],
        )
    }
}

/// 最新 schema，账本中已有 seq 1～3 三个事件，设备 EQUIPMENT 已存在。
#[allow(clippy::unwrap_used)] // 测试夹具：前置数据写入失败时测试无法开始，直接终止。
fn reading_db() -> (TempDir, Connection) {
    let (dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    for id in [EVT, EVT2, EVT3] {
        Event::new(id).insert(&conn).unwrap();
    }
    insert_equipment(&conn, EQUIPMENT, "F1", "Walk-in", "FRIDGE", 1, 1).unwrap();
    (dir, conn)
}

fn reading_ids(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    conn.prepare("SELECT id FROM temperature_readings ORDER BY event_seq")?
        .query_map([], |row| row.get(0))?
        .collect()
}

/// 整表内容，按第一列排序。
fn table_rows(
    conn: &Connection,
    table: &str,
) -> rusqlite::Result<Vec<Vec<rusqlite::types::Value>>> {
    let mut statement = conn.prepare(&format!("SELECT * FROM {table} ORDER BY 1"))?;
    let columns = statement.column_count();
    statement
        .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())?
        .collect()
}

// AGENTS「迁移」：已有 002 数据（含设备投影）的库升级到 003，原有四张表逐行不变，新表为空且可引用原有事件和设备。
#[test]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
fn upgrades_a_version_2_database_without_losing_data() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = open_writer(&dir.path().join("boh.db")).unwrap();
    conn.execute_batch(MIGRATION_001).unwrap();
    conn.execute_batch(MIGRATION_002).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    insert_meta(&conn, 1, STORE).unwrap();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    insert_equipment(&conn, EQUIPMENT, "F1", "Walk-in", "FREEZER", 0, 2).unwrap();
    let tables = [
        "store_meta",
        "processed_commands",
        "store_events",
        "equipment",
    ];
    let before: Vec<_> = tables
        .iter()
        .map(|t| table_rows(&conn, t).unwrap())
        .collect();

    migrate(&mut conn).unwrap();

    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
    let after: Vec<_> = tables
        .iter()
        .map(|t| table_rows(&conn, t).unwrap())
        .collect();
    assert_eq!(after, before);
    assert!(reading_ids(&conn).unwrap().is_empty());
    Reading::new(READING, 1).insert(&conn).unwrap();
    assert_eq!(reading_ids(&conn).unwrap(), [READING]);
}

// 003 + AGENTS「ID 与时间」：读数 id、actor_id、device_id 是 UUIDv7。
#[test]
fn temperature_reading_uuid_columns_reject_v4_uppercase_and_wrong_length() {
    let (_dir, conn) = reading_db();
    for invalid in INVALID_IDS {
        for column in ["id", "actor_id", "device_id"] {
            let mut reading = Reading::new(READING, 1);
            match column {
                "id" => reading.id = invalid,
                "actor_id" => reading.actor_id = invalid,
                "device_id" => reading.device_id = invalid,
                _ => unreachable!(),
            }
            assert_sqlite_error(reading.insert(&conn), ffi::SQLITE_CONSTRAINT_CHECK);
        }
    }
    Reading::new(READING, 1).insert(&conn).unwrap();
}

// 003：读数 id 唯一；每个事件至多一行读数（event_seq 唯一）。
#[test]
fn temperature_reading_id_and_event_seq_are_unique() {
    let (_dir, conn) = reading_db();
    Reading::new(READING, 1).insert(&conn).unwrap();
    assert_sqlite_error(
        Reading::new(READING, 2).insert(&conn),
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
    );
    assert_sqlite_error(
        Reading::new(READING2, 1).insert(&conn),
        ffi::SQLITE_CONSTRAINT_UNIQUE,
    );
    Reading::new(READING2, 2).insert(&conn).unwrap();
    assert_eq!(reading_ids(&conn).unwrap(), [READING, READING2]);
}

// 003 引用完整性：event_seq 必须是账本中已有的事件，立即检查。
#[test]
fn temperature_reading_event_seq_must_reference_an_event() {
    let (_dir, conn) = reading_db();
    for seq in [0, 4, 99] {
        assert_sqlite_error(
            Reading::new(READING, seq).insert(&conn),
            ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
        );
    }
    assert!(reading_ids(&conn).unwrap().is_empty());
    Reading::new(READING, 3).insert(&conn).unwrap();
}

// 003 引用完整性：equipment_id 必须是已有设备；外键延迟到提交时检查，同一事务允许先写读数、后写设备。
#[test]
fn temperature_reading_equipment_is_checked_at_commit() {
    let (_dir, mut conn) = reading_db();
    let mut orphan = Reading::new(READING, 1);
    orphan.equipment_id = EQUIPMENT2;
    // 自动提交：语句结束即提交，立即拒绝。
    assert_sqlite_error(orphan.insert(&conn), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    orphan.insert(&tx).unwrap();
    assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    assert!(reading_ids(&conn).unwrap().is_empty());

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    orphan.insert(&tx).unwrap();
    insert_equipment(&tx, EQUIPMENT2, "F2", "Reach-in", "FREEZER", 1, 1).unwrap();
    tx.commit().unwrap();
    assert_eq!(reading_ids(&conn).unwrap(), [READING]);
}

// AGENTS「只追加」：temperature_readings 是投影，没有只追加触发器；重建在一个事务内按任意顺序清空
// equipment 与本表再重写，提交时引用完整即可。只删设备、留下引用它的读数则在提交时拒绝。
#[test]
fn temperature_projection_can_be_cleared_and_rewritten() {
    let (_dir, mut conn) = reading_db();
    Reading::new(READING, 1).insert(&conn).unwrap();
    conn.execute(
        "UPDATE temperature_readings SET celsius_x10 = 40, note = 'x' WHERE id = ?1",
        params![READING],
    )
    .unwrap();

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    tx.execute("DELETE FROM equipment", []).unwrap();
    assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    assert_eq!(reading_ids(&conn).unwrap(), [READING]);

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    assert_eq!(tx.execute("DELETE FROM equipment", []).unwrap(), 1);
    assert_eq!(
        tx.execute("DELETE FROM temperature_readings", []).unwrap(),
        1
    );
    Reading::new(READING, 1).insert(&tx).unwrap();
    insert_equipment(&tx, EQUIPMENT, "F1", "Walk-in", "FRIDGE", 1, 1).unwrap();
    tx.commit().unwrap();
    let stored: (i64, Option<String>) = conn
        .query_row(
            "SELECT celsius_x10, note FROM temperature_readings WHERE id = ?1",
            params![READING],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, (38, None));
}

// 003：celsius_x10 是 0.1 °C 的整数，取值 -500～5000（-50.0～500.0 °C），两端可取。
#[test]
fn celsius_x10_is_bounded() {
    let (_dir, conn) = reading_db();
    for invalid in [-501, 5001, i64::MIN, i64::MAX] {
        let mut reading = Reading::new(READING, 1);
        reading.celsius_x10 = invalid;
        assert_sqlite_error(reading.insert(&conn), ffi::SQLITE_CONSTRAINT_CHECK);
    }
    for (id, seq, celsius_x10) in [(READING, 1, -500), (READING2, 2, 0), (READING3, 3, 5000)] {
        let mut reading = Reading::new(id, seq);
        reading.celsius_x10 = celsius_x10;
        reading.insert(&conn).unwrap();
    }
}

// 003：note 为 NULL（省略）或非空文本，最多 200 个字符（按字符计，不按字节）。
#[test]
fn temperature_note_is_null_or_non_empty_and_at_most_200_characters() {
    let (_dir, conn) = reading_db();
    let long_ascii = "a".repeat(201);
    let long_cjk = "冷".repeat(201);
    for invalid in ["", long_ascii.as_str(), long_cjk.as_str()] {
        let mut reading = Reading::new(READING, 1);
        reading.note = Some(invalid);
        assert_sqlite_error(reading.insert(&conn), ffi::SQLITE_CONSTRAINT_CHECK);
    }
    let max_cjk = "冷".repeat(200);
    Reading::new(READING, 1).insert(&conn).unwrap();
    let mut reading = Reading::new(READING2, 2);
    reading.note = Some(&max_cjk);
    reading.insert(&conn).unwrap();
    let notes: Vec<Option<String>> = conn
        .prepare("SELECT note FROM temperature_readings ORDER BY event_seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(notes, [None, Some(max_cjk)]);
}

// AGENTS「ID 与时间」：营业日是 'YYYY-MM-DD' 且真实存在。
#[test]
fn temperature_business_date_must_be_a_real_iso_date() {
    let (_dir, conn) = reading_db();
    for date in [
        "2026-02-30",
        "2026-13-01",
        "2026-1-5",
        "2026/10/05",
        "20261005",
    ] {
        let mut reading = Reading::new(READING, 1);
        reading.business_date = date;
        assert_sqlite_error(reading.insert(&conn), ffi::SQLITE_CONSTRAINT_CHECK);
    }
    Reading::new(READING, 1).insert(&conn).unwrap();
}

// AGENTS「SQLite」所有表用 STRICT：temperature_readings 的 INTEGER 列不接受非整数文本或小数。
#[test]
fn temperature_readings_is_strict() {
    let (_dir, conn) = reading_db();
    for column in ["event_seq", "celsius_x10", "occurred_at", "recorded_at"] {
        for value in ["'warm'", "38.5"] {
            let values: Vec<&str> = [
                ("id", "?1"),
                ("event_seq", "1"),
                ("equipment_id", "?2"),
                ("celsius_x10", "38"),
                ("actor_id", "?3"),
                ("device_id", "?4"),
                ("business_date", "'2026-10-05'"),
                ("occurred_at", "0"),
                ("recorded_at", "0"),
            ]
            .iter()
            .map(|(name, default)| if *name == column { value } else { default })
            .collect();
            assert_sqlite_error(
                conn.execute(
                    &format!(
                        "INSERT INTO temperature_readings (id, event_seq, equipment_id, celsius_x10,
                             actor_id, device_id, business_date, occurred_at, recorded_at)
                         VALUES ({})",
                        values.join(", ")
                    ),
                    params![READING, EQUIPMENT, ACTOR, DEVICE],
                ),
                ffi::SQLITE_CONSTRAINT_DATATYPE,
            );
        }
    }
    assert!(reading_ids(&conn).unwrap().is_empty());
}

// 004 + AGENTS「HTTP 约定」/health：clock_regression_ms 每次请求取 max(store_events.recorded_at)，
// 索引只建在 recorded_at 一列上，SQLite 用它的 min/max 优化直接取最大值，不扫描账本。
#[test]
fn recorded_at_index_serves_the_clock_regression_query() {
    let (_dir, conn) = fresh_db();
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_index_info('idx_store_events_recorded_at')")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(columns, ["recorded_at"]);
    let (table, unique): (String, i64) = conn
        .query_row(
            "SELECT tbl_name, (SELECT \"unique\" FROM pragma_index_list('store_events')
                               WHERE name = 'idx_store_events_recorded_at')
             FROM sqlite_master WHERE type = 'index' AND name = 'idx_store_events_recorded_at'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((table.as_str(), unique), ("store_events", 0));
    let plan: Vec<String> = conn
        .prepare("EXPLAIN QUERY PLAN SELECT max(recorded_at) FROM store_events")
        .unwrap()
        .query_map([], |r| r.get(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        plan,
        ["SEARCH store_events USING COVERING INDEX idx_store_events_recorded_at"]
    );
}

// AGENTS「迁移」：已有 003 数据的库升级到 004，原有表逐行不变；同一 recorded_at 可以出现在多个事件上（索引不唯一）。
#[test]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
fn upgrades_a_version_3_database_without_losing_data() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = open_writer(&dir.path().join("boh.db")).unwrap();
    conn.execute_batch(MIGRATION_001).unwrap();
    conn.execute_batch(MIGRATION_002).unwrap();
    conn.execute_batch(MIGRATION_003).unwrap();
    conn.pragma_update(None, "user_version", 3).unwrap();
    insert_meta(&conn, 1, STORE).unwrap();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    insert_equipment(&conn, EQUIPMENT, "F1", "Walk-in", "FREEZER", 1, 1).unwrap();
    Reading::new(READING, 1).insert(&conn).unwrap();
    let tables = [
        "store_meta",
        "processed_commands",
        "store_events",
        "equipment",
        "temperature_readings",
    ];
    let before: Vec<_> = tables
        .iter()
        .map(|t| table_rows(&conn, t).unwrap())
        .collect();

    migrate(&mut conn).unwrap();

    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
    let after: Vec<_> = tables
        .iter()
        .map(|t| table_rows(&conn, t).unwrap())
        .collect();
    assert_eq!(after, before);
    Event::new(EVT2).insert(&conn).unwrap();
    let recorded: Vec<i64> = conn
        .prepare("SELECT recorded_at FROM store_events ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(recorded, [TS, TS]);
}

// ---- 005：其余主数据与库存投影 ----

const ITEM: &str = "01890a5d-ac96-774b-bcce-b302099a8501";
const ITEM2: &str = "01890a5d-ac96-774b-bcce-b302099a8502";
const ITEM3: &str = "01890a5d-ac96-774b-bcce-b302099a8503";
const RECIPE: &str = "01890a5d-ac96-774b-bcce-b302099a8601";
const RECIPE2: &str = "01890a5d-ac96-774b-bcce-b302099a8602";
const SUPPLIER: &str = "01890a5d-ac96-774b-bcce-b302099a8701";
const SUPPLIER2: &str = "01890a5d-ac96-774b-bcce-b302099a8702";
const REASON: &str = "01890a5d-ac96-774b-bcce-b302099a8801";
const REASON2: &str = "01890a5d-ac96-774b-bcce-b302099a8802";
const LOT: &str = "RAW-FLOUR-20261005-001";
const LOT2: &str = "RAW-FLOUR-20261005-002";
const LOT3: &str = "RAW-FLOUR-20261005-003";
/// 迁移 007 之前（005、006）的 UUID 批次标识。
const UUID_LOT: &str = "01890a5d-ac96-774b-bcce-b302099a8901";
const UUID_LOT2: &str = "01890a5d-ac96-774b-bcce-b302099a8902";

/// 一行的全部列：(列名, SQL 字面量)。
type Row = Vec<(&'static str, String)>;

fn lit(text: &str) -> String {
    format!("'{text}'")
}

/// 把 `row` 中的一列换成另一个 SQL 字面量；列名不存在时 panic（测试写错）。
fn with(mut row: Row, column: &str, value: &str) -> Row {
    let slot = row
        .iter_mut()
        .find(|(name, _)| *name == column)
        .unwrap_or_else(|| panic!("no column {column}"));
    slot.1 = value.to_owned();
    row
}

fn insert(conn: &Connection, table: &str, row: &Row) -> rusqlite::Result<usize> {
    let columns: Vec<&str> = row.iter().map(|(name, _)| *name).collect();
    let values: Vec<&str> = row.iter().map(|(_, value)| value.as_str()).collect();
    conn.execute(
        &format!(
            "INSERT INTO {table} ({}) VALUES ({})",
            columns.join(", "),
            values.join(", ")
        ),
        [],
    )
}

fn item_row(id: &str, code: &str) -> Row {
    vec![
        ("id", lit(id)),
        ("code", lit(code)),
        ("name", lit("高筋面粉")),
        ("base_unit", lit("g")),
        ("category", lit("RAW")),
        ("default_shelf_life_ms", "NULL".into()),
        ("active", "1".into()),
        ("revision", "1".into()),
    ]
}

fn item_unit_row(item_id: &str, unit_code: &str) -> Row {
    vec![
        ("item_id", lit(item_id)),
        ("unit_code", lit(unit_code)),
        ("base_qty_per_unit", "25000".into()),
    ]
}

fn recipe_row(id: &str, code: &str) -> Row {
    vec![
        ("id", lit(id)),
        ("code", lit(code)),
        ("name", lit("吐司")),
        ("output_item_id", lit(ITEM2)),
        ("active", "1".into()),
        ("revision", "1".into()),
    ]
}

fn recipe_version_row(recipe_id: &str, version: i64) -> Row {
    vec![
        ("recipe_id", lit(recipe_id)),
        ("version", version.to_string()),
        ("output_qty_per_batch", "10".into()),
    ]
}

fn recipe_line_row(recipe_id: &str, version: i64, line_no: i64, item_id: &str) -> Row {
    vec![
        ("recipe_id", lit(recipe_id)),
        ("version", version.to_string()),
        ("line_no", line_no.to_string()),
        ("item_id", lit(item_id)),
        ("qty_per_batch", "500".into()),
    ]
}

fn supplier_row(id: &str, code: &str) -> Row {
    vec![
        ("id", lit(id)),
        ("code", lit(code)),
        ("name", lit("面粉供应商")),
        ("contact_phone", "NULL".into()),
        ("active", "1".into()),
        ("revision", "1".into()),
    ]
}

fn waste_reason_row(id: &str, code: &str) -> Row {
    vec![
        ("id", lit(id)),
        ("code", lit(code)),
        ("name", lit("过期")),
        ("active", "1".into()),
        ("revision", "1".into()),
    ]
}

/// 一个收货批次：批次日期与流水号从批次号末尾的 `YYYYMMDD-NNN` 拆出；批次号格式不对时 panic（测试写错）。
fn lot_row(lot_id: &str, source_event_seq: i64, source_line_no: i64) -> Row {
    let tail = &lot_id[lot_id.len() - 12..];
    let serial: i64 = tail[9..]
        .parse()
        .unwrap_or_else(|_| panic!("not a lot number: {lot_id}"));
    vec![
        ("lot_id", lit(lot_id)),
        ("item_id", lit(ITEM)),
        (
            "lot_date",
            lit(&format!("{}-{}-{}", &tail[0..4], &tail[4..6], &tail[6..8])),
        ),
        ("lot_serial", serial.to_string()),
        ("origin", lit("RECEIPT")),
        ("source_event_seq", source_event_seq.to_string()),
        ("source_line_no", source_line_no.to_string()),
        ("remaining_qty", "10".into()),
        ("expires_at", "NULL".into()),
        ("manufacturer_lot_no", "NULL".into()),
    ]
}

/// 迁移 007 之前（005、006）的批次行：UUID 批次标识，由 seq 1 的第 `source_line_no` 行建立；
/// `lot_number_column` 是生产商批号的列名（005 为 supplier_lot_no，006 改名为 manufacturer_lot_no）。
fn uuid_lot_row(lot_id: &str, source_line_no: i64, lot_number_column: &'static str) -> Row {
    vec![
        ("lot_id", lit(lot_id)),
        ("item_id", lit(ITEM)),
        ("origin", lit("RECEIPT")),
        ("source_event_seq", "1".into()),
        ("source_line_no", source_line_no.to_string()),
        ("remaining_qty", "10".into()),
        ("expires_at", "NULL".into()),
        (lot_number_column, lit("B-2026-10")),
    ]
}

fn unallocated_row(item_id: &str, qty: i64) -> Row {
    vec![("item_id", lit(item_id)), ("qty", qty.to_string())]
}

/// 一条未被吸收的收货流水：新建批次 LOT，+10。
fn movement_row(event_seq: i64, movement_no: i64) -> Row {
    vec![
        ("event_seq", event_seq.to_string()),
        ("movement_no", movement_no.to_string()),
        ("item_id", lit(ITEM)),
        ("lot_id", lit(LOT)),
        ("kind", lit("RECEIPT")),
        ("alloc_source", lit("NEW_LOT")),
        ("nominal_qty", "10".into()),
        ("qty_delta", "10".into()),
        ("absorbed_by_event_id", "NULL".into()),
        ("physical_at", TS.to_string()),
        ("business_date", lit("2026-10-05")),
    ]
}

/// 一条被事件 EVT 吸收的报损流水。
fn absorbed_row(event_seq: i64, movement_no: i64) -> Row {
    vec![
        ("event_seq", event_seq.to_string()),
        ("movement_no", movement_no.to_string()),
        ("item_id", lit(ITEM)),
        ("lot_id", "NULL".into()),
        ("kind", lit("WASTE")),
        ("alloc_source", lit("ABSORBED")),
        ("nominal_qty", "-3".into()),
        ("qty_delta", "0".into()),
        ("absorbed_by_event_id", lit(EVT)),
        ("physical_at", TS.to_string()),
        ("business_date", lit("2026-10-05")),
    ]
}

fn count_row(event_seq: i64, item_id: &str) -> Row {
    vec![
        ("event_seq", event_seq.to_string()),
        ("item_id", lit(item_id)),
        ("observed_at", TS.to_string()),
        ("book_qty", "10".into()),
        ("counted_qty", "8".into()),
    ]
}

/// 最新 schema，账本中已有 seq 1～3 三个事件，物料 ITEM、ITEM2 已存在。
#[allow(clippy::unwrap_used)] // 测试夹具：前置数据写入失败时测试无法开始，直接终止。
fn master_db() -> (TempDir, Connection) {
    let (dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    for id in [EVT, EVT2, EVT3] {
        Event::new(id).insert(&conn).unwrap();
    }
    insert(&conn, "items", &item_row(ITEM, "FLOUR")).unwrap();
    insert(&conn, "items", &item_row(ITEM2, "TOAST")).unwrap();
    (dir, conn)
}

/// master_db，另有批次 LOT（物料 ITEM，由 seq 1 建立）。
#[allow(clippy::unwrap_used)] // 测试夹具：前置数据写入失败时测试无法开始，直接终止。
fn inventory_db() -> (TempDir, Connection) {
    let (dir, conn) = master_db();
    insert(&conn, "inventory_lots", &lot_row(LOT, 1, 0)).unwrap();
    (dir, conn)
}

/// 依次执行给定的迁移 SQL（从 001 开始），user_version 设为迁移个数；不经 `migrate`。
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
#[allow(clippy::unwrap_used)] // 测试夹具：建库或迁移失败时测试无法开始，直接终止。
fn db_at_version(migrations: &[&str]) -> (TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = open_writer(&dir.path().join("boh.db")).unwrap();
    for sql in migrations {
        conn.execute_batch(sql).unwrap();
    }
    conn.pragma_update(None, "user_version", migrations.len() as i64)
        .unwrap();
    (dir, conn)
}

fn count_rows(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
}

fn immediate(conn: &mut Connection) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
}

const TABLES_005: [&str; 11] = [
    "items",
    "item_units",
    "recipes",
    "recipe_versions",
    "recipe_lines",
    "suppliers",
    "waste_reasons",
    "inventory_lots",
    "inventory_unallocated",
    "inventory_movements",
    "inventory_counts",
];

// AGENTS「迁移」：已有 004 数据（含设备与温度投影）的库升级到 005，原有表逐行不变；
// 新表为空，视图 inventory_on_hand 没有行。
#[test]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
fn upgrades_a_version_4_database_without_losing_data() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = open_writer(&dir.path().join("boh.db")).unwrap();
    for sql in [MIGRATION_001, MIGRATION_002, MIGRATION_003, MIGRATION_004] {
        conn.execute_batch(sql).unwrap();
    }
    conn.pragma_update(None, "user_version", 4).unwrap();
    insert_meta(&conn, 1, STORE).unwrap();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    insert_equipment(&conn, EQUIPMENT, "F1", "Walk-in", "FREEZER", 1, 1).unwrap();
    Reading::new(READING, 1).insert(&conn).unwrap();
    let tables = [
        "store_meta",
        "processed_commands",
        "store_events",
        "equipment",
        "temperature_readings",
    ];
    let before: Vec<_> = tables
        .iter()
        .map(|t| table_rows(&conn, t).unwrap())
        .collect();

    migrate(&mut conn).unwrap();

    assert_eq!(schema_version(&conn).unwrap(), LATEST_SCHEMA_VERSION);
    let after: Vec<_> = tables
        .iter()
        .map(|t| table_rows(&conn, t).unwrap())
        .collect();
    assert_eq!(after, before);
    for table in TABLES_005 {
        assert_eq!(count_rows(&conn, table).unwrap(), 0, "{table}");
    }
    assert_eq!(count_rows(&conn, "inventory_on_hand").unwrap(), 0);
}

// 006：已有 005 数据的库执行 006，inventory_lots 的批号列改名为 manufacturer_lot_no，各行取值不变；改名后的列仍拒绝空串。
// 迁移 007 会重建批次表，这里只执行到 006。
#[test]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
fn migration_006_renames_the_lot_number_column() {
    let (_dir, conn) = db_at_version(&[
        MIGRATION_001,
        MIGRATION_002,
        MIGRATION_003,
        MIGRATION_004,
        MIGRATION_005,
    ]);
    insert_meta(&conn, 1, STORE).unwrap();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    insert(&conn, "items", &item_row(ITEM, "FLOUR")).unwrap();
    insert(
        &conn,
        "inventory_lots",
        &uuid_lot_row(UUID_LOT, 0, "supplier_lot_no"),
    )
    .unwrap();
    let before = table_rows(&conn, "inventory_lots").unwrap();

    conn.execute_batch(MIGRATION_006).unwrap();

    assert_eq!(table_rows(&conn, "inventory_lots").unwrap(), before);
    let stored: String = conn
        .query_row(
            "SELECT manufacturer_lot_no FROM inventory_lots WHERE lot_id = ?1",
            [UUID_LOT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, "B-2026-10");
    assert!(
        conn.prepare("SELECT supplier_lot_no FROM inventory_lots")
            .is_err()
    );
    assert_sqlite_error(
        insert(
            &conn,
            "inventory_lots",
            &with(
                uuid_lot_row(UUID_LOT2, 1, "manufacturer_lot_no"),
                "manufacturer_lot_no",
                "''",
            ),
        ),
        ffi::SQLITE_CONSTRAINT_CHECK,
    );
}

// 005 + AGENTS「ID 与时间」：items、recipes、suppliers、waste_reasons 的主键是 UUIDv7（批次号见 007 的测试）。
#[test]
fn master_data_ids_reject_v4_uppercase_and_wrong_length() {
    let (_dir, conn) = master_db();
    for invalid in INVALID_IDS {
        let value = lit(invalid);
        for (table, row) in [
            ("items", with(item_row(ITEM3, "X"), "id", &value)),
            ("recipes", with(recipe_row(RECIPE, "X"), "id", &value)),
            ("suppliers", with(supplier_row(SUPPLIER, "X"), "id", &value)),
            (
                "waste_reasons",
                with(waste_reason_row(REASON, "X"), "id", &value),
            ),
        ] {
            assert_sqlite_error(insert(&conn, table, &row), ffi::SQLITE_CONSTRAINT_CHECK);
        }
    }
    insert(&conn, "items", &item_row(ITEM3, "X")).unwrap();
    insert(&conn, "recipes", &recipe_row(RECIPE, "X")).unwrap();
    insert(&conn, "suppliers", &supplier_row(SUPPLIER, "X")).unwrap();
    insert(&conn, "waste_reasons", &waste_reason_row(REASON, "X")).unwrap();
}

// domain「主数据」：code 在同一实体内唯一，停用的行也占用 code；不同实体可以用相同的 code。
#[test]
fn master_data_codes_are_unique_within_each_entity() {
    let (_dir, conn) = master_db();
    let duplicate = with(item_row(ITEM3, "FLOUR"), "active", "0");
    assert_sqlite_error(
        insert(&conn, "items", &duplicate),
        ffi::SQLITE_CONSTRAINT_UNIQUE,
    );
    for (table, first, second) in [
        (
            "recipes",
            recipe_row(RECIPE, "C1"),
            recipe_row(RECIPE2, "C1"),
        ),
        (
            "suppliers",
            supplier_row(SUPPLIER, "C1"),
            supplier_row(SUPPLIER2, "C1"),
        ),
        (
            "waste_reasons",
            waste_reason_row(REASON, "C1"),
            waste_reason_row(REASON2, "C1"),
        ),
    ] {
        insert(&conn, table, &with(first, "active", "0")).unwrap();
        assert_sqlite_error(insert(&conn, table, &second), ffi::SQLITE_CONSTRAINT_UNIQUE);
    }
    insert(&conn, "items", &item_row(ITEM3, "C1")).unwrap();
}

// domain「主数据」：code、name 非空；active 是布尔值（0 / 1），revision 从 1 开始。
#[test]
fn master_data_text_flags_and_revisions_are_checked() {
    let (_dir, conn) = master_db();
    let rows: [(&str, Row); 4] = [
        ("items", item_row(ITEM3, "X")),
        ("recipes", recipe_row(RECIPE, "X")),
        ("suppliers", supplier_row(SUPPLIER, "X")),
        ("waste_reasons", waste_reason_row(REASON, "X")),
    ];
    for (table, row) in &rows {
        for (column, value) in [
            ("code", "''"),
            ("name", "''"),
            ("active", "-1"),
            ("active", "2"),
            ("revision", "0"),
            ("revision", "-1"),
        ] {
            assert_sqlite_error(
                insert(&conn, table, &with(row.clone(), column, value)),
                ffi::SQLITE_CONSTRAINT_CHECK,
            );
        }
        insert(&conn, table, &with(row.clone(), "active", "0")).unwrap();
    }
}

// domain「主数据」ITEM：base_unit 只能是 g / ml / pcs，category 只能是 RAW / SEMI / FINISHED（区分大小写）；
// default_shelf_life_ms 为 NULL（快照中省略）或正整数。
#[test]
fn item_enums_and_shelf_life_are_checked() {
    let (_dir, conn) = master_db();
    let invalid = [
        ("base_unit", "'G'"),
        ("base_unit", "'kg'"),
        ("base_unit", "''"),
        ("category", "'raw'"),
        ("category", "'PACKAGING'"),
        ("default_shelf_life_ms", "0"),
        ("default_shelf_life_ms", "-1"),
    ];
    for (column, value) in invalid {
        assert_sqlite_error(
            insert(&conn, "items", &with(item_row(ITEM3, "X"), column, value)),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }
    let valid = [
        ("g", "RAW", "NULL"),
        ("ml", "SEMI", "1"),
        ("pcs", "FINISHED", "259200000"),
    ];
    for (n, (base_unit, category, shelf_life)) in valid.into_iter().enumerate() {
        let id = format!("01890a5d-ac96-774b-bcce-b302099a851{n}");
        let row = with(
            with(
                with(
                    item_row(&id, &format!("V{n}")),
                    "base_unit",
                    &lit(base_unit),
                ),
                "category",
                &lit(category),
            ),
            "default_shelf_life_ms",
            shelf_life,
        );
        insert(&conn, "items", &row).unwrap();
    }
}

// domain「主数据」units：同一物料内 unit_code 唯一、非空，系数是正整数；item_id 引用物料，延迟到提交时检查。
#[test]
fn item_units_are_checked() {
    let (_dir, mut conn) = master_db();
    insert(&conn, "item_units", &item_unit_row(ITEM, "bag")).unwrap();
    assert_sqlite_error(
        insert(&conn, "item_units", &item_unit_row(ITEM, "bag")),
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
    );
    insert(&conn, "item_units", &item_unit_row(ITEM2, "bag")).unwrap();
    for (column, value) in [
        ("unit_code", "''"),
        ("base_qty_per_unit", "0"),
        ("base_qty_per_unit", "-1"),
    ] {
        assert_sqlite_error(
            insert(
                &conn,
                "item_units",
                &with(item_unit_row(ITEM, "box"), column, value),
            ),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }
    insert(&conn, "item_units", &item_unit_row(ITEM, "box")).unwrap();

    // 自动提交：语句结束即提交，立即拒绝。
    assert_sqlite_error(
        insert(&conn, "item_units", &item_unit_row(ITEM3, "bag")),
        ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
    );
    let tx = immediate(&mut conn).unwrap();
    insert(&tx, "item_units", &item_unit_row(ITEM3, "bag")).unwrap();
    assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    let tx = immediate(&mut conn).unwrap();
    insert(&tx, "item_units", &item_unit_row(ITEM3, "bag")).unwrap();
    insert(&tx, "items", &item_row(ITEM3, "BUTTER")).unwrap();
    tx.commit().unwrap();
    assert_eq!(count_rows(&conn, "item_units").unwrap(), 4);
}

// domain「主数据」RECIPE：output_item_id 引用物料，延迟到提交时检查。
#[test]
fn recipe_output_item_is_checked_at_commit() {
    let (_dir, mut conn) = master_db();
    let orphan = with(recipe_row(RECIPE, "R1"), "output_item_id", &lit(ITEM3));
    assert_sqlite_error(
        insert(&conn, "recipes", &orphan),
        ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
    );
    let tx = immediate(&mut conn).unwrap();
    insert(&tx, "recipes", &orphan).unwrap();
    assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    let tx = immediate(&mut conn).unwrap();
    insert(&tx, "recipes", &orphan).unwrap();
    insert(&tx, "items", &item_row(ITEM3, "BRIOCHE")).unwrap();
    tx.commit().unwrap();
    assert_eq!(count_rows(&conn, "recipes").unwrap(), 1);
}

// domain「主数据」versions：version 从 1 开始、在同一配方内唯一，产出数量是正整数；recipe_id 引用配方，延迟检查。
#[test]
fn recipe_versions_are_checked() {
    let (_dir, mut conn) = master_db();
    insert(&conn, "recipes", &recipe_row(RECIPE, "R1")).unwrap();
    insert(&conn, "recipe_versions", &recipe_version_row(RECIPE, 1)).unwrap();
    assert_sqlite_error(
        insert(&conn, "recipe_versions", &recipe_version_row(RECIPE, 1)),
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
    );
    for (column, value) in [
        ("version", "0"),
        ("version", "-1"),
        ("output_qty_per_batch", "0"),
        ("output_qty_per_batch", "-10"),
    ] {
        assert_sqlite_error(
            insert(
                &conn,
                "recipe_versions",
                &with(recipe_version_row(RECIPE, 2), column, value),
            ),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }
    insert(&conn, "recipe_versions", &recipe_version_row(RECIPE, 2)).unwrap();

    assert_sqlite_error(
        insert(&conn, "recipe_versions", &recipe_version_row(RECIPE2, 1)),
        ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
    );
    let tx = immediate(&mut conn).unwrap();
    insert(&tx, "recipe_versions", &recipe_version_row(RECIPE2, 1)).unwrap();
    assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    let tx = immediate(&mut conn).unwrap();
    insert(&tx, "recipe_versions", &recipe_version_row(RECIPE2, 1)).unwrap();
    insert(&tx, "recipes", &recipe_row(RECIPE2, "R2")).unwrap();
    tx.commit().unwrap();
    assert_eq!(count_rows(&conn, "recipe_versions").unwrap(), 3);
}

// domain「主数据」lines：同一版本内 item_id 不重复、line_no 从 0 起唯一，用量是正整数；
// (recipe_id, version) 引用配方版本，item_id 引用物料，都延迟到提交时检查。
#[test]
fn recipe_lines_are_checked() {
    let (_dir, mut conn) = master_db();
    insert(&conn, "recipes", &recipe_row(RECIPE, "R1")).unwrap();
    insert(&conn, "recipe_versions", &recipe_version_row(RECIPE, 1)).unwrap();
    insert(&conn, "recipe_versions", &recipe_version_row(RECIPE, 2)).unwrap();
    insert(&conn, "recipe_lines", &recipe_line_row(RECIPE, 1, 0, ITEM)).unwrap();
    assert_sqlite_error(
        insert(&conn, "recipe_lines", &recipe_line_row(RECIPE, 1, 0, ITEM2)),
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
    );
    assert_sqlite_error(
        insert(&conn, "recipe_lines", &recipe_line_row(RECIPE, 1, 1, ITEM)),
        ffi::SQLITE_CONSTRAINT_UNIQUE,
    );
    // 同一物料可以出现在另一个版本中。
    insert(&conn, "recipe_lines", &recipe_line_row(RECIPE, 2, 0, ITEM)).unwrap();
    for (column, value) in [
        ("line_no", "-1"),
        ("qty_per_batch", "0"),
        ("qty_per_batch", "-500"),
    ] {
        assert_sqlite_error(
            insert(
                &conn,
                "recipe_lines",
                &with(recipe_line_row(RECIPE, 1, 1, ITEM2), column, value),
            ),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }
    insert(&conn, "recipe_lines", &recipe_line_row(RECIPE, 1, 1, ITEM2)).unwrap();

    for orphan in [
        recipe_line_row(RECIPE, 3, 0, ITEM),
        recipe_line_row(RECIPE, 2, 1, ITEM3),
    ] {
        assert_sqlite_error(
            insert(&conn, "recipe_lines", &orphan),
            ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
        );
        let tx = immediate(&mut conn).unwrap();
        insert(&tx, "recipe_lines", &orphan).unwrap();
        assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    }
    let tx = immediate(&mut conn).unwrap();
    insert(&tx, "recipe_lines", &recipe_line_row(RECIPE, 3, 0, ITEM3)).unwrap();
    insert(&tx, "recipe_versions", &recipe_version_row(RECIPE, 3)).unwrap();
    insert(&tx, "items", &item_row(ITEM3, "BUTTER")).unwrap();
    tx.commit().unwrap();
    assert_eq!(count_rows(&conn, "recipe_lines").unwrap(), 4);
}

// domain「主数据」SUPPLIER：contact_phone 为 NULL（快照中省略）或非空文本。
#[test]
fn supplier_contact_phone_is_null_or_non_empty() {
    let (_dir, conn) = master_db();
    assert_sqlite_error(
        insert(
            &conn,
            "suppliers",
            &with(supplier_row(SUPPLIER, "S1"), "contact_phone", "''"),
        ),
        ffi::SQLITE_CONSTRAINT_CHECK,
    );
    insert(&conn, "suppliers", &supplier_row(SUPPLIER, "S1")).unwrap();
    insert(
        &conn,
        "suppliers",
        &with(
            supplier_row(SUPPLIER2, "S2"),
            "contact_phone",
            "'021-5555 0101 转 8'",
        ),
    )
    .unwrap();
}

// 007 + domain「批次」「投影表」：origin 只有 RECEIPT / PRODUCTION（盘点不新建批次）；source_line_no 从 0 开始；余量可以为负；
// (source_event_seq, source_line_no) 唯一；同一物料、同一批次日期内流水号唯一（改分类后类型段不同也不能重号），
// 不同物料或不同日期可以同号；manufacturer_lot_no 为 NULL 或非空；source_event_seq 立即引用账本中的事件，item_id 延迟引用物料。
#[test]
fn inventory_lots_are_checked() {
    let (_dir, mut conn) = master_db();
    for (n, origin) in ["RECEIPT", "PRODUCTION"].iter().enumerate() {
        let lot = format!("RAW-FLOUR-20261005-00{}", n + 1);
        let row = with(lot_row(&lot, 2, n as i64), "origin", &lit(origin));
        insert(&conn, "inventory_lots", &row).unwrap();
    }
    assert_sqlite_error(
        insert(&conn, "inventory_lots", &lot_row(LOT3, 2, 0)),
        ffi::SQLITE_CONSTRAINT_UNIQUE,
    );
    assert_sqlite_error(
        insert(
            &conn,
            "inventory_lots",
            &lot_row("SEMI-FLOUR-20261005-001", 3, 0),
        ),
        ffi::SQLITE_CONSTRAINT_UNIQUE,
    );
    assert_sqlite_error(
        insert(
            &conn,
            "inventory_lots",
            &with(lot_row(LOT, 3, 0), "item_id", &lit(ITEM2)),
        ),
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
    );
    insert(
        &conn,
        "inventory_lots",
        &with(
            lot_row("RAW-TOAST-20261005-001", 3, 0),
            "item_id",
            &lit(ITEM2),
        ),
    )
    .unwrap();
    insert(
        &conn,
        "inventory_lots",
        &lot_row("RAW-FLOUR-20261006-001", 3, 1),
    )
    .unwrap();
    for (column, value) in [
        ("origin", "'COUNT_GAIN'"),
        ("origin", "'receipt'"),
        ("origin", "'TRANSFER'"),
        ("source_line_no", "-1"),
        ("manufacturer_lot_no", "''"),
    ] {
        assert_sqlite_error(
            insert(
                &conn,
                "inventory_lots",
                &with(lot_row(LOT3, 1, 0), column, value),
            ),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }
    for seq in [0, 4] {
        assert_sqlite_error(
            insert(&conn, "inventory_lots", &lot_row(LOT3, seq, 0)),
            ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
        );
    }
    insert(
        &conn,
        "inventory_lots",
        &with(lot_row(LOT3, 1, 0), "remaining_qty", "-3"),
    )
    .unwrap();
    let full = with(
        with(
            with(
                lot_row("RAW-FLOUR-20261005-004", 1, 1),
                "remaining_qty",
                "0",
            ),
            "expires_at",
            &TS.to_string(),
        ),
        "manufacturer_lot_no",
        "'B-2026-10'",
    );
    insert(&conn, "inventory_lots", &full).unwrap();

    let orphan = with(
        lot_row("RAW-BUTTER-20261005-001", 3, 2),
        "item_id",
        &lit(ITEM3),
    );
    let tx = immediate(&mut conn).unwrap();
    insert(&tx, "inventory_lots", &orphan).unwrap();
    assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    let tx = immediate(&mut conn).unwrap();
    insert(&tx, "inventory_lots", &orphan).unwrap();
    insert(&tx, "items", &item_row(ITEM3, "BUTTER")).unwrap();
    tx.commit().unwrap();
    assert_eq!(count_rows(&conn, "inventory_lots").unwrap(), 7);
}

// 007 + domain「批次」批次号 <类型>-<编码>-<YYYYMMDD>-<流水号>：类型只有 RAW / SEMI / FINISHED（区分大小写）；
// 编码非空，只含 A–Z、0–9、_；末尾的日期和流水号等于 lot_date（真实日期）与三位补零的 lot_serial（1～999）。
// UUID 批次标识、首尾空白都被拒绝。
#[test]
fn lot_id_is_a_lot_number_matching_its_date_and_serial() {
    let (_dir, conn) = master_db();
    let lot_id = |text: &str| with(lot_row(LOT, 1, 0), "lot_id", &lit(text));
    let rejected: Vec<(&str, Row)> = vec![
        ("lowercase type", lot_row("raw-FLOUR-20261005-001", 1, 0)),
        ("unknown type", lot_row("PKG-FLOUR-20261005-001", 1, 0)),
        ("missing type", lot_row("FLOUR-20261005-001", 1, 0)),
        (
            "type without separator",
            lot_row("RAWFLOUR-20261005-001", 1, 0),
        ),
        ("empty code", lot_row("RAW--20261005-001", 1, 0)),
        ("lowercase code", lot_row("RAW-flour-20261005-001", 1, 0)),
        ("hyphen in code", lot_row("RAW-FL-OUR-20261005-001", 1, 0)),
        ("space in code", lot_row("RAW-FL OUR-20261005-001", 1, 0)),
        ("non-ASCII code", lot_row("RAW-面粉-20261005-001", 1, 0)),
        (
            "date differs",
            with(lot_row(LOT, 1, 0), "lot_date", "'2026-10-06'"),
        ),
        (
            "serial differs",
            with(lot_row(LOT, 1, 0), "lot_serial", "2"),
        ),
        ("serial 000", lot_row("RAW-FLOUR-20261005-000", 1, 0)),
        (
            "serial 1000",
            with(lot_row(LOT, 1, 0), "lot_serial", "1000"),
        ),
        ("not a real date", lot_row("RAW-FLOUR-20260230-001", 1, 0)),
        ("two-digit serial", lot_id("RAW-FLOUR-20261005-01")),
        ("four-digit serial", lot_id("RAW-FLOUR-20261005-0001")),
        ("dashed date", lot_id("RAW-FLOUR-2026-10-05-001")),
        ("leading space", lot_id(" RAW-FLOUR-20261005-001")),
        ("trailing space", lot_id("RAW-FLOUR-20261005-001 ")),
        ("uuid", lot_id(UUID_LOT)),
    ];
    for (name, row) in &rejected {
        match insert(&conn, "inventory_lots", row) {
            Err(rusqlite::Error::SqliteFailure(error, _)) => {
                assert_eq!(error.extended_code, ffi::SQLITE_CONSTRAINT_CHECK, "{name}")
            }
            other => panic!("{name}: expected a CHECK failure, got {other:?}"),
        }
    }
    for (n, lot) in [
        LOT,
        "SEMI-A_1-20261005-999",
        "FINISHED-_-99991231-010",
        "RAW-0-00010101-001",
    ]
    .iter()
    .enumerate()
    {
        insert(&conn, "inventory_lots", &lot_row(lot, 1, n as i64)).unwrap();
    }
}

// 007 + domain「批次」FIFO 与流水号：idx_inventory_lots_fifo 是 (item_id, lot_date, lot_serial) 上的唯一索引；
// 按 FIFO 顺序列出物料的批次、取同一物料同一日期的最大流水号，都用它查找，不排序。
#[test]
fn fifo_index_serves_allocation_and_serial_queries() {
    let (_dir, conn) = master_db();
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_index_info('idx_inventory_lots_fifo') ORDER BY seqno")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(columns, ["item_id", "lot_date", "lot_serial"]);
    let unique: i64 = conn
        .query_row(
            "SELECT \"unique\" FROM pragma_index_list('inventory_lots')
             WHERE name = 'idx_inventory_lots_fifo'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unique, 1);
    for sql in [
        "EXPLAIN QUERY PLAN
         SELECT lot_id FROM inventory_lots WHERE item_id = ?1 AND remaining_qty > 0
         ORDER BY lot_date, lot_serial",
        "EXPLAIN QUERY PLAN
         SELECT max(lot_serial) FROM inventory_lots WHERE item_id = ?1 AND lot_date = '2026-10-05'",
    ] {
        let plan: Vec<String> = conn
            .prepare(sql)
            .unwrap()
            .query_map([ITEM], |r| r.get(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(
            plan.iter()
                .any(|step| step.contains("INDEX idx_inventory_lots_fifo")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }
}

// domain「批次」账外缺口：每个物料至多一行，qty <= 0；item_id 延迟引用物料。
#[test]
fn inventory_unallocated_is_checked() {
    let (_dir, mut conn) = master_db();
    assert_sqlite_error(
        insert(&conn, "inventory_unallocated", &unallocated_row(ITEM, 1)),
        ffi::SQLITE_CONSTRAINT_CHECK,
    );
    insert(&conn, "inventory_unallocated", &unallocated_row(ITEM, -2)).unwrap();
    assert_sqlite_error(
        insert(&conn, "inventory_unallocated", &unallocated_row(ITEM, -3)),
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
    );
    insert(&conn, "inventory_unallocated", &unallocated_row(ITEM2, 0)).unwrap();

    let tx = immediate(&mut conn).unwrap();
    insert(&tx, "inventory_unallocated", &unallocated_row(ITEM3, -1)).unwrap();
    assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    assert_eq!(count_rows(&conn, "inventory_unallocated").unwrap(), 2);
}

// domain「投影表」流水：kind、alloc_source 枚举；(event_seq, movement_no) 唯一，movement_no 从 0 开始；营业日是真实日期；
// event_seq、absorbed_by_event_id 立即引用账本，lot_id、item_id 延迟引用批次和物料。
#[test]
fn inventory_movement_columns_are_checked() {
    let (_dir, mut conn) = inventory_db();
    let kinds = ["RECEIPT", "PRODUCE", "CONSUME", "WASTE", "ADJUST"];
    for (movement_no, kind) in kinds.iter().enumerate() {
        let row = with(movement_row(1, movement_no as i64), "kind", &lit(kind));
        insert(&conn, "inventory_movements", &row).unwrap();
    }
    assert_sqlite_error(
        insert(&conn, "inventory_movements", &movement_row(1, 0)),
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
    );
    for (column, value) in [
        ("kind", "'receipt'"),
        ("kind", "'TRANSFER'"),
        ("alloc_source", "'new_lot'"),
        ("alloc_source", "'MANUAL'"),
        ("movement_no", "-1"),
        ("business_date", "'2026-02-30'"),
        ("business_date", "'2026/10/05'"),
    ] {
        assert_sqlite_error(
            insert(
                &conn,
                "inventory_movements",
                &with(movement_row(2, 0), column, value),
            ),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }
    for (column, value) in [
        ("event_seq", "0"),
        ("event_seq", "4"),
        (
            "absorbed_by_event_id",
            "'01890a5d-ac96-774b-bcce-b302099a80ff'",
        ),
    ] {
        let row = if column == "absorbed_by_event_id" {
            with(absorbed_row(2, 0), column, value)
        } else {
            with(movement_row(2, 0), column, value)
        };
        assert_sqlite_error(
            insert(&conn, "inventory_movements", &row),
            ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
        );
    }
    for (column, value) in [("lot_id", lit(LOT2)), ("item_id", lit(ITEM3))] {
        let tx = immediate(&mut conn).unwrap();
        insert(
            &tx,
            "inventory_movements",
            &with(movement_row(2, 0), column, &value),
        )
        .unwrap();
        assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    }
    assert_eq!(count_rows(&conn, "inventory_movements").unwrap(), 5);
}

// domain「盘点吸收」「投影表」：未被吸收的行 qty_delta = nominal_qty、alloc_source 不是 ABSORBED；
// 被吸收的行带 absorbed_by_event_id、alloc_source = ABSORBED、qty_delta = 0、lot_id 为 NULL。
// 指定 / FIFO / 新建批次的行必须带 lot_id，账外缺口（SHORTFALL）的行不带。
#[test]
fn inventory_movement_absorption_and_lot_rules_are_checked() {
    let (_dir, conn) = inventory_db();
    let rejected: Vec<Row> = vec![
        with(movement_row(2, 0), "qty_delta", "9"),
        with(movement_row(2, 0), "alloc_source", "'ABSORBED'"),
        with(movement_row(2, 0), "absorbed_by_event_id", &lit(EVT)),
        with(absorbed_row(2, 0), "alloc_source", "'FIFO'"),
        with(absorbed_row(2, 0), "absorbed_by_event_id", "NULL"),
        with(absorbed_row(2, 0), "qty_delta", "-3"),
        with(absorbed_row(2, 0), "lot_id", &lit(LOT)),
        with(movement_row(2, 0), "lot_id", "NULL"),
        with(
            with(movement_row(2, 0), "alloc_source", "'SPECIFIED'"),
            "lot_id",
            "NULL",
        ),
        with(
            with(movement_row(2, 0), "alloc_source", "'FIFO'"),
            "lot_id",
            "NULL",
        ),
        with(movement_row(2, 0), "alloc_source", "'SHORTFALL'"),
    ];
    for row in &rejected {
        assert_sqlite_error(
            insert(&conn, "inventory_movements", row),
            ffi::SQLITE_CONSTRAINT_CHECK,
        );
    }

    let negative = |row: Row| with(with(row, "nominal_qty", "-4"), "qty_delta", "-4");
    let accepted: Vec<Row> = vec![
        absorbed_row(2, 0),
        negative(with(movement_row(2, 1), "alloc_source", "'SPECIFIED'")),
        negative(with(movement_row(2, 2), "alloc_source", "'FIFO'")),
        negative(with(
            with(movement_row(2, 3), "alloc_source", "'SHORTFALL'"),
            "lot_id",
            "NULL",
        )),
        with(movement_row(2, 4), "alloc_source", "'CORRECTION'"),
        with(
            with(movement_row(2, 5), "alloc_source", "'REVERSAL'"),
            "lot_id",
            "NULL",
        ),
        with(
            with(movement_row(2, 6), "alloc_source", "'COUNT'"),
            "lot_id",
            "NULL",
        ),
        with(movement_row(2, 7), "alloc_source", "'COUNT'"),
    ];
    for row in &accepted {
        insert(&conn, "inventory_movements", row).unwrap();
    }
    assert_eq!(count_rows(&conn, "inventory_movements").unwrap(), 8);
}

// domain「盘点」盘点投影：每次盘点的每个物料至多一行；counted_qty 非负，book_qty 可以为负（含账外缺口）；
// event_seq 立即引用账本，item_id 延迟引用物料。
#[test]
fn inventory_counts_are_checked() {
    let (_dir, mut conn) = master_db();
    insert(&conn, "inventory_counts", &count_row(1, ITEM)).unwrap();
    assert_sqlite_error(
        insert(&conn, "inventory_counts", &count_row(1, ITEM)),
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
    );
    insert(&conn, "inventory_counts", &count_row(2, ITEM)).unwrap();
    assert_sqlite_error(
        insert(
            &conn,
            "inventory_counts",
            &with(count_row(1, ITEM2), "counted_qty", "-1"),
        ),
        ffi::SQLITE_CONSTRAINT_CHECK,
    );
    insert(
        &conn,
        "inventory_counts",
        &with(
            with(count_row(1, ITEM2), "book_qty", "-2"),
            "counted_qty",
            "0",
        ),
    )
    .unwrap();
    assert_sqlite_error(
        insert(&conn, "inventory_counts", &count_row(4, ITEM)),
        ffi::SQLITE_CONSTRAINT_FOREIGNKEY,
    );
    let tx = immediate(&mut conn).unwrap();
    insert(&tx, "inventory_counts", &count_row(3, ITEM3)).unwrap();
    assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    assert_eq!(count_rows(&conn, "inventory_counts").unwrap(), 3);
}

// domain「盘点吸收」：吸收判定按 (item_id, observed_at, event_seq) 查找最早的盘点，索引覆盖这三列，不排序。
#[test]
fn count_index_serves_the_absorption_query() {
    let (_dir, conn) = master_db();
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_index_info('idx_inventory_counts_item') ORDER BY seqno")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(columns, ["item_id", "observed_at", "event_seq"]);
    let plan: Vec<String> = conn
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT e.id FROM inventory_counts c JOIN store_events e ON e.seq = c.event_seq
             WHERE c.item_id = ?1 AND c.observed_at > ?2
             ORDER BY c.observed_at, c.event_seq LIMIT 1",
        )
        .unwrap()
        .query_map(params![ITEM, TS], |r| r.get(3))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        plan.iter().any(|step| step.starts_with("SEARCH c USING")
            && step.contains("INDEX idx_inventory_counts_item")),
        "{plan:?}"
    );
    assert!(
        !plan.iter().any(|step| step.contains("TEMP B-TREE")),
        "{plan:?}"
    );
}

// domain「批次」「投影表」：账面数（视图 inventory_on_hand）= 批次余量之和（含负余量）+ 账外缺口；每个物料一行，没有库存为 0。
// 迁移 007 重建了这个视图。
#[test]
fn inventory_on_hand_sums_lots_and_shortfall() {
    let (_dir, conn) = inventory_db();
    insert(&conn, "items", &item_row(ITEM3, "BUTTER")).unwrap();
    insert(
        &conn,
        "inventory_lots",
        &with(lot_row(LOT2, 2, 0), "remaining_qty", "7"),
    )
    .unwrap();
    insert(
        &conn,
        "inventory_lots",
        &with(lot_row(LOT3, 2, 1), "remaining_qty", "-3"),
    )
    .unwrap();
    insert(
        &conn,
        "inventory_lots",
        &with(
            lot_row("RAW-FLOUR-20261005-004", 2, 2),
            "remaining_qty",
            "0",
        ),
    )
    .unwrap();
    insert(&conn, "inventory_unallocated", &unallocated_row(ITEM, -2)).unwrap();
    insert(&conn, "inventory_unallocated", &unallocated_row(ITEM2, -5)).unwrap();

    let on_hand: Vec<(String, i64)> = conn
        .prepare("SELECT item_id, qty FROM inventory_on_hand ORDER BY item_id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        on_hand,
        [(ITEM.into(), 12), (ITEM2.into(), -5), (ITEM3.into(), 0)]
    );
}

// AGENTS「SQLite」所有表用 STRICT：005 各表的 INTEGER 列不接受非整数文本或小数。
#[test]
fn tables_005_are_strict() {
    let (_dir, conn) = inventory_db();
    let cases: [(&str, Row, &[&str]); 11] = [
        (
            "items",
            item_row(ITEM3, "X"),
            &["default_shelf_life_ms", "active", "revision"],
        ),
        (
            "item_units",
            item_unit_row(ITEM, "bag"),
            &["base_qty_per_unit"],
        ),
        ("recipes", recipe_row(RECIPE, "X"), &["active", "revision"]),
        (
            "recipe_versions",
            recipe_version_row(RECIPE, 1),
            &["version", "output_qty_per_batch"],
        ),
        (
            "recipe_lines",
            recipe_line_row(RECIPE, 1, 0, ITEM),
            &["version", "line_no", "qty_per_batch"],
        ),
        (
            "suppliers",
            supplier_row(SUPPLIER, "X"),
            &["active", "revision"],
        ),
        (
            "waste_reasons",
            waste_reason_row(REASON, "X"),
            &["active", "revision"],
        ),
        (
            "inventory_lots",
            lot_row(LOT2, 2, 0),
            &[
                "lot_serial",
                "source_event_seq",
                "source_line_no",
                "remaining_qty",
                "expires_at",
            ],
        ),
        ("inventory_unallocated", unallocated_row(ITEM, -1), &["qty"]),
        (
            "inventory_movements",
            movement_row(1, 0),
            &[
                "event_seq",
                "movement_no",
                "nominal_qty",
                "qty_delta",
                "physical_at",
            ],
        ),
        (
            "inventory_counts",
            count_row(1, ITEM),
            &["event_seq", "observed_at", "book_qty", "counted_qty"],
        ),
    ];
    for (table, row, columns) in &cases {
        for column in *columns {
            for value in ["'many'", "2.5"] {
                assert_sqlite_error(
                    insert(&conn, table, &with(row.clone(), column, value)),
                    ffi::SQLITE_CONSTRAINT_DATATYPE,
                );
            }
        }
    }
}

// AGENTS「只追加」：005 的表都是投影，没有只追加触发器；重建在一个事务内按任意顺序清空再重写，提交时引用完整即可。
// 只删物料、留下引用它的行，则在提交时拒绝。
#[test]
fn tables_005_can_be_cleared_and_rewritten() {
    let (_dir, mut conn) = inventory_db();
    let rows: Vec<(&str, Row)> = vec![
        ("item_units", item_unit_row(ITEM, "bag")),
        ("recipes", recipe_row(RECIPE, "R1")),
        ("recipe_versions", recipe_version_row(RECIPE, 1)),
        ("recipe_lines", recipe_line_row(RECIPE, 1, 0, ITEM)),
        ("suppliers", supplier_row(SUPPLIER, "S1")),
        ("waste_reasons", waste_reason_row(REASON, "EXPIRED")),
        ("inventory_unallocated", unallocated_row(ITEM, -2)),
        ("inventory_movements", movement_row(1, 0)),
        ("inventory_counts", count_row(3, ITEM)),
    ];
    for (table, row) in &rows {
        insert(&conn, table, row).unwrap();
    }
    for table in TABLES_005 {
        assert!(
            conn.execute(&format!("UPDATE {table} SET rowid = rowid"), [])
                .unwrap()
                >= 1,
            "{table}"
        );
    }

    let tx = immediate(&mut conn).unwrap();
    tx.execute("DELETE FROM items", []).unwrap();
    assert_sqlite_error(tx.commit(), ffi::SQLITE_CONSTRAINT_FOREIGNKEY);
    assert_eq!(count_rows(&conn, "items").unwrap(), 2);

    let tx = immediate(&mut conn).unwrap();
    for table in TABLES_005 {
        tx.execute(&format!("DELETE FROM {table}"), []).unwrap();
    }
    for (table, row) in rows.iter().rev() {
        insert(&tx, table, row).unwrap();
    }
    insert(&tx, "inventory_lots", &lot_row(LOT, 1, 0)).unwrap();
    insert(&tx, "items", &item_row(ITEM2, "TOAST")).unwrap();
    insert(&tx, "items", &item_row(ITEM, "FLOUR")).unwrap();
    tx.commit().unwrap();
    for table in TABLES_005 {
        assert!(count_rows(&conn, table).unwrap() >= 1, "{table}");
    }
}

// ---- 007：批次号 ----

const MIGRATIONS_001_TO_006: [&str; 6] = [
    MIGRATION_001,
    MIGRATION_002,
    MIGRATION_003,
    MIGRATION_004,
    MIGRATION_005,
    MIGRATION_006,
];
/// 收货 golden 样本 GOODS_RECEIVED@1：迁移 007 之前在线写入的 @1 收货，批次标识是 UUID。
const GOODS_RECEIVED_V1: &str =
    include_str!("../../boh-app/tests/golden/GOODS_RECEIVED@1/WITH_MANUFACTURER_LOT_NO.json");
/// 上面样本中的物料 ID 与批次 ID。
const GOODS_RECEIVED_V1_ITEM: &str = ITEM;
const GOODS_RECEIVED_V1_LOT: &str = "01890a5d-ac96-774b-bcce-b302099a8701";
const INSERT_RECEIPT_EVENT: &str =
    "INSERT INTO store_events (id, event_type, schema_version, aggregate_type, aggregate_id,
         aggregate_version, command_id, actor_id, device_id, business_date,
         occurred_at, recorded_at, payload)
     VALUES (?1, 'GOODS_RECEIVED', ?2, ?3, ?4, ?5, ?6, ?7, 'device-1', ?8, ?9, ?10, ?11)";

/// 迁移会改动的全部内容：表结构（含视图、索引）与各表的行。
fn whole_database(conn: &Connection) -> rusqlite::Result<Vec<Vec<rusqlite::types::Value>>> {
    let mut statement = conn.prepare(
        "SELECT type, name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\'
         ORDER BY name",
    )?;
    let columns = statement.column_count();
    let mut content: Vec<Vec<rusqlite::types::Value>> = statement
        .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())?
        .collect::<rusqlite::Result<_>>()?;
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for table in tables {
        content.extend(table_rows(conn, &table)?);
    }
    Ok(content)
}

/// 错误及其全部 source 的文本，用 " | " 连接。
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(" | ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

/// 迁移到 006 的库：门店身份、一条命令、seq 1 的报损事件（schema_version 1）、物料 FLOUR。
#[allow(clippy::unwrap_used)] // 测试夹具：前置数据写入失败时测试无法开始，直接终止。
fn version_6_db() -> (TempDir, Connection) {
    let (dir, conn) = db_at_version(&MIGRATIONS_001_TO_006);
    insert_meta(&conn, 1, STORE).unwrap();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    insert(&conn, "items", &item_row(ITEM, "FLOUR")).unwrap();
    (dir, conn)
}

// AGENTS「迁移」+ 007：账本中没有 @1 收货、物料编码都合规的 006 库升级到 007：原有表逐行不变（含其他事件类型的
// schema_version 1 事件、账外缺口、盘点投影）；inventory_lots 与 inventory_movements 重建为新结构且为空，
// 视图 inventory_on_hand 照常按物料给出账面数。编码 A_1、0、_ 都合规。
#[test]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
fn upgrades_a_version_6_database_rebuilding_the_lot_tables() {
    let (_dir, mut conn) = version_6_db();
    for (id, code) in [(ITEM2, "A_1"), (ITEM3, "0")] {
        insert(&conn, "items", &item_row(id, code)).unwrap();
    }
    insert(
        &conn,
        "items",
        &item_row("01890a5d-ac96-774b-bcce-b302099a8504", "_"),
    )
    .unwrap();
    insert_equipment(&conn, EQUIPMENT, "F1", "Walk-in", "FREEZER", 1, 1).unwrap();
    Reading::new(READING, 1).insert(&conn).unwrap();
    insert(&conn, "inventory_unallocated", &unallocated_row(ITEM, -2)).unwrap();
    insert(&conn, "inventory_counts", &count_row(1, ITEM)).unwrap();
    let kept = [
        "store_meta",
        "processed_commands",
        "store_events",
        "equipment",
        "temperature_readings",
        "items",
        "inventory_unallocated",
        "inventory_counts",
    ];
    let before: Vec<_> = kept.iter().map(|t| table_rows(&conn, t).unwrap()).collect();

    migrate(&mut conn).unwrap();

    assert_eq!(schema_version(&conn).unwrap(), 7);
    let after: Vec<_> = kept.iter().map(|t| table_rows(&conn, t).unwrap()).collect();
    assert_eq!(after, before);
    assert_eq!(count_rows(&conn, "inventory_lots").unwrap(), 0);
    assert_eq!(count_rows(&conn, "inventory_movements").unwrap(), 0);
    conn.prepare(
        "SELECT lot_id, item_id, lot_date, lot_serial, origin, source_event_seq, source_line_no,
                remaining_qty, expires_at, manufacturer_lot_no FROM inventory_lots",
    )
    .unwrap();
    let on_hand: i64 = conn
        .query_row(
            "SELECT qty FROM inventory_on_hand WHERE item_id = ?1",
            [ITEM],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(on_hand, -2);
    assert_eq!(count_rows(&conn, "inventory_on_hand").unwrap(), 4);
    insert(&conn, "inventory_lots", &lot_row(LOT, 1, 0)).unwrap();
    insert(&conn, "inventory_movements", &movement_row(1, 0)).unwrap();
}

// 007 + domain「收货接口」：账本中有 GOODS_RECEIVED@1（golden 样本中的 @1 收货）时迁移 007 报错，错误信息指出 @1 收货；
// 迁移整体回滚：user_version 仍为 6，表结构和全部行（含 UUID 批次及其流水）不变，没有遗留临时表。
// 节点经 boh_storage::open() 迁移，所以拒绝启动。
#[test]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
fn migration_007_refuses_a_ledger_with_goods_received_v1() {
    let (_dir, mut conn) = version_6_db();
    assert_eq!(GOODS_RECEIVED_V1_ITEM, ITEM);
    let receipt = Event {
        aggregate_type: "RECEIPT",
        payload: GOODS_RECEIVED_V1.trim_end(),
        ..Event::new(EVT2)
    };
    receipt.execute_insert(&conn, INSERT_RECEIPT_EVENT).unwrap();
    insert(
        &conn,
        "inventory_lots",
        &with(
            uuid_lot_row(GOODS_RECEIVED_V1_LOT, 0, "manufacturer_lot_no"),
            "source_event_seq",
            "2",
        ),
    )
    .unwrap();
    insert(
        &conn,
        "inventory_movements",
        &with(movement_row(2, 0), "lot_id", &lit(GOODS_RECEIVED_V1_LOT)),
    )
    .unwrap();
    let before = whole_database(&conn).unwrap();

    let error = migrate(&mut conn).expect_err("migration 007 must refuse GOODS_RECEIVED@1");

    assert!(
        error_chain(&error).contains("GOODS_RECEIVED@1"),
        "{}",
        error_chain(&error)
    );
    assert_eq!(schema_version(&conn).unwrap(), 6);
    assert_eq!(whole_database(&conn).unwrap(), before);
    assert_eq!(count_rows(&conn, "sqlite_temp_master").unwrap(), 0);
}

// 007 + domain「主数据」：items 中有不合规的物料编码（小写、-、空格、中文、尾部空格）时迁移 007 报错，错误信息指出 items.code；
// 迁移整体回滚，user_version 仍为 6，表结构和全部行不变。
#[test]
#[allow(clippy::disallowed_methods)] // 锁定测试经 boh_storage::testing 取得原始连接。
fn migration_007_refuses_non_conforming_item_codes() {
    for code in ["flour", "FL-OUR", "FL OUR", "面粉", "FLOUR2 "] {
        let (_dir, mut conn) = version_6_db();
        insert(&conn, "items", &item_row(ITEM2, code)).unwrap();
        let before = whole_database(&conn).unwrap();

        let error = migrate(&mut conn).expect_err("migration 007 must refuse the item code");

        assert!(
            error_chain(&error).contains("items.code"),
            "{code}: {}",
            error_chain(&error)
        );
        assert_eq!(schema_version(&conn).unwrap(), 6, "{code}");
        assert_eq!(whole_database(&conn).unwrap(), before, "{code}");
    }
}
