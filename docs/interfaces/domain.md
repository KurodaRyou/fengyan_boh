# domain 稳定测试接口

本文件属于 [锁定测试接口契约](../interfaces.md)，签名、可见性与行为约束均为锁定测试的依据。

## boh-domain：时间（`boh_domain::time`）

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureTimes {
    pub occurred_at: UnixMillis,
    pub started_at: Option<UnixMillis>,
    pub clock_synced_at: UnixMillis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    ClientClockNotSynced,
    CaptureTooOld,
    InvalidProductionTime,
    OutOfRange,
}

pub fn check_capture_times(times: CaptureTimes, recorded_at: UnixMillis) -> Result<(), TimeError>;
pub fn clock_signal_valid(last_synced_at: UnixMillis, now: UnixMillis) -> bool;

#[derive(Debug, Clone)]
pub struct StoreTimeZone { /* 私有 */ }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusinessDayCutoff { /* 私有 */ }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessDate { /* 私有 */ }  // Display 输出 'YYYY-MM-DD'

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    InvalidTimeZone(String),
    InvalidBusinessDayCutoff(String),
}

pub fn parse_timezone(value: &str) -> Result<StoreTimeZone, ConfigError>;
pub fn parse_business_day_cutoff(value: &str) -> Result<BusinessDayCutoff, ConfigError>;
pub fn business_date(
    occurred_at: UnixMillis,
    timezone: &StoreTimeZone,
    cutoff: BusinessDayCutoff,
) -> Result<BusinessDate, TimeError>;
```

`check_capture_times` 实现 [发生时间的校验](../business/time.md#发生时间的校验)，用于实时录入的写命令（`recorded_at` 是写事务内的节点时间）。比较按数学上的整数进行，不因溢出而改变结果：

| 条件（任一成立） | 结果 |
|---|---|
| `clock_synced_at > recorded_at + 60 000`，或 `recorded_at > clock_synced_at + 604 800 000` | `ClientClockNotSynced` |
| `occurred_at > recorded_at + 60 000`，或带 `started_at` 且 `started_at > recorded_at + 60 000` | `ClientClockNotSynced` |
| `recorded_at − occurred_at > 259 200 000`，或带 `started_at` 且 `recorded_at − started_at > 259 200 000` | `CaptureTooOld` |
| 带 `started_at` 且 `started_at > occurred_at` | `InvalidProductionTime` |

- 都不成立时返回 `Ok(())`；边界值（恰好 1 分钟、7 天、72 小时，`started_at = occurred_at`）照常受理。同时命中多行时返回哪一个不作规定（[写命令通用规则](../engineering/http.md#写命令通用规则)业务校验）。不返回 `OutOfRange`。
- `clock_signal_valid`：`last_synced_at ≤ now ≤ last_synced_at + 604 800 000` 时为 `true`（[校时信号](../business/time.md#校时信号)），比较同样不因溢出而改变结果。节点信号用它判定；前端信号不用它（允许领先 1 分钟）。

| 结果 | HTTP |
|---|---|
| `TimeError::ClientClockNotSynced` | `400 CLIENT_CLOCK_NOT_SYNCED` |
| `TimeError::CaptureTooOld` | `400 CAPTURE_TOO_OLD` |
| `TimeError::InvalidProductionTime` | `400 INVALID_PRODUCTION_TIME` |
| `TimeError::OutOfRange` | `400 VALIDATION_FAILED` |

- `business_date`：把 `occurred_at` 换算成 `timezone` 的当地时间，当地时刻早于 `cutoff` 归前一日。`occurred_at` 无法换算，或结果年份超出 0000–9999：`OutOfRange`。
- `parse_timezone`：接受 IANA 时区名，只在 jiff 内置时区库（`jiff::tz::TimeZoneDatabase::bundled()`）中查找；查不到，或得到的是 jiff 的未知时区（`Etc/Unknown`），都为 `InvalidTimeZone`。
  - jiff 查找 `Etc/Unknown` 不报错，返回一个行为同 UTC 的未知时区。
  - 不用 `jiff::tz::db()`：它优先读门店机的 zoneinfo，开发机和门店机可能得到不同的结果。
- `parse_business_day_cutoff`：只接受 `HH:MM`，两位小时 `00`–`23`、两位分钟 `00`–`59`，其他一律为 `InvalidBusinessDayCutoff`。

### 闭店备份时刻

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosingBackupTime { /* 私有 */ }  // Display 输出 'HH:MM'

// ConfigError 增加变体：
//     InvalidClosingBackupTime(String),

pub fn parse_closing_backup_time(value: &str) -> Result<ClosingBackupTime, ConfigError>;
pub fn next_closing_backup(
    after: UnixMillis,
    timezone: &StoreTimeZone,
    time: ClosingBackupTime,
) -> Result<UnixMillis, TimeError>;
```

- `parse_closing_backup_time`：格式规则同 `parse_business_day_cutoff`，非法时为 `InvalidClosingBackupTime`（携带原值）。
- `next_closing_backup`：返回严格晚于 `after` 的第一个「`timezone` 当地日期 + `time`」时刻。
  - 当地不存在的时刻（夏令时跳过）按跳过之前的偏移换算，即顺延跳过的时长（跳过 02:00–03:00 时，02:30 为 03:30）。
  - 当地重复的时刻只取第一次（较早的那个）；同一天的第二次不算触发时刻。
  - 结果或换算超出可表示范围：`OutOfRange`。

## boh-domain：报损 payload（`boh_domain::waste`）

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WasteLogged { /* 字段由实现决定 */ }
```

- `WASTE_LOGGED@1` 的 payload 类型；写入与 `projections::apply` 都用它序列化和解析。结构见 [事件目录](../domain.md#事件目录)、[「报损接口」](../business/operations.md#报损接口)。
- 锁定测试对 `golden/WASTE_LOGGED@1/` 的每份样本做 `serde_json::from_str::<WasteLogged>` 再 `serde_json::to_string`，结果必须与样本逐字节相同（含被吸收的分支：写入路径随盘点切片才产生它们）。
- 解析拒绝[报损接口](../business/operations.md#报损接口)规定以外的结构：未知字段、`alloc` 与 `absorbed_by_event_id` 同时出现或都不出现、`lot_id` 与 `lot_book_qty` 只出现一个、数量或换算系数写成 JSON 数字（[数量](../business/inventory.md#数量)：payload 中是小数字符串）。
