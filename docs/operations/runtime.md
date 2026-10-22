# 服务生命周期与健康状态

审查启动、后台任务、关闭和健康报告时读取。启动与配置的完整契约见 [server 接口](../interfaces/server.md)，测试节点入口见 [app 接口](../interfaces/app.md)。配置 TLS 的规则及未实现边界见 [operations/tls.md](tls.md)。

## 优雅关闭

- 优雅关闭顺序：停止接收 HTTP（局域网 HTTPS 与本机工作站监听）→ 停止同步 worker 和后台任务（执行完正在进行和已排队的备份、闭店流程与自检；节点校时核对、叶证书续期见下） → `WriterHandle::shutdown()` 处理完已入队的写任务 → `wal_checkpoint(TRUNCATE)` → 退出。
  - 节点校时核对：正在等待的 NTP 查询直接取消；已经开始的状态文件持久化执行完再退出（`spawn_blocking` 开始运行后不能中止）。
  - 叶证书续期：等待下一次判断的间隔直接取消；已经开始的签发与证书文件持久化执行完再退出（见 [关闭](tls.md#关闭)）。
  - checkpoint 因读事务未结束而未能截断 WAL（`CheckpointBusy`）时只记 `warn`，正常退出：已提交的事务已持久化在 WAL 中，下次打开时 SQLite 用保留的 WAL 读取并在需要时恢复已提交状态，checkpoint 在之后满足触发条件时执行。
    只有 `CheckpointBusy` 这样处理；I/O 错误、数据库损坏、关闭连接失败等照常报错。

## 健康状态（GET /health）

- `GET /health` 不需要身份。`data` 的字段（时间都是 UTC Unix 毫秒）：

  | 字段 | 取值 |
  |---|---|
  | `status` | `"ok"` 或 `"degraded"` |
  | `schema_version` | 数据库的 `user_version` |
  | `clock_synced` | 节点校时信号是否有效；含义见 [校时信号](../business/time.md#校时信号) |
  | `clock_synced_at` | 节点的上次校时时间（[门店节点校时](../business/time.md#门店节点校时)）；没有时为 `null` |
  | `clock_regression_ms` | `max(0, max(store_events.recorded_at) − now)`，账本为空时为 0；含义见 [时钟异常](../business/time.md#通用规则) |
  | `last_backup_ok_at` | 本进程最近一次成功备份完成时的 `now()`；本进程还没有成功过时为 `null` |
  | `last_backup_seq` | 该次备份的 `n`（见[备份流程](backup.md#流程)）；同上为 `null` |
  | `last_backup_failed_at` | 本进程最近一次备份尝试失败时为该次失败时的 `now()`；最近一次尝试成功或还没有尝试时为 `null` |
  | `wal_size_bytes` | `<数据库文件>-wal` 的大小；文件不存在时为 0 |
  | `invariant_check` | 本进程最近一次不变量自检的尝试与报告，结构见 [报告](invariant-check.md#报告) |
  | `tls_ca_not_after` | 门店签发 CA 的 `notAfter`；未配置 TLS 时为 `null` |
  | `tls_cert_not_after` | 正在使用的叶证书的 `notAfter`；未配置 TLS 或没有叶证书时为 `null` |
  | `issues` | 问题清单：`[{code, severity, since}]`，按下表顺序排列，没有时为 `[]` |

  - 问题清单：生成报告时按下表判定。`since` 是本进程中第一次判定该问题存在时的 `now()`；问题消失时移除，再出现时重新计时。只在进程内存中，不写数据库。
    显示对象由前端按 `code` 决定，见下表；节点不按身份过滤（`/health` 不需要身份，只给码）。

    | `code` | 条件 | `severity` | 显示对象 |
    |---|---|---|---|
    | `CLOCK_NOT_SYNCED` | `clock_synced` 是 `false` | `ERROR` | 全员（写入被拒，按 SOP 开热点） |
    | `CLOCK_REGRESSION` | `clock_regression_ms > 300000` | `WARNING` | 店长 |
    | `BACKUP_FAILED` | `last_backup_failed_at` 不是 `null` | `ERROR` | 店长 |
    | `INVARIANT_VIOLATED` | `invariant_check.last_attempt.result` 是 `violated` | `ERROR` | 店长 |
    | `INVARIANT_CHECK_FAILED` | `invariant_check.last_attempt.result` 是 `failed` | `ERROR` | 店长 |
    | `TLS_NOT_READY` | 已配置 TLS，且以下任一条件成立：HTTPS 尚未成功监听；没有当前可用叶证书；门店签发 CA 按本次时钟读数尚未生效或已过期。当前证书的可用判定见 [叶证书的签发与续期](tls.md#叶证书的签发与续期) | `ERROR` | 全员（平板可能无法连接，按 SOP 处理） |
    | `TLS_CA_EXPIRING` | `now ≥ tls_ca_not_after − 180 天`；`now ≥ tls_ca_not_after − 30 天` 时为 `ERROR` | `WARNING` / `ERROR` | 店长 |
    | `TLS_CERT_EXPIRING` | `now ≥ tls_cert_not_after − 7 天` | `ERROR` | 店长 |

    - `TLS_NOT_READY` 只根据节点已知的监听状态和当前证书、CA 判断，不探测客户端网络；生成报告时使用同一次时钟读数。全部条件消失后移除；未配置 TLS 的 debug 模式不产生此问题。其他 TLS 问题可同时存在。
    - `ERROR`：现在已经坏了，需要马上处理；`WARNING`：将来会坏。与 [异常分级](../engineering/http.md#异常分级)的级别无关。
  - `issues` 不为空时 `status` 为 `degraded`；否则为 `ok`。
    `degraded` 只是报告，不让进程退出或重启；写入是否被拒只看 `clock_synced`。
  - 备份字段只反映本进程：`null` 表示「本进程还不知道」，不表示没有备份；重启会清掉失败状态。`ok` 只表示本进程暂未发现异常，不表示已有一份可用备份。
    判断备份是否过期、重启前的历史，留到需要「门店当前是否受有效备份保护」时另行设计。
  - 生成报告成功返回 `200`（含 `degraded`）；查询数据库或读取 WAL 文件失败（文件不存在除外）返回 `500 INTERNAL_ERROR`。监控必须看 `data.status`，不能只看状态码。
  - 实现状态：`tls_ca_not_after`、`tls_cert_not_after`、`issues` 随局域网 HTTPS 切片实现；实现之前 `status` 仍按问题清单中前五个码的条件判定。
  - 后续加入的字段：认证实现后加 `auth_failures_last_hour`；同步实现后加 `max(seq) − acked_seq` 和最后一次成功同步的时间。

## 常用命令

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
scripts/ci-scan.sh
cargo run -p boh-server -- init deploy/config.dev.toml   # 新数据库先初始化：写 store_meta 和预置报损原因；已初始化时拒绝
cargo run -p boh-server -- deploy/config.dev.toml
cargo run -p boh-server -- rebuild-projections deploy/config.dev.toml   # 只在服务停止时运行
```

生产构建（在 macOS 上交叉编译 Linux 静态二进制，需要 `cargo-zigbuild` 和 `zig`）：

```bash
cargo zigbuild --release -p boh-server --target x86_64-unknown-linux-musl
```

ARM 门店机改用 `aarch64-unknown-linux-musl`。部署文件见 `deploy/`。
