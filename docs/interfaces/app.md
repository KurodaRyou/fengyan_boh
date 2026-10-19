# app 稳定测试接口

本文件属于 [锁定测试接口契约](../interfaces.md)，签名、可见性与行为约束均为锁定测试的依据。

## boh-app：Router 测试入口

```rust
#[doc(hidden)]
pub fn test_router(
    db_path: &Path,
    clock: Clock,
    clock_synced_at: Option<UnixMillis>,
) -> Result<Router, StorageError>;
```

- 在 `db_path` 上调用 `boh_storage::open()`，以 `clock` 作为服务端时钟，构造与生产入口路由相同的 `axum::Router`。
- `clock_synced_at`：节点的上次校时时间，相当于启动时从状态文件读到的值；`None` 表示没有信号。
  - 信号是否有效每次按 `clock.now()` 判定（`clock_signal_valid`），`/health` 的 `clock_synced_at` 就是这个值。
  - 不读写状态文件，不做 NTP 核对：信号在 `Router` 的生命期内不变。
- 不读环境变量和配置文件，不监听端口；测试直接用这个 `Router` 处理请求。使用固定的测试配置：

  | 配置项 | 值 |
  |---|---|
  | `store_id` | `01890a5d-ac96-774b-bcce-b302099a8050` |
  | `timezone` | `Asia/Shanghai` |
  | `business_day_cutoff` | `04:00` |
  | `dev_actor_stub` | `true`（身份由请求头提供，见 [开发桩](../business/auth.md#开发桩)） |

- `store_meta` 为空时，用与 `boh-server init` 写 `store_meta` 相同的代码写入上表的 `store_id`，`created_at` 取 `clock.now()`；已存在且 `store_id` 不同时返回 `Err`。
  - 不写预置主数据（报损原因），也不写 `processed_commands`：测试节点的账本从空开始。
- 写线程不经 `shutdown`：`Router` 及其全部克隆释放后，写线程自行退出。
- 不启动备份调度、不变量自检、节点校时核对等后台任务：`/health` 的备份字段和 `invariant_check` 的两个字段一直为 `null`。
- 两份 `clippy.toml` 禁用 `test_router`，只有锁定测试及 `spec_support/` 可以 `#[allow]`。

## boh-app：带后台任务的测试节点

```rust
#[doc(hidden)]
pub struct TestNodeConfig {
    pub backup_dir: PathBuf,
    pub backup_keep_count: NonZeroU32,
    pub timezone: String,             // IANA 时区名
    pub closing_backup_time: String,  // 'HH:MM'
    pub ntp_servers: Vec<String>,     // 与配置项 ntp_servers 相同的写法
}

#[doc(hidden)]
pub async fn test_node(db_path: &Path, clock: Clock, config: TestNodeConfig) -> Result<TestNode, StorageError>;

#[doc(hidden)]
pub struct TestNode {
    pub router: Router,
    /* 私有 */
}
impl TestNode {
    pub fn hold_backups(&self) -> BackupHold;
    pub fn writer(&self) -> Writer;
    pub async fn run_invariant_check(&self);
    pub fn hold_invariant_checks(&self) -> InvariantCheckHold;
    pub async fn wait_clock_checks(&self, count: u64);
    pub async fn wait_clock_ticks(&self, count: u64);
    pub async fn shutdown(self) -> Result<(), StorageError>;
}

#[doc(hidden)]
pub struct BackupHold { /* 私有 */ }
impl BackupHold {
    pub async fn started(&self);
}

#[doc(hidden)]
pub struct InvariantCheckHold { /* 私有 */ }
impl InvariantCheckHold {
    pub async fn started(&self);
}
```

- 在 tokio 运行时内调用。与生产启动相同：`boh_storage::open()`；`store_meta` 处理同 `test_router`；创建 `backup_dir`（含上级目录），失败或不是目录时返回 `Err`；启动备份调度。
  - 配置：`store_id`、`business_day_cutoff`、开发桩同 `test_router`；`timezone` 同时用于营业日和闭店备份；时区、`closing_backup_time` 或 `ntp_servers`（规则同配置项，见[「boh-server：配置」](server.md#boh-server配置)）非法时返回 `Err`。
  - 节点校时与生产相同（[门店节点校时](../business/time.md#门店节点校时)）：启动时读取状态文件 `<db_path>-clock-sync` 作为初始信号，按 `ntp_servers` 核对 `clock`（`Clock::now` 作为 `sntpc` 的本机时间，超时和间隔经 `Clock::sleep`），成功后按规定写状态文件。
  - 返回时，调度任务已按 `clock` 当时的时间算好第一个触发时刻，启动检查已提交到检查入口（不等待它完成），状态文件已读取，启动时的核对已开始（不等待它完成）。
- `router`：与生产入口相同的路由；`/health` 反映本节点的备份与自检结果。
- `hold_backups`：返回的句柄存在期间，每次开始的备份在取得开始时间（文件名中的时间）并读取 `seq_before` 之后、执行 `VACUUM INTO` 之前等待；所有句柄释放后继续。
  - `started()`：有备份正在等待该句柄时返回（已在等待则立即返回）。
  - 句柄不借用 `TestNode`：持有句柄时也可以调用 `shutdown`，`shutdown` 会等到句柄释放、队列执行完。
- `writer`：本节点唯一写连接的 `Writer`（克隆）。锁定测试只用它在节点运行中修改投影表，制造或修复违规；不另开写连接。
- 启动检查和闭店检查的结果，锁定测试轮询 `/health` 的 `invariant_check.last_attempt` 等待。
- `run_invariant_check`：向检查入口提交一次检查，等到这次检查的结果（含 `failed`）交给 `/health` 后返回。入口中已有检查时排在它们之后。
- `hold_invariant_checks`：返回的句柄存在期间，每次检查在入口中开始执行之后、开启读快照之前等待；所有句柄释放后继续。
  - `started()`：有检查正在等待时返回（已在等待则立即返回）。
  - 句柄不借用 `TestNode`，规则同 `hold_backups`；`shutdown` 会等到句柄释放、已提交的检查执行完。
  - 暂停机制只在 `boh-app` 内部，不对外提供其他入口。
- `wait_clock_checks`：等到本节点启动以来完成的核对次数（含启动时的那次）不少于 `count` 后返回。一次核对按 [一次核对](../business/time.md#门店节点校时)查询一轮服务器；完成指结果已发布（含状态文件的写入或删除，成功或失败），并且已开始等待下一次判断（已调用 `Clock::sleep`）。
- `wait_clock_ticks`：等到启动以来完成的每分钟判断次数（不含启动时的核对）不少于 `count` 后返回。完成指已决定是否核对、需要核对时这次核对已完成，并且已开始等待下一次判断。
  - 锁定测试据此推进 `ManualClock`：返回之后的 `advance` 计入下一次判断的等待。
- 持久化失败的用例把数据库所在目录设为只读来制造写入失败，要求测试以非 root 用户运行；以 root 运行时用例报告环境不满足条件并失败，不跳过。
- `shutdown`：不再接受新的触发，执行完正在进行和已提交的检查、已接受的闭店流程和已排队的备份，再 `WriterHandle::shutdown()`。正在等待的 NTP 查询和核对间隔直接取消，已经开始的状态文件持久化执行完。返回后不留任何后台任务。
- 两份 `clippy.toml` 禁用 `test_node`，只有锁定测试及 `spec_support/` 可以 `#[allow]`。
