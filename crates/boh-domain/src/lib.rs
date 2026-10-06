//! 纯领域类型与校验不变量。
//!
//! 本 crate 禁止依赖数据库、HTTP 或任何系统 I/O（包括读取系统时钟）。

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod time;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    #[error("invalid UUIDv7: {0}")]
    InvalidId(String),
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
        #[serde(try_from = "Uuid", into = "Uuid")]
        pub struct $name(Uuid);

        impl $name {
            pub fn from_uuid(id: Uuid) -> Result<Self, DomainError> {
                if id.get_version_num() == 7 {
                    Ok(Self(id))
                } else {
                    Err(DomainError::InvalidId(id.to_string()))
                }
            }

            pub fn parse(s: &str) -> Result<Self, DomainError> {
                let id = Uuid::parse_str(s).map_err(|_| DomainError::InvalidId(s.to_owned()))?;
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
    }

    #[test]
    fn serde_enforces_v7() {
        let ok: CommandId = serde_json::from_str(&format!("\"{V7}\"")).unwrap();
        assert_eq!(ok.to_string(), V7);
        assert!(serde_json::from_str::<CommandId>(&format!("\"{V4}\"")).is_err());
    }
}
