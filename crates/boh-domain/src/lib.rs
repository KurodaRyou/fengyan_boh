//! 纯领域类型与校验不变量。
//!
//! 本 crate 禁止依赖数据库、HTTP 或任何系统 I/O（包括读取系统时钟）。

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod equipment;
pub mod master_data;
pub mod temperature;
pub mod time;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    #[error("invalid UUIDv7: {0}")]
    InvalidId(String),
    #[error("invalid value for {0}")]
    InvalidField(&'static str),
}

/// UTC Unix 毫秒时间戳。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixMillis(pub i64);

/// 定义一个只接受 UUIDv7 的 ID 新类型。
macro_rules! uuid_v7_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "Uuid")]
        pub struct $name(Uuid);

        impl $name {
            /// Pure UUIDv7 construction: the caller supplies time and entropy.
            pub fn from_parts(time: UnixMillis, entropy: [u8; 10]) -> Result<Self, DomainError> {
                // UUID timestamps are unsigned; pre-epoch clocks use epoch for the ID only.
                let millis = u64::try_from(time.0.max(0))
                    .map_err(|_| DomainError::InvalidField("id_timestamp"))?;
                if millis > 0xffff_ffff_ffff {
                    return Err(DomainError::InvalidField("id_timestamp"));
                }
                Ok(Self(uuid::Builder::from_unix_timestamp_millis(millis, &entropy).into_uuid()))
            }

            pub fn from_uuid(id: Uuid) -> Result<Self, DomainError> {
                if id.get_version_num() == 7 && id.get_variant() == uuid::Variant::RFC4122 {
                    Ok(Self(id))
                } else {
                    Err(DomainError::InvalidId(id.to_string()))
                }
            }

            pub fn parse(s: &str) -> Result<Self, DomainError> {
                let id = Uuid::parse_str(s).map_err(|_| DomainError::InvalidId(s.to_owned()))?;
                if id.to_string() != s {
                    return Err(DomainError::InvalidId(s.to_owned()));
                }
                Self::from_uuid(id)
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl TryFrom<Uuid> for $name {
            type Error = DomainError;

            fn try_from(id: Uuid) -> Result<Self, DomainError> {
                Self::from_uuid(id)
            }
        }

        impl TryFrom<String> for $name {
            type Error = DomainError;

            fn try_from(text: String) -> Result<Self, DomainError> {
                Self::parse(&text)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Uuid {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

uuid_v7_id!(
    /// 门店 ID（`store_meta.store_id`）。
    StoreId
);
uuid_v7_id!(
    /// 每次备份的文件标识。
    BackupId
);
uuid_v7_id!(
    /// 事件 ID（`store_events.id`）。
    EventId
);
uuid_v7_id!(
    /// 客户端生成的命令幂等键（`processed_commands.command_id`）。
    CommandId
);
uuid_v7_id!(
    /// 聚合 ID（`store_events.aggregate_id`）。
    AggregateId
);

#[cfg(test)]
mod tests {
    use super::*;

    const V7: &str = "01890a5d-ac96-774b-bcce-b302099a8057";
    const V4: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn accepts_v7() {
        let id = CommandId::parse(V7).unwrap();
        assert_eq!(id.to_string(), V7);
    }

    #[test]
    fn rejects_non_v7_and_garbage() {
        assert!(CommandId::parse(V4).is_err());
        assert!(CommandId::parse("not-a-uuid").is_err());
        assert!(CommandId::parse("01890a5d-ac96-774b-0cce-b302099a8057").is_err());
    }

    #[test]
    fn builds_ids_from_explicit_time_and_entropy() {
        let first = EventId::from_parts(UnixMillis(1_791_248_400_000), [1; 10]).unwrap();
        let second = EventId::from_parts(UnixMillis(1_791_248_400_000), [2; 10]).unwrap();
        assert_ne!(first, second);
        assert_eq!(first.as_uuid().get_version_num(), 7);
        assert_eq!(first.as_uuid().get_variant(), uuid::Variant::RFC4122);
        assert_eq!(
            first.as_uuid().get_timestamp().unwrap().to_unix().0,
            1_791_248_400
        );
        assert!(EventId::from_parts(UnixMillis(-1), [0; 10]).is_ok());
        assert!(EventId::from_parts(UnixMillis(i64::MAX), [0; 10]).is_err());
    }

    #[test]
    fn serde_enforces_v7() {
        let ok: CommandId = serde_json::from_str(&format!("\"{V7}\"")).unwrap();
        assert_eq!(ok.to_string(), V7);
        assert!(serde_json::from_str::<CommandId>(&format!("\"{V4}\"")).is_err());
    }

    #[test]
    fn every_id_parser_and_deserializer_requires_canonical_text() {
        for text in [
            V7.to_uppercase(),
            V7.replace('-', ""),
            format!("{{{V7}}}"),
            format!("urn:uuid:{V7}"),
        ] {
            let json = serde_json::to_string(&text).unwrap();
            assert!(CommandId::parse(&text).is_err());
            assert!(AggregateId::parse(&text).is_err());
            assert!(StoreId::parse(&text).is_err());
            assert!(EventId::parse(&text).is_err());
            assert!(BackupId::parse(&text).is_err());
            assert!(serde_json::from_str::<CommandId>(&json).is_err());
            assert!(serde_json::from_str::<AggregateId>(&json).is_err());
            assert!(serde_json::from_str::<StoreId>(&json).is_err());
            assert!(serde_json::from_str::<EventId>(&json).is_err());
            assert!(serde_json::from_str::<BackupId>(&json).is_err());
        }
    }
}
