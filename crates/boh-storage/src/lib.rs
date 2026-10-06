//! SQLite 存储层：连接初始化、迁移、单写线程、只读连接池。

mod connection;
mod error;
mod migrate;
mod readers;
mod writer;

pub use connection::{checkpoint_truncate, open_reader, open_writer};
pub use error::StorageError;
pub use migrate::{LATEST_SCHEMA_VERSION, migrate, schema_version};
pub use readers::Readers;
pub use writer::{Writer, WriterHandle, spawn_writer};

pub use rusqlite;
