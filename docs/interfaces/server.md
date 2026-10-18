# server 稳定测试接口

本文件属于 [锁定测试接口契约](../interfaces.md)，签名、可见性与行为约束均为锁定测试的依据。

## boh-server：配置

```rust
// crates/boh-server/src/lib.rs
pub mod config;

// boh_server::config
pub struct Config {
    pub store_id: StoreId,
    pub db_path: PathBuf,
    pub listen_addr: SocketAddr,
    pub timezone: StoreTimeZone,
    pub business_day_cutoff: BusinessDayCutoff,
    pub reader_pool_size: NonZeroUsize,
    pub dev_actor_stub: bool,
    pub backup_dir: PathBuf,
    pub backup_keep_count: NonZeroU32,
    pub closing_backup_time: ClosingBackupTime,
    pub ntp_servers: Vec<NtpServer>,
    pub workstation_addr: Option<SocketAddr>,  // 随局域网 HTTPS 切片
    pub tls: Option<TlsConfig>,                // 随局域网 HTTPS 切片
}

// 随局域网 HTTPS 切片
pub struct TlsConfig {
    pub ip: Ipv4Addr,
    pub ca_cert_path: PathBuf,
    pub ca_key_path: PathBuf,
    pub cert_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NtpServer {
    pub host: String,  // 主机名或 IP 地址；IPv6 地址不带方括号
    pub port: u16,
}
pub fn parse_config(text: &str) -> Result<Config, impl std::error::Error>;
```

- 解析 TOML 配置文本，不读文件、不创建目录。错误类型由实现决定，锁定测试只区分 `Ok` / `Err`。
- `backup_dir` 必填；`backup_keep_count` 缺省为 168，必须是正整数（TOML 整数，`1`–`u32::MAX`）；`closing_backup_time` 缺省为 `"23:30"`，格式见 `parse_closing_backup_time`。
- `ntp_servers` 必填，非空字符串数组，按原顺序解析（[门店节点校时](../business/time.md#门店节点校时)）：
  - `host` 或 `host:port`，端口是 `1`–`65535` 的十进制数，缺省 123；IPv6 地址写成 `[addr]` 或 `[addr]:port`，`host` 中不带方括号。
  - `host` 不能为空，不含空白；不带方括号的 IPv6 地址（如 `::1`）有多个冒号，无法区分端口，为错误。
- `workstation_addr`（随局域网 HTTPS 切片）可选；IP 不是回环地址（`127.0.0.0/8`、`::1`）时为错误。
- `[tls]`（随局域网 HTTPS 切片）可选；有这一段时四项都必填，`ip` 必须在 `10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16` 内，`listen_addr` 的 IP 必须等于 `ip`（[监听](../operations/tls.md#监听)「配置与启动检查」）。release 构建缺少 `[tls]` 时拒绝启动，这一检查不在 `parse_config` 中。
- `main` 经此函数加载配置。

## boh-server：`init` 子命令

锁定测试以子进程运行 `boh-server` 二进制（`env!("CARGO_BIN_EXE_boh-server")`）：`boh-server init <配置文件>`。

- 退出码 0 表示成功，非 0 表示失败；测试不依赖输出文本。
- 读取配置中的 `db_path`、`store_id`、`timezone`、`business_day_cutoff`、`ntp_servers`；不创建 `backup_dir`，不监听端口，不启动后台任务。
- 节点信号（[门店节点校时](../business/time.md#门店节点校时) `init`）：状态文件 `<db_path>-clock-sync` 有效时不查询 NTP；否则查询一轮，成功时写状态文件后继续，失败时以非 0 退出，数据库中不写任何数据（`store_meta`、`processed_commands`、`store_events` 仍为空），状态文件不创建。
  - 测试把 `ntp_servers` 指向 `spec_support` 中的假 NTP 服务（本机 UDP），它以系统时间加设定的偏差回复；查询轮次和顺序由假服务收到的请求核对。
- 时间取系统时钟：测试只断言时间落在调用前后读取的系统时间之间，营业日按 `boh_domain::time::business_date` 由事件的 `occurred_at` 核对。
- 写入内容见 [主数据](../business/master-data.md#主数据)初始化；测试经 `boh_storage::testing::open_reader` 读取数据库，经 `boh_app::test_router` 访问已初始化的数据库。
- 回滚测试：先经 `boh_storage::testing::open_writer` 和 `migrate` 建库，在 `waste_reasons` 中放一行 `code = 'OTHER'`；`init` 写不进最后一条预置原因，必须以非 0 退出，`store_meta`、`processed_commands`、`store_events` 仍为空。删掉这一行后 `init` 可以重新执行。

## boh-server：`rebuild-projections` 子命令

锁定测试以子进程运行：`boh-server rebuild-projections <配置文件>`。

- 退出码 0 表示成功，非 0 表示失败；测试不依赖输出文本。
- 经 `boh_storage::open()` 打开 `db_path`，核对 `store_meta`（为空或 `store_id` 与配置不同时以非 0 退出，不重建），再调用 `Writer::rebuild_projections`。
  - 重建失败（如某个事件的 payload 无法解析）时以非 0 退出，投影表保持运行前的内容。
- 不受节点信号限制：不读状态文件，不查询 NTP，不监听端口，不启动后台任务。
- 参数只接受这一种形式：子命令名拼错、配置文件之外多带参数时以非 0 退出，不打开数据库；配置文件不存在或无法解析时以非 0 退出。

## boh-server：服务进程

锁定测试以子进程运行：`boh-server <配置文件>`，经配置的 `listen_addr`（本机地址）用 HTTP/1.1 访问，由测试经系统 `kill` 命令发送信号。

- 就绪：`GET /health` 返回 200 之后视为已开始服务。
- 局域网 HTTPS 切片：配置 `[tls]` 的服务进程经 HTTPS 访问，测试用 `spec_support/` 中固定的测试连锁根与门店签发 CA；`TestNode` 驱动叶证书续期的测试入口、HTTPS 测试客户端随该切片的锁定测试给出。
- 启动时读取状态文件并开始一次核对，不等待它完成就开始服务（[门店节点校时](../business/time.md#门店节点校时)）。
- 收到 SIGTERM 或 SIGINT 后按 [优雅关闭顺序](../operations/runtime.md#优雅关闭)退出，退出码 0：
  - 正在等待的 NTP 查询直接取消，不等满 5 秒超时。只在等待 NTP、没有进行中的备份、自检、闭店流程或状态文件持久化时，退出时间不受这 5 秒影响（锁定测试在这种场景下断言 3 秒内退出）；有这些工作时按 [关闭顺序](../operations/runtime.md#优雅关闭)执行完再退出。
  - 没有读事务阻塞 checkpoint 时，退出后 `<db_path>-wal` 不存在或大小为 0（锁定测试只构造这种场景）；`CheckpointBusy` 时按 [优雅关闭](../operations/runtime.md#优雅关闭)保留 WAL 并正常退出，不在此列。已成功的写命令经 `boh_storage::testing::open_reader` 都能读到。
