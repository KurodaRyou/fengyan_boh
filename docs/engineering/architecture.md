# 技术栈与模块职责

修改依赖或模块边界时读取。业务语义按 [领域索引](../domain.md) 定位；各层稳定测试入口见 [interfaces.md](../interfaces.md)。

## 技术栈（已定，不要替换）

| 用途 | 选择 |
|---|---|
| 语言 | Rust（edition 2024），Cargo workspace |
| 数据库 | SQLite，`rusqlite`（`bundled`）——**不用 sqlx / ORM** |
| 迁移 | 自带的极简迁移器（`PRAGMA user_version`），SQL 文件 `include_str!` 进二进制 |
| HTTP | `tokio` + `axum` |
| ID | `uuid` v7 |
| 序列化 | `serde` / `serde_json` |
| 配置 | `toml` |
| 日志 | `tracing` / `tracing-subscriber` |
| 错误 | 库 crate 用 `thiserror`，只有 `boh-server` 的 `main` 用 `anyhow` |
| 测试临时目录 | `tempfile`（仅 dev-dependency） |
| 时区 / 营业日 | `jiff`，启用 `tzdb-bundle-always`：时区库编进二进制，不依赖门店机的 zoneinfo |
| 员工 PIN 哈希（待引入，认证切片） | `argon2`（Argon2id） |
| 局域网 HTTPS（待引入，局域网 HTTPS 切片） | `rustls`（关闭默认特性，启用 `ring`、`std`、`tls12`）：TLS 实现；`rustls-pki-types`：读取 PEM（不用已停止维护的 `rustls-pemfile`）；`axum-server`（rustls 特性，不带默认密码学实现）：HTTPS 监听、`Handle` 优雅关闭、`RustlsConfig` 重新加载证书。见 [docs/operations/tls.md](../operations/tls.md) |
| 叶证书签发（待引入，局域网 HTTPS 切片） | `rcgen`（默认 `ring`，加 `pem`、`x509-parser` 特性）：节点签发叶证书；`x509-parser` 解析门店签发 CA 和现有叶证书（签发者名称取 CA 证书原文，读取 `notAfter`） |
| 门店节点校时的 NTP 核对 | `sntpc`，关闭默认特性；本机时间经 `NtpTimestampGenerator` 取自时钟模块，UDP 用 tokio |
| 上行同步（待实现） | `reqwest` + `rustls`，mTLS |

禁止引入：Redis、消息队列、ORM、微服务、任何需要外部进程的依赖。整个节点是**一个静态链接二进制 + 一个 SQLite 文件**。工作站网页前端的静态文件编进二进制；门店机浏览器只做显示，节点不依赖它。
**新增依赖必须先在本表登记用途，并经人工批准**；未登记的依赖不得加入任何 `Cargo.toml`。

---

## 架构

```
[ 平板 / 手机原生 App（后台任务：结果未知的命令原 ID 重试；不排队）]  ──HTTPS (LAN)──►  axum handler
[ 工作站：门店机浏览器 ]  ──HTTP（只绑定本机回环地址）──►  同一 axum（静态文件 + API）
                                   │
              ┌────────────────────┴───────────────────┐
              ▼ 命令（写）                              ▼ 查询（读）
   Writer::call(闭包)                          Readers::call(闭包)
   └─ mpsc ─► 专用 OS 线程 "sqlite-writer"      └─ spawn_blocking + 只读连接池 (query_only)
              独占唯一写连接                        每次调用包一个 DEFERRED 读事务
              BEGIN IMMEDIATE
              ledger::execute
                ├─ 幂等检查 processed_commands（先于任何业务校验）
                ├─ 业务校验 → 分配 / 吸收判定，结果写进 payload
                ├─ Ledger::append（0～N 次）
                │    ├─ INSERT store_events（seq 由 SQLite 分配）
                │    └─ projections::apply（只读事件本身；重建时共用）
                └─ INSERT processed_commands（含响应和 warnings）
              COMMIT

   后台任务（读连接 / 备份连接）：备份、不变量自检、节点校时核对；叶证书续期（不用数据库连接）
   同步 worker（待实现，与请求完全解耦）
   └─ SELECT store_events WHERE seq > acked_seq ORDER BY seq ─► mTLS 推送总部 ─► 更新 sync_state
```

### Crate 划分

```
crates/
  boh-domain/   纯类型与校验不变量、库存规则纯函数（见「库存规则归属」）、发生时间校验与营业日纯函数。只允许依赖 serde / uuid / thiserror / jiff。禁止数据库、HTTP、系统 I/O（包括读系统时钟）。
  boh-storage/  SQLite：连接初始化、迁移、写入线程、读连接池、ledger（唯一写事件入口）、projections（含各事件共用的库存投影写入）、备份、时钟模块（可注入）、原生 SQL。
  boh-app/      http（路由、响应信封）、service（命令编排，含各命令共用的库存决策读取）、节点校时核对（NTP 查询与校时信号）、叶证书签发与续期（见 docs/tls.md）、sync（同步 worker，待实现）。
  boh-server/   main：加载配置、启动（HTTPS 与本机工作站监听）、信号处理、优雅关闭；子命令 init、rebuild-projections；认证切片加子命令 enroll-code、unlock-device、reset-unlock-code、revoke-device、reset-pin。
migrations/     编号 SQL 文件，001_xxx.sql、002_xxx.sql……
deploy/         systemd unit、示例配置、部署要求与部署记录模板。
scripts/        CI 扫描脚本。
```

