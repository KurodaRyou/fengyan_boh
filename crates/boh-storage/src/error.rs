#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("failed to enable WAL, journal_mode is {0}")]
    WalNotEnabled(String),

    #[error("database schema version {found} is not supported (latest known: {supported})")]
    UnsupportedSchemaVersion { found: i64, supported: i64 },

    #[error("writer thread is closed")]
    WriterClosed,

    #[error("writer thread panicked")]
    WriterPanicked,

    #[error("reader pool is closed")]
    ReadersClosed,

    #[error("failed to spawn writer thread: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("blocking task failed: {0}")]
    Join(String),
}
