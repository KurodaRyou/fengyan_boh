#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("backup: {0}")]
    Backup(String),
    #[error("domain: {0}")]
    Domain(#[from] boh_domain::DomainError),

    #[error("time configuration: {0}")]
    TimeConfiguration(#[from] boh_domain::time::ConfigError),

    #[error("invalid or unsupported event: {0}")]
    InvalidEvent(String),

    #[error("store is not initialized")]
    StoreNotInitialized,

    #[error("store is already initialized")]
    StoreAlreadyInitialized,

    #[error("database belongs to a different store")]
    StoreMismatch,
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

    #[error("WAL checkpoint could not truncate because the database is busy")]
    CheckpointBusy,

    #[error("reader pool invariant violated: {0}")]
    ReaderPoolInvariant(&'static str),

    #[error("failed to spawn writer thread: {0}")]
    Spawn(#[source] std::io::Error),

    #[error("blocking task failed: {0}")]
    Join(String),
}

#[cfg(test)]
mod tests {
    use super::StorageError;
    use boh_domain::time::{ConfigError, parse_business_day_cutoff, parse_timezone};

    #[test]
    fn time_configuration_errors_preserve_their_cause() {
        for (error, cause) in [
            (
                parse_timezone("Asia/Shangai").unwrap_err(),
                ConfigError::InvalidTimeZone("Asia/Shangai".into()),
            ),
            (
                parse_business_day_cutoff("24:00").unwrap_err(),
                ConfigError::InvalidBusinessDayCutoff("24:00".into()),
            ),
        ] {
            let error = StorageError::from(error);
            assert_eq!(error.to_string(), format!("time configuration: {cause}"));
            assert!(matches!(error, StorageError::TimeConfiguration(actual) if actual == cause));
        }
    }
}
