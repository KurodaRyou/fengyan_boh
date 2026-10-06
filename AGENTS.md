# Fengyan BOH — 门店边缘节点

烘焙门店后厨（BOH）系统的门店本地节点。部署在每家门店的一台 Linux 机器上，局域网内的平板 / POS 通过 HTTP 访问。
**断网是常态，不是异常**：所有门店内核心操作必须在完全离线时正常工作，联网后再把事件同步到总部。

业务事件目录与领域规则见 [docs/domain.md](docs/domain.md)。

本文件是所有编码 agent（Codex、Antigravity、Claude Code 等）共用的项目规则。

---

## 协作分工

- **架构与设计**由人 + Claude 负责：本文件、`docs/domain.md`、迁移 SQL 的表结构设计。
- **实现**可由任意 agent 完成，但必须遵守本文件全部规则。
- 实现 agent **不得**自行修改本文件、`docs/domain.md`、已存在的迁移文件；发现规则不合理或缺失时，停下来在交付说明里提出，不要绕过。
- 「尚未设计」一节中的内容，在设计落地到文档之前**不要实现**。
- 交付前必须本地通过：`cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。
- 交付说明写清：改了哪些文件、对应「开发顺序」的哪几步、有哪些未决问题。之后由 Claude 做 code review。

### 文档分层
- `AGENTS.md`、`docs/`：只写**当前生效的结论**。不写讨论过程、问答记录、被否决的方案、修改历史。
  - 规则反直觉、容易被“优化”掉时，紧跟一行理由（如 `synchronous = FULL`）；其他规则不写理由。
  - 待确认问题解决后直接删除，不留划线；结论写进对应章节。
- `workdocs/`（不进版本库）：评审、提案、修订稿等过程文档。**不是规范**，内容可能已过时，实现时不要读取或引用。
  其中的结论只有写回 `AGENTS.md` / `docs/` 才算生效。
  - 一份过程文档的结论全部写回后，由完成写回的 agent 在同一次工作中**删除该文件**，并在交付说明中列出删除了哪些文件。
    仍有未写回结论的文档不删；`workdocs/` 中只应留下进行中的工作。
- 实现落地后，字段与表结构以代码为唯一来源（`boh-domain` 结构体、迁移 SQL）：`docs/domain.md` 中对应的字段清单改为指向代码，
  只保留语义与不变量；文档中的算例写成集成测试后，只留一个帮助理解的例子。

---

## 技术栈（已定，不要替换）

| 用途 | 选择 |
|---|---|
| 语言 | Rust（edition 2024），Cargo workspace |
| 数据库 | SQLite，`rusqlite`（`bundled`）——**不用 sqlx / ORM** |
| 迁移 | 自带的极简迁移器（`PRAGMA user_version`），SQL 文件 `include_str!` 进二进制 |
| HTTP | `tokio` + `axum` |
| ID | `uuid` v7 |
| 序列化 | `serde` / `serde_json` |
| 日志 | `tracing` |
| 错误 | 库 crate 用 `thiserror`，只有 `boh-server` 的 `main` 用 `anyhow` |
| 上行同步（待实现） | `reqwest` + `rustls`，mTLS |
| 时区 / 营业日（待实现） | `jiff` |

禁止引入：Redis、消息队列、ORM、微服务、任何需要外部进程的依赖。整个节点是**一个静态链接二进制 + 一个 SQLite 文件**。
新增依赖前先问自己：20 行代码能不能自己写？能就自己写。

---

## 架构

```
[ 平板 / POS ]  ──HTTP (LAN)──►  axum handler
                                   │
              ┌────────────────────┴───────────────────┐
              ▼ 命令（写）                              ▼ 查询（读）
   Writer::call(闭包)                          Readers::call(闭包)
   └─ mpsc ─► 专用 OS 线程 "sqlite-writer"      └─ spawn_blocking + 只读连接池 (query_only)
              独占唯一写连接
              BEGIN IMMEDIATE
                ├─ 校验幂等键 processed_commands
                ├─ INSERT store_events
                ├─ 同步更新投影表
                ├─ INSERT outbox_events
                └─ INSERT processed_commands（含响应）
              COMMIT

   同步 worker（后台 task，与请求完全解耦）
   └─ 轮询 outbox_events JOIN store_events ─► mTLS 推送总部 ─► 标记 synced / 退避重试
