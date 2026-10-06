//! 阶段 0.4：锁定终版第 5 节的 001；每项测试注明对应的设计条款。

use std::fmt::Debug;

use boh_storage::rusqlite::{self, Connection, TransactionBehavior, ffi, params};
use boh_storage::testing::{migrate, open_reader, open_writer};
use boh_storage::{LATEST_SCHEMA_VERSION, StorageError, schema_version};
use tempfile::TempDir;

const MIGRATION_001: &str = include_str!("../../../migrations/001_initial_schema.sql");
const MIGRATION_001_FNV1A_64: u64 = 0xfdd0_ce57_922a_6b5f;
const STORE: &str = "01890a5d-ac96-774b-bcce-b302099a8050";
const ACTOR: &str = "01890a5d-ac96-774b-bcce-b302099a8051";
const CMD: &str = "01890a5d-ac96-774b-bcce-b302099a8057";
const EVT: &str = "01890a5d-ac96-774b-bcce-b302099a8058";
const EVT2: &str = "01890a5d-ac96-774b-bcce-b302099a8059";
const EVT3: &str = "01890a5d-ac96-774b-bcce-b302099a805a";
const TS: i64 = 1_791_158_400_000;
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

// 4.2「迁移不可修改」：按 UTF-8 原始字节锁定第 5 节完整 SQL（包括注释）。
#[test]
fn migration_001_matches_frozen_checksum() {
    let hash = MIGRATION_001
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    assert_eq!(hash, MIGRATION_001_FNV1A_64);
}

// 5 + 保留的迁移契约：重复执行迁移不改变版本，也不丢失已有数据。
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

// 5 + 保留的迁移契约：未知的新版本必须拒绝，不能向下覆盖。
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

// 4.2「禁止用 REPLACE 绕过触发器」：写、读连接都启用 recursive_triggers。
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

// 5「门店身份（单行）」：唯一可用的 id 为 1。
#[test]
fn store_meta_accepts_only_id_one() {
    let (_dir, conn) = fresh_db();
    for id in [-1, 0, 2] {
        assert_sqlite_error(insert_meta(&conn, id, STORE), ffi::SQLITE_CONSTRAINT_CHECK);
    }
    insert_meta(&conn, 1, STORE).unwrap();
}

// 5 + 附录「store_meta 写入第二行」：显式重复键和自动分配第二行都被拒绝。
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

// 5 的 store_meta 保护触发器；4.2 和附录的 REPLACE 防绕过要求。
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

// 5 的 store_id UUIDv7 CHECK。
#[test]
fn store_id_rejects_v4_uppercase_and_wrong_length() {
    let (_dir, conn) = fresh_db();
    for invalid in INVALID_IDS {
        assert_sqlite_error(insert_meta(&conn, 1, invalid), ffi::SQLITE_CONSTRAINT_CHECK);
    }
    insert_meta(&conn, 1, STORE).unwrap();
}

// 5 的 processed_commands.command_id UUIDv7 CHECK。
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

// 5 的 store_events.id / aggregate_id / actor_id UUIDv7 CHECK。
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

// 5：事件的 command_id 通过延迟外键引用已经校验 UUIDv7 的命令键。
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

// 5：request 必须是 JSON 对象，数组、标量和非法 JSON 都不能成为规范化请求。
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

// 5：payload 必须是 JSON 对象；附录记录数组 payload 被拒绝。
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

// 5：response 的 JSON 有效性约束。
#[test]
fn response_must_be_valid_json() {
    let (_dir, conn) = fresh_db();
    assert_sqlite_error(conn.execute(
        "INSERT INTO processed_commands (command_id, command_type, request, response, recorded_at)
         VALUES (?1, 'waste.log', '{}', '{invalid', ?2)",
        params![CMD, TS],
    ), ffi::SQLITE_CONSTRAINT_CHECK);
}

// 5 + 附录：营业日必须格式正确且真实存在；非法日期不能因 NULL 而绕过 CHECK。
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

// 5「DEFERRABLE INITIALLY DEFERRED」：同一事务允许先事件、后命令。
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

// 5 + 附录：命令始终不存在时在提交阶段拒绝，并回滚事件。
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

// 3.1 + 5：省略 seq，由 SQLite 从 1 连续分配。
#[test]
fn automatically_assigned_seq_is_contiguous() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    for id in [EVT, EVT2, EVT3] {
        Event::new(id).insert(&conn).unwrap();
    }
    assert_eq!(event_seqs(&conn).unwrap(), [1, 2, 3]);
}

// 3.1 + 附录：包含多个事件的事务回滚后，下一个 seq 仍是已提交的 max + 1。
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

// 3.1 + 5 的连续性触发器 + 附录显式 seq = 10 的反例。
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

// 5：command_id 主键唯一。
#[test]
fn duplicate_command_id_is_rejected() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    assert_sqlite_error(
        insert_command(&conn, CMD, "{}"),
        ffi::SQLITE_CONSTRAINT_PRIMARYKEY,
    );
}

// 5：外部事件 id 唯一，不能因采用 seq 主键而失去 UUID 去重约束。
#[test]
fn duplicate_event_id_is_rejected() {
    let (_dir, conn) = fresh_db();
    insert_command(&conn, CMD, "{}").unwrap();
    Event::new(EVT).insert(&conn).unwrap();
    let mut duplicate = Event::new(EVT);
    duplicate.aggregate_id = EVT2;
    assert_sqlite_error(duplicate.insert(&conn), ffi::SQLITE_CONSTRAINT_UNIQUE);
}

// 5：UNIQUE(aggregate_type, aggregate_id, aggregate_version) 三列共同生效。
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

// 5：schema_version 和 aggregate_version 都从 1 开始。
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

// 5 的三张 STRICT 表：非数字文本不能存入各自的 INTEGER 时间列。
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

// 4.2 + 5 + 附录：UPDATE / DELETE / REPLACE / UPSERT 都必须由账本触发器拒绝。
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

// 4.2 + 5 + 附录：覆盖 seq、事件 id 和聚合版本三个冲突入口，避免只测到唯一约束。
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
