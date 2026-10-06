# 锁定测试使用的接口

锁定测试只通过本文件列出的接口访问系统（见 [AGENTS.md](../AGENTS.md)「测试分工」）。签名变更必须以 `docs:` 提交更新本文件。
时间与营业日规则见 [domain.md](domain.md)「时间」。

## boh-storage

### 公开 API

```rust
pub fn open(path: &Path, reader_pool_size: NonZeroUsize) -> Result<Storage, StorageError>;

pub struct Storage {
    pub writer: Writer,
    pub writer_handle: WriterHandle,
    pub readers: Readers,
}

#[derive(Clone)]
pub struct Writer { /* 私有 */ }
impl Writer {
    pub async fn call<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<StorageError> + Send + 'static;
    pub async fn rebuild_projections(&self) -> Result<u64, StorageError>;
}

pub struct WriterHandle { /* 私有 */ }
impl WriterHandle {
    pub async fn shutdown(self) -> Result<(), StorageError>;
}

#[derive(Clone)]
pub struct Readers { /* 私有 */ }
impl Readers {
    pub async fn call<T, E, F>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<StorageError> + Send + 'static;
}

pub const LATEST_SCHEMA_VERSION: i64;
pub fn schema_version(conn: &Connection) -> Result<i64, StorageError>;

pub enum StorageError { /* thiserror */ }

pub mod clock;                   // 见「时钟」
#[doc(hidden)] pub mod testing;  // 见「boh_storage::testing」
pub use rusqlite;
```

- `open`：打开数据库（不存在则创建）→ 打开唯一写连接 → 在写连接上迁移到 `LATEST_SCHEMA_VERSION`，每个迁移一个事务 → 打开 `reader_pool_size` 个读连接 → 启动 `sqlite-writer` 线程。一个进程对同一数据库只调用一次。
- 数据库的 `user_version` 高于 `LATEST_SCHEMA_VERSION`：返回 `StorageError::UnsupportedSchemaVersion { found, supported }`，不迁移、不启动写线程，`boh-server` 拒绝启动。
- `Writer::call`：在写线程上以 `BEGIN IMMEDIATE` 执行 `f`，`Ok` 提交、`Err` 回滚；单个任务超过 50ms 记 `warn`。写线程已停止时返回 `StorageError::WriterClosed`。
- `Writer::rebuild_projections`：在写线程上的一个写事务中清空全部投影表，再按 `seq` 升序对每个事件调用在线写入使用的同一个 `projections::apply`，提交后返回重放的事件数。失败时回滚，投影不变。
  - 投影表 = `sqlite_master` 中除 `sqlite_` 开头的内部表、`store_meta`、`processed_commands`、`store_events` 和认证状态表以外的全部表。
  - `boh-server rebuild-projections <配置文件>` 经 `boh_storage::open()` 调用它，只在服务停止时运行。
- `WriterHandle::shutdown`：处理完已入队的任务后停止写线程，在写连接上执行 `wal_checkpoint(TRUNCATE)`，然后关闭写连接。
- `Readers::call`：借出一个只读连接，在一个 DEFERRED 读事务中执行 `f`，`f` 内的读取看到同一个快照；在 `spawn_blocking` 上运行。
- `StorageError` 的变体由实现决定；锁定测试只依赖 `UnsupportedSchemaVersion { found: i64, supported: i64 }`。
- `open_writer`、`open_reader`、`migrate`、`spawn_writer`、`checkpoint_truncate`、`Readers::open` 都是 `pub(crate)`。

### boh_storage::testing

```rust
#[doc(hidden)]
pub mod testing {
    pub fn open_writer(path: &Path) -> Result<Connection, StorageError>;
    pub fn open_reader(path: &Path) -> Result<Connection, StorageError>;
    pub fn migrate(conn: &mut Connection) -> Result<(), StorageError>;
    pub fn rebuild_projections(conn: &mut Connection) -> Result<u64, StorageError>;
}
```

- `open_writer`：读写连接（文件不存在则创建），已设好全部 PRAGMA，不迁移。
- `open_reader`：只读连接，已设好全部 PRAGMA 和 `query_only = ON`。
- `migrate`：与 `open` 内部相同的迁移。
- `rebuild_projections`：在 `conn` 上以 `BEGIN IMMEDIATE` 执行与 `Writer::rebuild_projections` 相同的重建。`conn` 须由 `open_writer` 打开。
- 这些函数都是转发到 crate 内部函数的独立函数，不用 `pub use`。
  - clippy 按函数定义禁用；重导出会让 crate 内部对原函数的调用也被禁用。
- 两份 `clippy.toml` 禁用这些函数，只有锁定测试及 `spec_support/` 可以 `#[allow]`。

### 时钟（`boh_storage::clock`）

```rust
#[derive(Clone, Debug)]
pub struct Clock { /* 私有 */ }
impl Clock {
    pub fn system() -> Clock;
    pub fn now(&self) -> UnixMillis;
    pub async fn sleep_until(&self, deadline: UnixMillis);
}

#[derive(Clone, Debug)]
pub struct ManualClock { /* 私有 */ }
impl ManualClock {
    pub fn new(start: UnixMillis) -> ManualClock;
    pub fn clock(&self) -> Clock;
    pub fn set(&self, now: UnixMillis);
    pub fn advance(&self, by: Duration);
}
```

