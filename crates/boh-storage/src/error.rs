use std::error::Error;
use std::fmt;
use std::panic::Location;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupStage {
    Generate,
    Verify,
    FileSync,
    AllocateNumber,
    Rename,
    DirectorySync,
    Worker,
    Schedule,
}

impl fmt::Display for BackupStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Generate => "生成",
            Self::Verify => "校验",
            Self::FileSync => "文件 fsync",
            Self::AllocateNumber => "分配备份序号",
            Self::Rename => "重命名",
            Self::DirectorySync => "目录 fsync",
            Self::Worker => "备份任务",
            Self::Schedule => "计算备份触发时间",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("{operation}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
        location: &'static Location<'static>,
    },

    #[error("backup stage {stage} (number {})", .number.map_or_else(|| "unassigned".to_owned(), |number| number.to_string()))]
    Backup {
        stage: BackupStage,
        number: Option<u64>,
        #[source]
        source: Box<dyn Error + Send + Sync>,
        location: &'static Location<'static>,
    },

    #[error("rebuild event seq={seq} event_type={event_type} schema_version={schema_version}")]
    Rebuild {
        seq: i64,
        event_type: String,
        schema_version: i64,
        #[source]
        source: Box<dyn Error + Send + Sync>,
        location: &'static Location<'static>,
    },

    #[error("{operation}")]
    External {
        operation: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
        location: &'static Location<'static>,
    },

    #[error("domain: {0}")]
    Domain(#[from] boh_domain::DomainError),

    #[error("time configuration: {0}")]
    TimeConfiguration(#[from] boh_domain::time::ConfigError),

    #[error("invalid or unsupported event: {0}")]
    InvalidEvent(String),

    #[error("{0}")]
    Message(&'static str),

    #[error("store is not initialized")]
    StoreNotInitialized,

    #[error("store is already initialized")]
    StoreAlreadyInitialized,

    #[error("database belongs to a different store")]
    StoreMismatch,

    #[error("{operation}")]
    Sqlite {
        operation: &'static str,
        #[source]
        source: rusqlite::Error,
        location: &'static Location<'static>,
    },

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

    #[error("启动写线程")]
    Spawn {
        #[source]
        source: std::io::Error,
        location: &'static Location<'static>,
    },

    #[error("{operation}")]
    Join {
        operation: &'static str,
        #[source]
        source: tokio::task::JoinError,
        location: &'static Location<'static>,
    },
}

impl StorageError {
    #[track_caller]
    pub fn sqlite(operation: &'static str, source: rusqlite::Error) -> Self {
        Self::Sqlite {
            operation,
            source,
            location: Location::caller(),
        }
    }

    #[track_caller]
    pub fn io(operation: &'static str, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            source,
            location: Location::caller(),
        }
    }

    #[track_caller]
    pub fn spawn(source: std::io::Error) -> Self {
        Self::Spawn {
            source,
            location: Location::caller(),
        }
    }

    #[track_caller]
    pub fn join(operation: &'static str, source: tokio::task::JoinError) -> Self {
        Self::Join {
            operation,
            source,
            location: Location::caller(),
        }
    }

    #[track_caller]
    pub fn external(operation: &'static str, source: impl Error + Send + Sync + 'static) -> Self {
        Self::External {
            operation,
            source: Box::new(source),
            location: Location::caller(),
        }
    }

    #[track_caller]
    pub fn backup(stage: BackupStage, number: Option<u64>, source: StorageError) -> Self {
        Self::Backup {
            stage,
            number,
            source: Box::new(source),
            location: Location::caller(),
        }
    }

    #[track_caller]
    pub fn rebuild(
        seq: i64,
        event_type: String,
        schema_version: i64,
        source: StorageError,
    ) -> Self {
        Self::Rebuild {
            seq,
            event_type,
            schema_version,
            source: Box::new(source),
            location: Location::caller(),
        }
    }

    pub fn location(&self) -> Option<&'static Location<'static>> {
        match self {
            Self::Io { location, .. }
            | Self::Sqlite { location, .. }
            | Self::Join { location, .. }
            | Self::External { location, .. }
            | Self::Spawn { location, .. }
            | Self::Backup { location, .. }
            | Self::Rebuild { location, .. } => Some(location),
            _ => None,
        }
    }
}

/// Formats every cause and its captured conversion site without using Debug,
/// which could expose request values embedded in unrelated structures.
pub struct Diagnostic<'a>(pub &'a (dyn Error + 'static));

impl fmt::Display for Diagnostic<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut cause = Some(self.0);
        let mut first = true;
        while let Some(error) = cause {
            if !first {
                f.write_str("; caused by: ")?;
            }
            first = false;
            write!(f, "{error}")?;
            if let Some(location) = error
                .downcast_ref::<StorageError>()
                .and_then(StorageError::location)
            {
                write!(f, " at {location}")?;
            }
            cause = error.source();
        }
        Ok(())
    }
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
