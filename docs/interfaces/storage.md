# storage 稳定测试接口

本文件属于 [锁定测试接口契约](../interfaces.md)，签名、可见性与行为约束均为锁定测试的依据。

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
  - 有读事务未结束、等满 `busy_timeout` 仍无法截断 WAL 时，记 `warn` 并返回 `Ok`；之后重新 `open` 能读到全部已提交的数据。其他错误照常返回 `Err`。
- `Readers::call`：借出一个只读连接，在一个 DEFERRED 读事务中执行 `f`，`f` 内的读取看到同一个快照；在 `spawn_blocking` 上运行。
- `StorageError` 的变体由实现决定；锁定测试只依赖 `UnsupportedSchemaVersion { found: i64, supported: i64 }`。
  - 迁移 SQL 执行失败时，错误的 `source()` 链保留 SQLite 的原始错误；锁定测试按链上的文本核对迁移中约束的名字（如迁移 007 的前置检查）。
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
- 本期没有写入口的事件（如 `source = HQ_PACKAGE` 的 `MASTER_DATA_CHANGED`、带被吸收行的 `WASTE_LOGGED`），锁定测试经 `open_writer` 直接写入 `processed_commands` 和 `store_events`（不写 `seq` 列），再 `rebuild_projections` 核对投影。
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
    pub async fn sleep(&self, duration: Duration);
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
- `sleep`：按单调时间等待 `duration`，不受墙钟跳变影响；`duration` 为 0 时立即返回。`Clock::system()` 用 tokio 的单调计时。必须在 tokio 运行时内调用。
  - 节点校时的每个服务器的查询超时（5 秒）、每 1 分钟一次的判断和「距上次核对满 1 小时」都经它计时（[门店节点校时](../business/time.md#门店节点校时)）。
  - 「满 1 小时」按单调时间判断，不按判断的次数累计：`ManualClock` 一次推进 1 小时之后，下一次判断就核对。
- `ManualClock` 供测试使用：由它得到的 `Clock` 共享同一个时间，不随真实时间流动。
  - 它有两个读数：墙钟（`now()` 返回的值）和单调时间。`set` 只改墙钟，可以回拨，模拟墙钟跳变；`advance` 把两者向前推进同样的时长。
  - 墙钟到达 `deadline` 后，正在等待的 `sleep_until` 立即返回；自调用 `sleep` 起单调时间累计推进达到 `duration` 后，正在等待的 `sleep` 立即返回。
- HTTP 黑盒测试：把 `ManualClock::clock()` 交给 `test_router`，服务端读到的当前时间（如 `recorded_at`）就是 `ManualClock` 设定的值。
- 备份触发测试经 `boh_app::test_node` 进行：调度任务按 `now()` 算出下一个触发时刻（UTC 整点或 `closing_backup_time`），时钟回拨后重算（[触发](../operations/backup.md#触发)）。
  - 测试把时间设到触发前 1ms，断言未触发；推进 1ms，断言触发。
  - 排队用例：用 `TestNode::hold_backups` 让备份停在执行中，再推进到下一个触发时刻。
