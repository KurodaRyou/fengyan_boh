# Fengyan BOH — 门店边缘节点

烘焙门店后厨（BOH）系统的门店本地节点。部署在每家门店的一台 Linux 机器上，局域网内的平板通过浏览器（门店节点托管的 Web SPA）访问。
**断网是常态，不是异常**：所有门店内核心操作必须在完全离线时正常工作，联网后再把事件同步到总部。

业务决策、事件目录与领域规则见 [docs/domain.md](docs/domain.md)。

本文件是所有编码 agent（Codex、Antigravity、Claude Code 等）共用的项目规则。

---

## 协作分工

- **架构与设计**由人 + Claude 负责：本文件、`docs/`、迁移 SQL 与 schema 测试、锁定测试（见「测试分工」）。
- **实现**可由任意 agent 完成，但必须遵守本文件全部规则。
- 实现 agent **不得**修改 `.github/CODEOWNERS` 列出的任何受保护路径（本文件、`docs/`、迁移、锁定测试、CI 与构建配置等）。
  发现规则不合理或缺失时，停下来在交付说明里提出，不要绕过。
- 「尚未设计」一节和 `docs/domain.md`「待确认问题」中的内容，在设计落地到文档之前**不要实现**。
- 交付前必须本地通过：`cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`scripts/ci-scan.sh`。
- 交付说明写清：改了哪些文件、对应「开发顺序」的哪几步、有哪些未决问题。之后由 Claude 做 code review。
- 合入：`main` 只接受 PR，CI `check` 必须通过，禁止强推和删除；不要求 PR 批准。
  - 所有提交和 PR 都用同一个 GitHub 账号，作者不能批准自己的 PR。锁定路径由本文件规则、Codex 自动 review、Claude review 把关，人确认后合入。
- 锁定路径（`.github/CODEOWNERS` 列出的路径）的改动只能出现在人或 Claude 提交、提交信息以 `spec:`（锁定测试）或 `docs:`（规则、设计、迁移、CI 与构建配置）开头的提交中。

### 测试分工

| 测试 | 编写 | 锁定 | 位置 |
|---|---|---|---|
| Schema 测试、迁移校验和 | Claude | 是 | `crates/boh-storage/tests/schema.rs` |
| 验收用例（`docs/domain.md`「验收用例」） | Claude 写，预期值由人确认 | 是 | `crates/*/tests/spec_*.rs` |
| Golden payload：每个 `event_type@schema_version` 一份 | Claude | 是 | `crates/*/tests/golden/` |
| HTTP 契约：状态码、错误码、信封、重复提交同一 `command_id` | Claude | 是 | `crates/*/tests/spec_*.rs` |
| 重放一致性：清空投影、重建、逐行比对 | Claude | 是 | `crates/*/tests/spec_*.rs` |
| `src/` 内的单元测试、其他集成测试 | 实现 agent | 否 | 任意，但不得以 `spec_` 开头 |

- 锁定测试只规定行为，不规定实现。它只通过稳定接口访问系统：
  HTTP 黑盒（测试入口：用数据库路径和可注入的时钟构造 `Router`）、对已冻结表的只读 SQL、接口说明中列出的纯函数。
  接口签名由 Claude 随锁定测试一起给出，写进 `docs/`。
- 锁定测试的辅助代码只放在 `crates/*/tests/spec_support/`（同样锁定），不依赖任何不受保护的代码。
- 预期值由人工确认。**任何人都不允许为了让测试通过而修改预期值。**
- 实现 agent 认为锁定测试有错时，停下来在交付说明里提出。不得修改，不得加 `#[ignore]`，不得用 `cfg`、feature 或 Cargo 配置让它不编译、不运行。
- 合入方式：锁定测试作为切片分支的第一个提交（提交信息以 `spec:` 开头，由人提交）；实现 agent 在其上开发，测试与实现在同一个 PR 合入。
  Review 时 `git diff <spec 提交> HEAD -- <锁定路径>` 必须为空。PR 打开后又有新提交时，合入前评论 `@codex review` 重新触发 Codex review。

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
| 配置 | `toml` |
| 日志 | `tracing` / `tracing-subscriber` |
| 错误 | 库 crate 用 `thiserror`，只有 `boh-server` 的 `main` 用 `anyhow` |
| 测试临时目录 | `tempfile`（仅 dev-dependency） |
| 时区 / 营业日（待引入） | `jiff` |
| 员工 PIN 哈希（待引入，认证切片） | `argon2`（Argon2id） |
| 局域网 HTTPS（待引入，认证切片之前） | `rustls`，接入方式随 `docs/domain.md` Q9 确定 |
| 上行同步（待实现） | `reqwest` + `rustls`，mTLS |

禁止引入：Redis、消息队列、ORM、微服务、任何需要外部进程的依赖。整个节点是**一个静态链接二进制 + 一个 SQLite 文件**。
**新增依赖必须先在本表登记用途，并经人工批准**；未登记的依赖不得加入任何 `Cargo.toml`。

---

## 架构

```
[ 平板 Web SPA（IndexedDB 命令暂存）]  ──HTTP (LAN)──►  axum handler
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

   定时任务（读连接 / 备份连接）：不变量自检、备份
   同步 worker（待实现，与请求完全解耦）
   └─ SELECT store_events WHERE seq > acked_seq ORDER BY seq ─► mTLS 推送总部 ─► 更新 sync_state
```

### Crate 划分

```
crates/
  boh-domain/   纯类型与校验不变量、相对校准与营业日纯函数。只允许依赖 serde / uuid / thiserror / jiff。禁止数据库、HTTP、系统 I/O（包括读系统时钟）。
  boh-storage/  SQLite：连接初始化、迁移、写入线程、读连接池、ledger（唯一写事件入口）、projections、备份、时钟模块（可注入）、原生 SQL。
  boh-app/      http（路由、响应信封）、service（命令编排）、sync（同步 worker，待实现）。
  boh-server/   main：加载配置、启动、信号处理、优雅关闭；子命令 init、rebuild-projections；认证切片加设备管理子命令 enroll-code、unlock-device、reset-unlock-code、revoke-device。
migrations/     编号 SQL 文件，001_xxx.sql、002_xxx.sql……
deploy/         systemd unit、示例配置。
scripts/        CI 扫描脚本。
```

保持扁平：不要 trait 抽象工厂、不要 Repository 接口层、不要 DDD 聚合根类层次。函数 + 结构体 + 原生 SQL。

---

## 不可违反的规则

### SQLite
- 每个连接打开时都设置（见 `boh-storage/src/connection.rs`；备份校验连接例外，见下）：
  `journal_mode = WAL`、`busy_timeout = 5000`、`synchronous = FULL`、`foreign_keys = ON`、`recursive_triggers = ON`。
  - 用 `FULL` 而不是 `NORMAL`：`NORMAL` 在断电时可能回滚已经回复客户端“成功”的事务。不要改回去。
  - `recursive_triggers` 关闭时，`INSERT OR REPLACE` 会绕过只追加触发器，静默替换账本行。不要关。
- **只有一个写连接**，由 `sqlite-writer` 线程独占。所有写操作必须走 `Writer::call`。禁止在任何地方另开写连接。
  `open_writer` / `open_reader` / `spawn_writer` 为 `pub(crate)`，对外只暴露 `boh_storage::open()`。
  - 锁定测试需要的原始连接（已设好 PRAGMA）只经 `#[doc(hidden)] pub mod boh_storage::testing` 提供；
    `clippy.toml` 的 `disallowed-methods` 禁用 `boh_storage::testing` 中的函数，只有锁定测试可以 `#[allow]`。
- 写事务一律 `BEGIN IMMEDIATE`（`Writer::call` 已处理）。
- 读连接 `query_only = ON`，只通过 `Readers::call` 使用；`Readers::call` 内部包一个 DEFERRED 读事务，读事务保持简短。
- **受控例外：备份连接**。只在备份模块中打开，都用 `SQLITE_OPEN_READ_ONLY`，不设 `query_only`，不进读连接池：
  - 源库连接：照常设置上面的 PRAGMA，执行 `VACUUM INTO`。`query_only` 连接上执行 `VACUUM INTO` 会失败，所以不能复用读连接池。
  - 校验连接：打开 `VACUUM INTO` 生成的临时文件，只执行校验查询，**不设置 `journal_mode`**。
    - 生成的文件是 DELETE 模式，只读连接上改成 WAL 会报 `attempt to write a readonly database`。
- **禁止跨 `.await` 持有 `Connection`**；禁止在 `Writer::call` / `Readers::call` 的闭包里做网络 I/O 或 sleep——闭包会阻塞唯一的写线程。
  写线程对每个任务计时，超过 50ms 记 `warn`。
- 所有表用 `STRICT`。

### 备份
1. 先删除备份目录中残留的 `tmp-*.db`；在备份连接上读取 `seq_before = coalesce(max(seq), 0)`，再执行 `VACUUM INTO '<备份目录>/tmp-<UUIDv7>.db'`。
   - `VACUUM INTO` 不覆盖已存在的文件；固定文件名在上次中途退出后会让之后每次备份都失败。
2. 只读打开临时文件，执行 `PRAGMA integrity_check`，并以 `n = coalesce(max(seq), 0)` 校验 `n >= seq_before` 且 `count(*) = n`（空账本时三者都为 0）；校验失败则删除临时文件。
   - 备份期间写入照常提交，备份快照可能包含 `seq_before` 之后的事件，所以只比下界，不比相等。备份自身的 `n` 记为 `last_backup_seq`。
3. 校验通过后对文件 fsync，原子重命名为 `boh-<UTC时间>-<UUIDv7>.db`（与临时文件同一个 UUID），再 fsync 备份目录；按保留策略清理旧文件后再 fsync 一次目录。
   - 只 fsync 文件不能让重命名后的目录项持久化，断电后新备份可能消失。
   - 文件名带 UUID：时钟回拨或同一时刻两次备份时，同名重命名会覆盖已有的备份。
4. 低峰时段每小时一次，闭店后一次。`VACUUM INTO` 期间持有读快照，会推迟 WAL checkpoint。
5. **恢复演练写成测试**：从备份恢复、清空投影并重建，结果必须和原库一致。只验证备份文件生成了不算数。
6. 异地复制（有公网用对象存储，没有公网用 NAS 或双盘轮换）本期不开发。

### 迁移
- 迁移文件一旦发布（合并到主干）**禁止修改**，只能新增下一个编号的文件。schema 测试对每个迁移文件做内容校验和。
- 每个迁移在一个事务中执行并设置 `user_version`。数据库版本高于程序支持的版本时**拒绝启动**。
- 先写迁移和 schema 测试（`crates/boh-storage/tests/schema.rs`），测试通过、schema 锁定后再写业务实现。

### ID 与时间
- 所有实体 / 事件主键用 **UUIDv7**（`TEXT`，小写带连字符）。禁止自增整数 ID。
  唯一例外：`store_events.seq`（门店内提交顺序，由 SQLite 分配，禁止显式指定）。投影表复合主键中的 `line_no` 是行号，不是自增键。
- 客户端提交的 ID（如 `command_id`）在 `boh-domain` 中校验必须是 v7。
- 时间戳一律 `INTEGER`，UTC Unix **毫秒**。禁止存格式化的日期时间字符串。
  例外：表示门店当地日期的业务字段——`business_date`（营业日）和采购单的 `deliver_on`（要求到货日），格式 `'YYYY-MM-DD'`，它们是业务概念而不是时间点。
- **顺序只看 `seq`**：重放、FIFO、同步都按 `seq`，不按任何时间戳。
- `recorded_at` 取系统时钟原值，**不做单调钳制**。
  - 顺序已由 `seq` 保证；钳制会让一次跳到未来的时钟把之后的时间全部卡在未来。
- `occurred_at` 由相对校准计算（或补录时显式给出），`business_date` 由 `occurred_at` 计算。规则见 `docs/domain.md`「时间」。
- 系统时间只通过时钟模块读取，业务代码显式接收时间。

### 只追加（Append-only）
- `store_events`、`processed_commands` 由数据库触发器禁止 `UPDATE` / `DELETE`；`store_meta` 是单行表，禁止修改和删除。
- `seq` 连续无空洞：由连续性触发器保证，禁止显式写入 `seq`。
- **只有 `Ledger::append` 能写 `store_events`**，并在同一处调用 `projections::apply`。
- 纠错 = 追加一条冲销 / 更正事件，绝不修改原事件。
- 当前状态（库存余量等）放在**投影表**中，在产生事件的**同一事务内**同步更新。投影必须能从事件流按 `seq` 完整重建。
  - `projections::apply` 只读事件本身（`store_events` 的列和 payload），不查主数据，不重新做决策（分配、吸收）。在线写入和 `rebuild-projections` 共用同一个 `apply`。
  - 业务决策在 `append` 之前完成，结果写进 payload。
- 主数据也走事件流（`MASTER_DATA_CHANGED`），主数据表是投影。
  例外：员工凭据、设备注册、会话等认证状态表不进事件流、不同步、不参与重建。
- 事件 payload 结构一旦发布不得修改：改结构只能升 `schema_version`，再写 upcast。

### 幂等
- 每个写命令必须带客户端生成的 `command_id`（UUIDv7）。超时或结果未知时，客户端**必须用原 ID、原内容重试**。
- 规范化请求：把命令反序列化成强类型结构体再序列化，剥离 `command_id` 和 `sent_at` 后存进 `processed_commands.request`。
  只剥离这两个字段（`captured_at` 是业务内容）。比对规范化文本，不用哈希。
- **幂等检查先于任何业务校验**。
  - 同一 `command_id` + 同一规范化请求 → 直接返回保存的响应（含 warnings），不重复执行。
  - 同一 `command_id` + 不同规范化请求 → `409 IDEMPOTENCY_CONFLICT`，响应中给出差异字段。
- 只有成功的命令写入 `processed_commands`。被业务校验拒绝的命令不落库，修正后可以用同一个 ID 重提。
- 命令和事件结构体禁止 `HashMap` / `HashSet`，只用 `Vec` / `BTreeMap`。

### 上行同步（只定契约，本期不实现）
- 不建 outbox。连续的 `seq` 就是同步游标。开发同步时新增迁移建单行表 `sync_state`（`acked_seq`、`last_attempt_at`、`last_error`、`consecutive_failures`）。
- 推送 `seq > acked_seq` 的事件，按 `seq` 升序分批，批次头携带 `store_id`。
- **严格按序**：遇阻就重试并报警，不跳号，没有 `dead` 状态。
- 总部按 `(store_id, seq)` 严格按序、幂等地接收，先原样落库再异步处理；解析不了的事件进入总部隔离区，不退回门店。
- 失败按指数退避。无网络时 worker 安静退避，只记 `debug` 级日志，不报错、不影响请求路径。

### 数值
- **禁止浮点数参与业务计算**（clippy `float_arithmetic = deny`）。
- 金额：`i64`，单位“分”。重量 / 体积 / 数量：`i64`，单位为该物料的最小单位（克、毫升、个）。
- `[profile.release] overflow-checks = true`；业务计算一律用 `checked_*`。

### Rust
- 非测试代码禁止 `unwrap()` / `expect()`（clippy deny）；用 `?` 和明确的错误类型。
- `unsafe` 禁止（`forbid`）。
- `clippy.toml` 的 `disallowed-methods` / `disallowed-types`：

  | 禁用 | 只允许 `#[allow]` 的位置 |
  |---|---|
  | `rusqlite::Connection::open*` | `boh-storage/src/connection.rs`、备份模块 |
  | `std::time::SystemTime::now`、`jiff::Timestamp::now`、`jiff::Zoned::now` | 时钟模块 |
  | `std::thread::sleep` | 无 |
  | `HashMap` / `HashSet`（`boh-domain`） | 无 |

  crate 级 `clippy.toml` 会覆盖根目录的配置，所以根目录的规则要完整复制进去。
- 禁止在上表以外的位置豁免这些 lint，禁止用 crate 级 `#![allow]` 放宽任何 deny 的 lint。
- Release 配置 `panic = "abort"`：任何 panic 都直接结束进程，由 systemd 重启。不要用 `catch_unwind` 吞 panic。
- 优雅关闭顺序：停止接收 HTTP → 停止同步 worker 和定时任务 → `WriterHandle::shutdown()` 处理完已入队的写任务 → `wal_checkpoint(TRUNCATE)` → 退出。

### 机器强制

| 规则 | 强制方式 |
|---|---|
| 只有一个写连接 | `boh_storage::open()` 唯一对外入口；`disallowed-methods` |
| 写事件只有一个入口 | `scripts/ci-scan.sh`：只有 `boh-storage/src/ledger.rs` 和 crate 集成测试允许出现 `INSERT INTO store_events` |
| 禁止 REPLACE 绕过触发器 | `recursive_triggers = ON` + schema 测试；`scripts/ci-scan.sh` 禁止 crate 集成测试以外的 `.rs` / `.sql` 中出现 `REPLACE` / `DO UPDATE` |
| 时间只有一个来源、闭包内不阻塞 | `disallowed-methods` |
| 序列化结果确定 | `boh-domain/clippy.toml` 的 `disallowed-types` |
| 迁移不可修改 | schema 测试中的校验和 + 锁定路径 |
| Payload 不可修改 | golden 测试 + 锁定路径 |
| 重放结果确定 | 锁定的重放一致性测试 |
| 防止溢出 | `overflow-checks` + `checked_*` |
| 业务规则正确 | 锁定的验收用例，预期值由人工确认 |
| 锁定测试、CI、构建配置不被改动 | 锁定路径清单（`.github/CODEOWNERS`）+ `main` 分支保护（只接受 PR、CI 通过、禁止强推）+ Codex 与 Claude review 比对 `spec:` 提交 |

---

## HTTP 约定

- 业务接口统一前缀 `/api/v1/`；`GET /health` 例外。
- 所有响应都是同一信封（`boh-app/src/http.rs`），`warnings` 总是存在，没有警告时为空数组：

```json
{ "success": true,  "data": { },  "warnings": [ { "code": "STOCK_SHORTFALL", "message": "...", "details": { } } ], "error": null }
{ "success": false, "data": null, "warnings": [], "error": { "code": "IDEMPOTENCY_CONFLICT", "message": "..." } }
```

- 警告不改变 `success`。警告码见 `docs/domain.md`：`STOCK_SHORTFALL`、`MOVED_DURING_COUNT`、`ABSORBED_BY_COUNT`、`EXPIRES_BEFORE_OLDER_STOCK`、`CAPTURE_TIME_ADJUSTED`。
- HTTP 状态码要和语义一致：校验失败 400、不存在 404、幂等冲突与业务冲突 409、内部错误 500。`code` 是稳定的大写蛇形字符串，客户端按 `code` 判断，不按 `message`。
- 内部错误只记日志，不把 SQL / 内部细节返回给客户端。
- `/health` 暴露：`clock_regression_ms`（超过 5 分钟为 `degraded`）、`last_backup_ok_at`、`last_backup_seq`、WAL 文件大小、最近一次不变量自检的结果；认证实现后加 `auth_failures_last_hour`；同步实现后加 `max(seq) − acked_seq` 和最后一次成功同步的时间。
  不变量自检由定时任务在读连接上执行，`/health` 只返回最近一次的结果，不现场计算。

---

## 开发顺序（每个新功能）

1. **人 + Claude**：在 `docs/domain.md` 中定义事件类型、payload 字段、投影影响、错误码和警告码。
2. **Claude**：写迁移 SQL（如需新表 / 投影表）。
3. **Claude**：写 schema 测试，跑通后表结构锁定。
4. **Claude**：写锁定测试（golden payload、HTTP 契约、验收用例、重放一致性）和接口说明；**人确认预期值**后作为 `spec:` 提交。
5. **实现 agent**：`boh-domain` 命令 / 事件结构体 + 校验。
6. **实现 agent**：`boh-app::service` 中经 `ledger::execute` 组装一个事务；`projections::apply` 增加该事件的投影。
7. **实现 agent**：`boh-app::http` handler + 路由。
8. **实现 agent**：全部锁定测试通过，补自己的单元测试，提交交付说明。
9. **Codex** 自动 review + **Claude** review（按「不可违反的规则」逐条核对，锁定路径 diff 为空），人合入。

### 路线图

1. **核心管道**：时钟模块、相对校准、营业日纯函数；`ledger::execute` / `Ledger::append` / `projections::apply` 骨架与 `rebuild-projections`；
   `Readers::call` 包读事务、写线程任务计时；信封加 `warnings`、`Actor` 提取器；`boh_storage::open()` 收口（先由 Claude 以 `spec:` 提交把 schema 测试改用 `boh_storage::testing`）；备份模块与恢复演练测试。
2. **黄金切片**：温度记录（含设备主数据事件，打通管道、幂等、重放、golden）→ 002：主数据与库存投影表 → 收货 + 报损（FIFO、账外缺口、分配来源、吸收规则、不变量自检）→ 局域网 HTTPS → 员工认证。
3. **扩展**：生产 → 盘点 → 纠错（冲销、数量更正）→ 补录入口 → 销售导入。每一步配对应的验收用例。

---

## 常用命令

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
scripts/ci-scan.sh
cargo run -p boh-server -- deploy/config.dev.toml
```

生产构建（在 macOS 上交叉编译 Linux 静态二进制，需要 `cargo-zigbuild` 和 `zig`）：

```bash
cargo zigbuild --release -p boh-server --target x86_64-unknown-linux-musl
```

ARM 门店机改用 `aarch64-unknown-linux-musl`。部署文件见 `deploy/`。

---

## 尚未设计（动手前先讨论）

- 下行同步：主数据包的格式、拉取与导入流程（原则已定：总部维护统一的 JSON 主数据包，逐行比对快照，`source = HQ_PACKAGE`）。**总部本期不开发，延后。**
- 上行同步的实现：总部接收接口、批量大小、mTLS 证书下发与轮换。**延后**；契约见「上行同步」。
- 局域网 HTTPS：证书签发与平板信任方式（见 `docs/domain.md` Q9）。**认证切片的前置条件。**
- 异地备份复制的实现。

---

## Review guidelines

供 Codex 自动 review 和 Claude review 使用。

**P0（必须阻止合入）**
- 修改了 `.github/CODEOWNERS` 列出的锁定路径，而该改动所在提交的信息不以 `spec:` 或 `docs:` 开头。逐个列出这些文件。
- 在不以 `spec:` / `docs:` 开头的提交中让锁定测试（`crates/*/tests/spec_*.rs`、`spec_support/`、`golden/`、`crates/boh-storage/tests/schema.rs`）失效：
  修改预期值、删除用例、加 `#[ignore]`、用 `cfg` / feature 排除、在 `Cargo.toml` 中设置 `test = false` / `autotests = false`、改 CI 让测试不运行。
- 修改已合入 `main` 的迁移文件。

**P1**
- 违反「不可违反的规则」任一条，例如：另开写连接、绕过 `Ledger::append` 写 `store_events`、闭包内做网络 I/O 或 sleep、
  非测试代码 `unwrap()` / `expect()`、浮点参与业务计算、未经 `checked_*` 的业务算术、新增未在技术栈表登记的依赖。
- HTTP 响应不符合「HTTP 约定」：信封缺字段、状态码与语义不符、把 SQL 或内部细节返回给客户端。
- 写命令缺少幂等处理，或缺少重复提交同一 `command_id` 的测试。
- 认证接口、PIN 或令牌可经明文 HTTP 访问或传输。

`spec:` / `docs:` 提交是设计变更：检查内容是否自洽、是否与 `docs/domain.md` 一致；其中修改已合入的迁移仍按 P0 处理。
