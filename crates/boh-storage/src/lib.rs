//! SQLite 存储层：连接初始化、迁移、单写线程、只读连接池。

mod connection;
mod error;
pub mod ledger;
mod master_data;
mod migrate;
mod projections;
mod readers;
pub mod store;
mod waste;
mod writer;

pub mod backup;
pub mod clock;
#[doc(hidden)]
pub mod testing;

use std::num::NonZeroUsize;
use std::path::Path;

pub use error::{BackupStage, Diagnostic, StorageError};
pub use migrate::{LATEST_SCHEMA_VERSION, schema_version};
pub use readers::Readers;
pub use writer::{Writer, WriterHandle};

pub use rusqlite;

pub struct Storage {
    pub writer: Writer,
    pub writer_handle: WriterHandle,
    pub readers: Readers,
}

/// 一个进程对同一数据库只调用一次；迁移和读池成功后才启动写线程。
pub fn open(path: &Path, reader_pool_size: NonZeroUsize) -> Result<Storage, StorageError> {
    let mut conn = connection::open_writer(path)?;
    migrate::migrate(&mut conn)?;
    let readers = Readers::open(path, reader_pool_size.get())?;
    let (writer, writer_handle) = writer::spawn_writer(conn)?;
    Ok(Storage {
        writer,
        writer_handle,
        readers,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    // Check each object boundary and field, including nested array elements.
    // Duplicate fields stay in raw JSON so no Value conversion can erase them.
    pub(crate) fn assert_strict_payload<T>(
        decode: impl Fn(&str) -> Result<T, StorageError>,
        good: &str,
        optional: &[&str],
        operation: &'static str,
    ) {
        assert!(decode(good).is_ok(), "{good}");
        match decode("{") {
            Err(StorageError::External {
                operation: actual,
                source,
                ..
            }) => {
                assert_eq!(actual, operation);
                assert!(source.downcast_ref::<serde_json::Error>().is_some());
            }
            _ => panic!("decoder must preserve the parse error and its operation"),
        }
        for json in ["", "null", "true", "1", "\"text\"", "[]"] {
            assert!(decode(json).is_err(), "{json}");
        }
        assert!(decode(&format!("{good}{good}")).is_err());
        let value: serde_json::Value = serde_json::from_str(good).unwrap();
        let json = value.to_string();
        check_object_fields(&decode, &json, &value, optional);
    }

    fn check_object_fields<T>(
        decode: &impl Fn(&str) -> Result<T, StorageError>,
        json: &str,
        value: &serde_json::Value,
        optional: &[&str],
    ) {
        use serde_json::{Value, json};

        match value {
            Value::Object(fields) => {
                let object = value.to_string();
                let unknown = format!("{{\"unknown\":true,{}", &object[1..]);
                let bad = json.replacen(&object, &unknown, 1);
                assert!(decode(&bad).is_err(), "{bad}");
                for (key, field) in fields {
                    let mut missing = fields.clone();
                    missing.remove(key);
                    let changed = json.replacen(&object, &Value::Object(missing).to_string(), 1);
                    assert_eq!(
                        decode(&changed).is_ok(),
                        optional.contains(&key.as_str()),
                        "{changed}"
                    );
                    let duplicate = format!("{{{}:{field},{}", json!(key), &object[1..]);
                    let bad = json.replacen(&object, &duplicate, 1);
                    assert!(decode(&bad).is_err(), "{bad}");
                    let mut wrong_types = vec![Value::Null];
                    match field {
                        Value::Number(number) => {
                            wrong_types.extend([json!(true), json!(number.to_string())]);
                            wrong_types.push(serde_json::from_str(&format!("{number}.0")).unwrap());
                        }
                        Value::Bool(_) => wrong_types.extend([json!(1), json!("true")]),
                        Value::String(text) => wrong_types.extend([
                            json!(1),
                            json!(true),
                            json!({ text: null }),
                            json!([text]),
                        ]),
                        Value::Array(_) => wrong_types.extend([json!({}), json!("array")]),
                        Value::Object(_) => wrong_types.extend([json!([]), json!("object")]),
                        Value::Null => panic!("valid payloads cannot contain null"),
                    }
                    for wrong in wrong_types {
                        let mut changed = fields.clone();
                        changed.insert(key.clone(), wrong);
                        let bad = json.replacen(&object, &Value::Object(changed).to_string(), 1);
                        assert!(decode(&bad).is_err(), "{bad}");
                    }
                    check_object_fields(decode, json, field, optional);
                }
            }
            Value::Array(elements) => {
                for element in elements {
                    check_object_fields(decode, json, element, optional);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn open_rejects_newer_schema_without_migrating() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("boh.db");
        let conn = connection::open_writer(&path).unwrap();
        let future = LATEST_SCHEMA_VERSION + 1;
        conn.pragma_update(None, "user_version", future).unwrap();
        drop(conn);
        assert!(matches!(
            open(&path, NonZeroUsize::new(1).unwrap()),
            Err(StorageError::UnsupportedSchemaVersion { found, supported })
                if found == future && supported == LATEST_SCHEMA_VERSION
        ));
        let conn = connection::open_reader(&path).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), future);
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = 'table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }
}