保持扁平：不要 trait 抽象工厂、不要 Repository 接口层、不要 DDD 聚合根类层次。函数 + 结构体 + 原生 SQL。

### 库存规则归属

库存规则的业务语义只写在以下业务专题（[「批次」](../business/inventory.md#批次)、[「盘点」](../business/operations.md#盘点)、[「盘点吸收」](../business/operations.md#盘点吸收)、[「纠错」](../business/operations.md#纠错)、[「投影表」](../business/inventory.md#投影表)）；本节只规定代码放在哪里、谁调用谁。

- 业务专题中由多个命令或事件共同引用的库存规则只有一份实现，放在下表的三层中；新切片调用它们，不在自己的 service 或事件投影函数中另写一份。
  - 只共享语义相同的部分。只属于一个命令的规则留在该命令的 service 中，例如报损的 `ITEM_HAS_NO_LOTS`、收货的批次日期校验与 `EXPIRES_BEFORE_OLDER_STOCK`；不为复用把不同命令合并成通用的库存命令。
  - 不足判定（`STOCK_SHORTFALL` 的触发条件：`qty` 超过物料净账面或指定批次的余量）由报损和纠错共用，纯函数在 `boh_domain::inventory`；是否要求确认由命令的 service 决定。
  - 确认的匹配与 `409 CONFIRMATION_REQUIRED` 的组装（[异常分级](http.md#异常分级)）是 `boh_app` 中的一个小辅助模块，供各命令的 service 调用，不放在 `service::inventory`。投影不读取确认。

| 层 | 模块 | 负责 | 不做 |
|---|---|---|---|
| 纯规则 | `boh_domain::inventory` | `alloc` 的类型；一个物料在一条命令内的账面工作状态（按 FIFO 顺序的批次余量、账外缺口、净账面）；按指定批次、FIFO、账外缺口分配并更新工作状态；批次流水号的递增与上限 | 读数据库或时钟；返回 `ApiError`、HTTP 状态码或错误码 |
| 决策读取 | `boh_app::service::inventory` | 在命令所在的事务内读取纯规则需要的库存投影，按物料缓存成命令级工作集，使同一命令后面的行看到前面各行的结果；为建批次的行分配批次号；把纯规则的错误转换成对应业务专题规定的错误码 | 写任何表、追加事件；替各命令决定拒绝、确认或警告 |
| 投影写入 | `boh_storage::inventory` | 按事件中已记录的决策建批次、按 `alloc` 增减批次余量和账外缺口、按[「投影表」](../business/inventory.md#投影表)的展开顺序写 `inventory_movements` 并编号 | 分配，判定不足或吸收；读取主数据、`inventory_counts` 或其他事件 |

- 调用方向：各命令的 service → `service::inventory` → `boh_domain::inventory`。`service::inventory` 不调用具体命令的 service；`boh_domain::inventory` 返回 `boh-domain` 的错误类型。
- `boh_domain::inventory`、`service::inventory` 同时放库存明细与批次查询的类型和处理函数。
- 同一命令的预检和正式提交走同一条计算路径。
- 投影写入的调用限制：
  - `boh_storage::inventory` 的函数是 `pub(crate)`，只由 `projections::apply` 分派到的事件投影函数调用。在线写入经 `Ledger::append`、重建经 `rebuild-projections` 进入，两条路径执行同一段代码。
  - 参数只取自事件本身（payload、`store_events` 的列和该事件的 `seq`）；只读取要更新的投影行的当前值。
  - 数量越界或要更新的批次不存在时报错，不钳制、不跳过。
  - service 只读库存投影表，不写；库存投影表只经事件投影函数写入。
- 之后加入的共享库存规则（如盘点吸收判定、库存状态上限）按同一分层放置：纯计算在 `boh_domain::inventory`，需要读投影的部分在 `service::inventory`，投影写入在 `boh_storage::inventory`。

---

## 部署与断网

烘焙门店后厨（BOH）系统的门店本地节点。部署在每家门店的一台 Linux 机器上，局域网内由门店统一采购、纳入设备管理的平板和手机通过 Android / iOS 原生 App 访问（部署要求见 [前端校时](../business/time.md#前端校时)）。
**断网是可能出现的情况**：正常情况下门店长时间有外网，偶尔因路由器故障、重启等断开外网。门店节点和前端的校时信号有效时，所有门店内核心操作无需外网即可正常工作，联网后再把事件同步到总部。节点无有效校时信号时，按 [门店机校时](../operations/sop.md#门店机校时)打开手机热点联网校准，获得有效信号后恢复写入；前端无有效信号时，先向门店节点校时。没有可用热点时，继续采用纸面登记流程。平板连不上门店节点时，未发出的记录改用纸面登记、恢复后补录；结果未知的记录由 App 后台任务用原 ID 重试，需要时在门店机工作站核对（见 [命令提交与结果未知](../business/command-submission.md#命令提交与结果未知)）。