- `UnixMillis` 即 `boh_domain::UnixMillis`。生产只用 `Clock::system()`；需要当前时间的组件接收一个 `Clock`，自己不读系统时间。
- `now`：UTC Unix 毫秒，系统时钟取原值，不做单调钳制。
- `sleep_until`：`now() >= deadline` 时才返回，`deadline` 已过时立即返回；系统时间回拨时随之推迟。必须在 tokio 运行时内调用。
- `ManualClock` 供测试使用：由它得到的 `Clock` 共享同一个时间，不随真实时间流动。`set` 可以回拨，`advance` 只向前；时间到达 `deadline` 后，正在等待的 `sleep_until` 立即返回。
- HTTP 黑盒测试：把 `ManualClock::clock()` 交给 `test_router`，服务端读到的当前时间（如 `recorded_at`）就是 `ManualClock` 设定的值。
- 备份触发测试：调度任务用 `now()` 算出下一个触发时刻（整点或 `closing_backup_time`），再 `sleep_until`。
  - 测试把时间设到触发前 1ms，断言未触发；推进 1ms，断言触发。
  - 排队用例：在备份执行期间推进到下一个触发时刻。

## boh-domain：时间（`boh_domain::time`）

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureTimes {
    pub captured_at: UnixMillis,
    pub sent_at: UnixMillis,
    pub started_captured_at: Option<UnixMillis>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Calibrated {
    pub occurred_at: UnixMillis,
    pub started_at: Option<UnixMillis>,
    pub capture_time_adjusted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    CaptureTooOld,
    InvalidProductionTime,
    OutOfRange,
}

pub fn calibrate(times: CaptureTimes, recorded_at: UnixMillis) -> Result<Calibrated, TimeError>;

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

`calibrate` 按以下顺序判定，先命中的结果生效：

1. 带 `started_captured_at` 且它晚于 `captured_at`：`InvalidProductionTime`。
2. `lag = sent_at − captured_at`；带开始时间时另算 `start_lag = sent_at − started_captured_at`。溢出：`OutOfRange`。
3. 任一 lag 大于 72 小时（259 200 000 ms）：`CaptureTooOld`；恰好 72 小时照常受理。
4. 负的 lag 按 0 处理，此时 `capture_time_adjusted = true`。
5. `occurred_at = recorded_at − lag`，`started_at = recorded_at − start_lag`。溢出：`OutOfRange`。

| 结果 | HTTP |
|---|---|
| `TimeError::CaptureTooOld` | `400 CAPTURE_TOO_OLD` |
| `TimeError::InvalidProductionTime` | `400 INVALID_PRODUCTION_TIME` |
| `TimeError::OutOfRange` | `400 VALIDATION_FAILED` |
| `capture_time_adjusted = true` | 警告 `CAPTURE_TIME_ADJUSTED` |

- `business_date`：把 `occurred_at` 换算成 `timezone` 的当地时间，当地时刻早于 `cutoff` 归前一日。`occurred_at` 无法换算，或结果年份超出 0000–9999：`OutOfRange`。
- `parse_timezone`：接受 IANA 时区名，只在 jiff 内置时区库（`jiff::tz::TimeZoneDatabase::bundled()`）中查找；查不到，或得到的是 jiff 的未知时区（`Etc/Unknown`），都为 `InvalidTimeZone`。
  - jiff 查找 `Etc/Unknown` 不报错，返回一个行为同 UTC 的未知时区。
  - 不用 `jiff::tz::db()`：它优先读门店机的 zoneinfo，开发机和门店机可能得到不同的结果。
- `parse_business_day_cutoff`：只接受 `HH:MM`，两位小时 `00`–`23`、两位分钟 `00`–`59`，其他一律为 `InvalidBusinessDayCutoff`。

## boh-app：Router 测试入口

```rust
#[doc(hidden)]
pub fn test_router(db_path: &Path, clock: Clock) -> Result<Router, StorageError>;
```

- 在 `db_path` 上调用 `boh_storage::open()`，以 `clock` 作为服务端时钟，构造与生产入口路由相同的 `axum::Router`。
- 不读环境变量和配置文件，不监听端口；测试直接用这个 `Router` 处理请求。使用固定的测试配置：

  | 配置项 | 值 |
  |---|---|
  | `store_id` | `01890a5d-ac96-774b-bcce-b302099a8050` |
  | `timezone` | `Asia/Shanghai` |
  | `business_day_cutoff` | `04:00` |
  | `dev_actor_stub` | `true`（身份由请求头提供，见 domain.md「员工认证」开发桩） |

- `store_meta` 为空时，用与 `boh-server init` 相同的代码写入上表的 `store_id`，`created_at` 取 `clock.now()`；已存在且 `store_id` 不同时返回 `Err`。
- 写线程不经 `shutdown`：`Router` 及其全部克隆释放后，写线程自行退出。
- 两份 `clippy.toml` 禁用 `test_router`，只有锁定测试及 `spec_support/` 可以 `#[allow]`。