```

### Crate 划分

```
crates/
  boh-domain/   纯类型与校验不变量。只允许依赖 serde / uuid / thiserror。禁止数据库、HTTP、系统 I/O。
  boh-storage/  SQLite：连接初始化、迁移、写入线程、读连接池、原生 SQL。
  boh-app/      http（路由、响应信封）、service（命令编排，待建）、sync（outbox worker，待建）。
  boh-server/   main：加载配置、启动、信号处理、优雅关闭。
migrations/     编号 SQL 文件，001_xxx.sql、002_xxx.sql……
deploy/         systemd unit、示例配置。
```

保持扁平：不要 trait 抽象工厂、不要 Repository 接口层、不要 DDD 聚合根类层次。函数 + 结构体 + 原生 SQL。

---

## 不可违反的规则

### SQLite
- 每个连接打开时都设置（见 `boh-storage/src/connection.rs`）：
  `journal_mode = WAL`、`busy_timeout = 5000`、`synchronous = FULL`、`foreign_keys = ON`。
  - 用 `FULL` 而不是 `NORMAL`：`NORMAL` 在断电时可能回滚已经回复客户端“成功”的事务。不要改回去。
- **只有一个写连接**，由 `sqlite-writer` 线程独占。所有写操作必须走 `Writer::call`。禁止在任何地方另开写连接。
- 写事务一律 `BEGIN IMMEDIATE`（`Writer::call` 已处理）。
- 读连接 `query_only = ON`，只通过 `Readers::call` 使用。
- **禁止跨 `.await` 持有 `Connection`**；禁止在 `Writer::call` / `Readers::call` 的闭包里做网络 I/O 或 sleep——闭包会阻塞唯一的写线程。
- 所有表用 `STRICT`。

### 迁移
- 迁移文件一旦发布（合并到主干）**禁止修改**，只能新增下一个编号的文件。
- 每个迁移在一个事务中执行并设置 `user_version`。数据库版本高于程序支持的版本时**拒绝启动**。
- 先写迁移和 schema 测试（`crates/boh-storage/tests/schema.rs`），测试通过、schema 锁定后再写业务实现。

### ID 与时间
- 所有实体 / 事件主键用 **UUIDv7**（`TEXT`，小写带连字符）。禁止自增整数 ID。
- 客户端提交的 ID（如 `command_id`）在 `boh-domain` 中校验必须是 v7。
- 时间戳一律 `INTEGER`，UTC Unix **毫秒**。禁止存格式化的日期时间字符串。
  唯一例外：`business_date`（营业日，`'YYYY-MM-DD'`），它是业务概念而不是时间点。
- 写入线程保证 `created_at` 单调递增：`max(系统时间, 上一条 + 1)`，防止离线时钟回拨。

### 只追加（Append-only）
- 账本类表（`store_events`、`processed_commands`）由数据库触发器禁止 `UPDATE` / `DELETE`。
- 纠错 = 追加一条冲销 / 调整事件，绝不修改原事件。
- 当前状态（库存余量等）放在**投影表**中，在产生事件的**同一事务内**同步更新。投影必须能从事件流完整重建。
- 主数据（商品、配方、供应商、员工）不是事件溯源，是普通状态表。本期在门店本地维护，未来由总部下发覆盖（约定见 `docs/domain.md`「主数据」）。

### 幂等
- 每个写命令必须带客户端生成的 `command_id`（UUIDv7）。
- 同一 `command_id` + 同一请求哈希 → 直接返回 `processed_commands` 中保存的响应，不重复执行。
- 同一 `command_id` + 不同请求哈希 → `409 IDEMPOTENCY_CONFLICT`。

### Outbox / 同步
- 每个产生事件的写事务，在同一事务内为每条事件插入一行 `outbox_events`（只存 `event_id` 和同步状态，payload 从 `store_events` 读）。
- `outbox_events` 是同步状态表，不是账本，允许 `UPDATE status`。
- 投递语义是**至少一次**：总部必须按 `event_id` 去重，按 `(aggregate_type, aggregate_id, aggregate_version)` 排序，不能依赖到达顺序。
- 失败按指数退避写 `next_attempt_at`；超过上限标记 `dead`，**不能阻塞后续事件**，并在健康检查中暴露 dead 数量。
- 无网络时 worker 安静退避，只记 `debug` 级日志，不报错、不影响请求路径。

### 数值
- **禁止浮点数参与业务计算**（clippy `float_arithmetic = deny`）。
- 金额：`i64`，单位“分”。重量 / 体积 / 数量：`i64`，单位为该物料的最小单位（克、毫升、个）。

### Rust
- 非测试代码禁止 `unwrap()` / `expect()`（clippy deny）；用 `?` 和明确的错误类型。
- `unsafe` 禁止（`forbid`）。
- Release 配置 `panic = "abort"`：任何 panic 都直接结束进程，由 systemd 重启。不要用 `catch_unwind` 吞 panic。
- 优雅关闭顺序：停止接收 HTTP → 停止同步 worker → `WriterHandle::shutdown()` 处理完已入队的写任务 → `wal_checkpoint(TRUNCATE)` → 退出。

---

## HTTP 约定

- 业务接口统一前缀 `/api/v1/`；`GET /health` 例外。
- 所有响应都是同一信封（`boh-app/src/http.rs`）：

```json
{ "success": true,  "data": { },  "error": null }
{ "success": false, "data": null, "error": { "code": "IDEMPOTENCY_CONFLICT", "message": "..." } }
```

- HTTP 状态码要和语义一致：校验失败 400、不存在 404、幂等冲突 409、内部错误 500。`code` 是稳定的大写蛇形字符串，客户端按 `code` 判断，不按 `message`。
- 内部错误只记日志，不把 SQL / 内部细节返回给客户端。

---

## 开发顺序（每个新功能）

1. 在 `docs/domain.md` 中定义事件类型、payload 字段、投影影响。
2. 写迁移 SQL（如需新表 / 投影表）。
3. 写 schema 测试，跑通。
4. `boh-domain`：命令 / 事件结构体 + 校验。
5. `boh-app::service`：在 `Writer::call` 中组装一个事务。
6. `boh-app::http`：handler + 路由。
7. 集成测试：包括重复提交同一 `command_id`。

---

## 常用命令

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p boh-server -- deploy/config.dev.toml
```

生产构建（在 macOS 上交叉编译 Linux 静态二进制，需要 `cargo-zigbuild` 和 `zig`）：

```bash
cargo zigbuild --release -p boh-server --target x86_64-unknown-linux-musl
```

ARM 门店机改用 `aarch64-unknown-linux-musl`。部署文件见 `deploy/`。

---

## 尚未设计（动手前先讨论）

- 下行同步：总部 → 门店的主数据拉取协议（游标 / 版本号）。**总部本期不开发，延后。**
- 上行同步：总部接收接口、批量大小、mTLS 证书下发与轮换。**延后**；outbox 照常写入，同步 worker 不实现。
- 营业日：门店时区 + 日切时间配置。**阻塞所有事件写入**，见 `docs/domain.md` 待确认问题 Q1。
- 局域网客户端认证：已确定「员工上班登录终端」，登录方式（PIN / 工牌）、会话过期、设备注册未定。
- 本地备份（定期 `VACUUM INTO` 到第二块盘 / Litestream）与 outbox 已同步行的保留期。
