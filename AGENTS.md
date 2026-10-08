# Fengyan BOH — 门店边缘节点

烘焙门店后厨（BOH）系统的门店本地节点。部署在每家门店的一台 Linux 机器上，局域网内的平板通过浏览器（门店节点托管的 Web SPA）访问。
**断网是常态，不是异常**：所有门店内核心操作必须在完全离线时正常工作，联网后再把事件同步到总部。

业务决策、事件目录与领域规则见 [docs/domain.md](docs/domain.md)；系统依赖的门店操作规范见 [docs/sop.md](docs/sop.md)；规范用词见 [docs/glossary.md](docs/glossary.md)；锁定测试使用的接口见 [docs/interfaces.md](docs/interfaces.md)；备份模块的细则见 [docs/backup.md](docs/backup.md)。

本文件是所有编码 agent（Codex、Antigravity、Claude Code 等）共用的项目规则。

---

## 协作分工

- **架构与设计**由人 + Claude 负责：本文件、`docs/`、迁移 SQL 与 schema 测试、锁定测试（见「测试分工」）。
- **实现**可由任意 agent 完成，但必须遵守本文件全部规则。
- **测试 agent**（Gemini）只做独立测试审查（见「测试分工」）：只读，不修改仓库，不提交。
- 实现 agent **不得**修改 `.github/CODEOWNERS` 列出的任何受保护路径（本文件、`docs/`、迁移、锁定测试、CI 与构建配置等）。
  发现规则不合理或缺失时，停下来在交付说明里提出，不要绕过。
- 「尚未设计」一节和 `docs/domain.md`「待确认问题」中的内容，在设计落地到文档之前**不要实现**。
- 交付前必须本地通过：`cargo fmt --all`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`scripts/ci-scan.sh`。
- 交付说明写清：改了哪些文件、对应「开发顺序」的哪几步、有哪些未决问题。之后由 Claude 在本地 review，通过后再提 PR（见「开发顺序」第 6、7 步）。
- 合入：`main` 只接受 PR，CI `check` 必须通过，禁止强推和删除；不要求 PR 批准。
  - 所有提交和 PR 都用同一个 GitHub 账号，作者不能批准自己的 PR。锁定路径由本文件规则、Codex 自动 review、Claude review 把关，人确认后合入。
- 锁定路径（`.github/CODEOWNERS` 列出的路径）的改动只能出现在人或 Claude 提交、提交信息以 `spec:`（锁定测试）或 `docs:`（规则、设计、迁移、CI 与构建配置）开头的提交中。
- 切片需要的依赖、构建或 CI 改动用 `docs:` 提交，放在 `spec:` 提交之前。
- 不属于当前切片的规则或流程改动，用独立的 `docs:` 分支和 PR，在下一个切片开分支之前合入；不 cherry-pick 进切片分支。

### 测试分工

| 测试 | 编写 | 锁定 | 位置 |
|---|---|---|---|
| Schema 测试、迁移校验和 | Claude | 是 | `crates/boh-storage/tests/schema.rs` |
| 验收用例（`docs/domain.md`「验收用例」） | Claude 写，预期值由人确认 | 是 | `crates/*/tests/spec_*.rs` |
| Golden payload：每个 `event_type@schema_version` 的每种 payload 结构变体至少一份 | Claude | 是 | `crates/*/tests/golden/` |
| HTTP 契约：状态码、错误码、信封、重复提交同一 `command_id` | Claude | 是 | `crates/*/tests/spec_*.rs` |
| 重放一致性：清空投影、重建、逐行比对 | Claude | 是 | `crates/*/tests/spec_*.rs` |
| `src/` 内的单元测试、其他集成测试 | 实现 agent | 否 | 任意，但不得以 `spec_` 开头 |

- Golden 按 payload 的结构分支覆盖，不只按事件类型计数：`MASTER_DATA_CHANGED` 的 `ITEM`、`RECIPE`、`SUPPLIER`、`EMPLOYEE`、`WASTE_REASON`、`EQUIPMENT` 各有独立样本；库存影响的正常 / 吸收互斥分支及可选字段的出现 / 省略分支也分别覆盖。每个分支须校验完整序列化 JSON，不得仅检查公共字段。
- 锁定测试只规定行为，不规定实现。它只通过稳定接口访问系统：
  HTTP 黑盒（测试入口：用数据库路径和可注入的时钟构造 `Router`）、对已冻结表的只读 SQL、接口说明中列出的纯函数。
  接口签名由 Claude 随锁定测试一起给出，写进 [docs/interfaces.md](docs/interfaces.md)。
- 锁定测试的辅助代码只放在 `crates/*/tests/spec_support/`（同样锁定），不依赖任何不受保护的代码。
- 预期值由人工确认。**任何人都不允许为了让测试通过而修改预期值。**
- 实现 agent 认为锁定测试有错时，停下来在交付说明里提出。不得修改，不得加 `#[ignore]`，不得用 `cfg`、feature 或 Cargo 配置让它不编译、不运行。
- 合入方式：锁定测试以 `spec:` 提交（由人提交）；实现 agent 在其上开发，测试与实现在同一个 PR 合入。
  Review 的比对基准是切片分支开头连续的 `spec:` / `docs:` 提交中的最后一个，`git diff <比对基准> HEAD -- <锁定路径>` 必须为空；实现开始后要改设计时按「开发顺序」末尾的规定处理。PR 打开后又有新提交时，合入前评论 `@codex review` 重新触发 Codex review。

**独立测试审查**（测试 agent）：
- 时机：「开发顺序」第 4.2 步，锁定测试和接口说明写好之后、人确认预期值之前。实现交付后（第 5 步之后），可以再做一次进程级黑盒验收，与 Claude review 并行。
- 审查对象是 Claude 准备的副本：复制工作区，排除 `.git`、`workdocs/`、`target/`。产出（报告、脚本、日志、运行时数据）只写进副本中的 `blindtest/`。
  - 看不到提交历史、review 记录和过程文档，推演才独立于设计方。
- 规范依据只有本文件和 `docs/`；代码和测试不是依据。
- 第一阶段（规范审查）：只读本文件、`docs/`、`migrations/`，先把推演出的场景清单写进报告，写完不改；再读锁定测试逐条对照。不读 `src/`。
- 黑盒验收只测锁定测试覆盖不到的真实进程行为：配置、启动与拒绝启动、停止信号、命令行子命令。确认了行为缺陷之后才可以读 `src/` 定位原因。
- 发现分为缺用例、预期值存疑、规范不清、实现缺陷，每条带文档依据（路径:行号与原文）和确定程度（确认 / 推测）。
  - 「缺用例」必须写明会让哪一种合理的实现错误漏过锁定测试；只能靠故障注入才能测的不报；现有稳定接口测不到时，写明需要的接口改动。
  - 严重度：一个合理的实现错误会因此漏过锁定测试，并造成数据丢失或错误、备份不可用、幂等失效或规则违反，才算「高」或「中」；其余为「低」。
- Claude 逐条核对报告，采纳项写进锁定测试、接口说明或 `docs/` 之后，人再确认预期值。

### 文档分层
- `AGENTS.md`、`docs/`：只写**当前生效的结论**。不写讨论过程、问答记录、被否决的方案、修改历史。
  - 规则反直觉、容易被“优化”掉时，紧跟一行理由（如 `synchronous = FULL`）；其他规则不写理由。
  - 待确认问题解决后直接删除，不留划线；结论写进对应章节。
- `workdocs/`（不进版本库）：评审、提案、修订稿等过程文档。**不是规范**，内容可能已过时，实现时不要读取或引用。
  其中的结论只有写回 `AGENTS.md` / `docs/` 才算生效。
  - 一份过程文档的结论全部写回后，由完成写回的 agent 在同一次工作中**删除该文件**，并在交付说明中列出删除了哪些文件。
    仍有未写回结论的文档不删；`workdocs/` 中只应留下进行中的工作。
- 实现落地后，字段与表结构以代码为唯一来源（`boh-domain` 结构体、迁移 SQL）：`docs/domain.md` 中对应的字段清单改为指向代码（时机见「开发顺序」第 1 步），
  只保留语义与不变量；文档中的算例写成集成测试后，只留一个帮助理解的例子。

---

## 开发顺序（每个新功能）

1. **人 + Claude**：在切片分支开头的 `docs:` 提交中，先按「文档分层」整理上一个已合入切片在 `docs/` 中的字段清单（改为指向代码），再在 `docs/domain.md` 中定义本切片的事件类型、payload 字段、投影影响、错误码和警告码。
   - 整理不放在本切片的实现提交之后：实现之后的 `docs:` 提交不在分支开头，会让 `git diff <比对基准> HEAD -- <锁定路径>` 非空。
2. **Claude**：写迁移 SQL（如需新表 / 投影表）。
3. **Claude**：写 schema 测试，跑通后表结构锁定。
4. **锁定测试**：
   1. **Claude** 写锁定测试（golden payload、HTTP 契约、验收用例、重放一致性）和接口说明。
   2. **测试 agent** 做独立测试审查（见「测试分工」）；Claude 逐条核对，把采纳项写进锁定测试、接口说明或 `docs/`。引出的规范修改作为 `docs:` 提交，放在 `spec:` 提交之前。
   3. **人逐条确认预期值**后作为 `spec:` 提交。Claude 给出实现 agent 的 prompt：本切片范围、比对基准、需要顺带落实的已生效改动（已写回 `docs/` 或锁定测试，如上一切片遗留的修复）、不涉及规范的小改动（如去掉多余的检查）。`workdocs/` 中的条目由 Claude 把内容直接写进 prompt，不让实现 agent 读取或引用 `workdocs/`。
5. **实现 agent**：在 `spec:` 提交之上实现本切片，交付前全部锁定测试通过：
   - `boh-domain`：命令 / 事件结构体 + 校验。
   - `boh-app::service`：经 `ledger::execute` 组装一个事务；`projections::apply` 增加该事件的投影。
   - `boh-app::http`：handler + 路由。
   - 补自己的单元测试，提交交付说明。
6. **Claude** 在本地 review 切片分支：按「不可违反的规则」逐条核对，逐个提交核对锁定路径，`git diff <比对基准> HEAD -- <锁定路径>` 为空。
   有问题退回实现 agent 修改后重新 review；通过后 Claude 给出可以提 PR 的结论。
7. **提 PR**，GitHub 上自动运行 **Codex** review（只在 PR 打开后运行，本地不跑）。Codex 的发现由 Claude 判断是否采纳，需要修改的退回实现 agent；
   PR 打开后又有新提交时，合入前评论 `@codex review` 重新触发。CI `check` 通过后人合入。

**实现开始后要改设计**（第 6、7 步的 review 发现需要改 `docs/` 或锁定测试）：
- 在切片分支开头的 `docs:` / `spec:` 提交之后追加新的提交：规范改动由 Claude 作为 `docs:` 提交；锁定测试改动由人逐条确认预期值后作为 `spec:` 提交。再把实现提交 rebase 到它们之后，比对基准随之后移。
  - 切片分支允许强推，`main` 不允许：锁定路径的改动必须始终位于分支开头，不能合入后再补救。
- 之后实现 agent 在 rebase 后的分支上修改，从第 6 步重新 review。

### 路线图

1. **Write path 与基础设施**：时钟模块、相对校准、营业日纯函数；`Readers::call` 包读事务、写线程任务计时；信封加 `warnings`；`boh_storage::open()` 收口。
2. **Walking skeleton 与首批切片**：
   设备主数据（walking skeleton：`ledger::execute` / `Ledger::append` / `projections::apply` 骨架与 `rebuild-projections`、`Actor` 开发桩、`boh-server init`、`EQUIPMENT` 写接口、迁移 002：`equipment`；打通 write path、幂等、重放、golden payload）
   → 温度记录（迁移 003：`temperature_readings`）→ 备份模块与恢复演练测试、`/health` 字段（迁移 004：`store_events(recorded_at)` 索引）→ 005：其余主数据与库存投影表 → 收货 + 报损（FIFO、账外缺口、分配来源、吸收规则、不变量自检）→ 局域网 HTTPS → 员工认证。
   - 设备主数据切片的 `init` 只写 `store_meta`；预置报损原因随 005 加入；`EMPLOYEE`（`employees` 投影、写接口、golden 样本）与第一个店长随认证切片加入。
3. **扩展**：生产 → 盘点 → 纠错（冲销、数量更正）→ 补录入口 → 销售导入。每一步配对应的验收用例。

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
| 时区 / 营业日 | `jiff`，启用 `tzdb-bundle-always`：时区库编进二进制，不依赖门店机的 zoneinfo |
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
  boh-server/   main：加载配置、启动、信号处理、优雅关闭；子命令 init、rebuild-projections；认证切片加子命令 enroll-code、unlock-device、reset-unlock-code、revoke-device、reset-pin。
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
  `open_writer` / `open_reader` / `migrate` / `spawn_writer` 为 `pub(crate)`，对外只暴露 `boh_storage::open()`（公开 API 见 [docs/interfaces.md](docs/interfaces.md)）。
  - 锁定测试需要的原始连接（已设好 PRAGMA）只经 `#[doc(hidden)] pub mod boh_storage::testing` 提供；
    `clippy.toml` 的 `disallowed-methods` 禁用 `boh_storage::testing` 中的函数，只有锁定测试及 `spec_support/` 可以 `#[allow]`。
- 写事务一律 `BEGIN IMMEDIATE`（`Writer::call` 已处理）。
- 读连接 `query_only = ON`，只通过 `Readers::call` 使用；`Readers::call` 内部包一个 DEFERRED 读事务，读事务保持简短。
  - 提交或回滚之后连接仍处于事务中（`!is_autocommit()`）时，记 `error` 并 `std::process::abort()`，由 systemd 重启，不归还连接、不让进程带着失效的读池继续运行。
    普通查询失败、已成功回滚的错误不触发。这是不可恢复故障的路径，不走优雅关闭。
- **受控例外：备份连接**。只在备份模块中打开，都用 `SQLITE_OPEN_READ_ONLY`，不设 `query_only`，不进读连接池：
  - 源库连接：照常设置上面的 PRAGMA，执行 `VACUUM INTO`。`query_only` 连接上执行 `VACUUM INTO` 会失败，所以不能复用读连接池。
  - 校验连接：打开 `VACUUM INTO` 生成的临时文件，只执行校验查询，**不设置 `journal_mode`**。
    - 生成的文件是 DELETE 模式，只读连接上改成 WAL 会报 `attempt to write a readonly database`。
- **禁止跨 `.await` 持有 `Connection`**；禁止在 `Writer::call` / `Readers::call` 的闭包里做网络 I/O 或 sleep——闭包会阻塞唯一的写线程。
  写线程对每个任务计时，超过 50ms 记 `warn`。
- 所有表用 `STRICT`。

### 备份
细则（文件名、流程、保留策略、触发、关闭、测试要求）见 [docs/backup.md](docs/backup.md)。
- 在 `spawn_blocking` 上执行，不占写线程；连接只用「SQLite」中的备份连接。
- 新备份经完整性与 `seq` 校验、文件 fsync、原子重命名和目录 fsync 之后才算成功；失败只记日志并反映在 `/health`，不重试、不退出进程、不影响写入。
- 禁止并发备份。收到停止信号后不再接受新的触发，先执行完正在进行和已排队的备份，再关写线程。
- **恢复演练写成测试**：从备份恢复、清空投影并重建，结果必须和原库一致。只验证备份文件生成了不算数。

### 迁移
- 迁移文件一旦发布（合并到主干）**禁止修改**，只能新增下一个编号的文件。schema 测试对每个迁移文件做内容校验和。
- 每个迁移在一个事务中执行并设置 `user_version`。数据库版本高于程序支持的版本时**拒绝启动**。
- 先写迁移和 schema 测试（`crates/boh-storage/tests/schema.rs`），测试通过、schema 锁定后再写业务实现。

### ID 与时间
- 所有实体 / 事件主键用 **UUIDv7**（`TEXT`，36 位小写带连字符，版本位 `7`，变体位 `8`–`b`）。禁止自增整数 ID。
  唯一例外：`store_events.seq`（门店内提交顺序，由 SQLite 分配）。投影表中的 `line_no`、`source_line_no`、`movement_no` 是行号或展开编号，不是自增键。
- 客户端提交的 ID（如 `command_id`，含请求体、路径参数、查询参数和开发桩请求头中的 ID）在 `boh-domain` 中校验必须是 v7，文本只接受与存储相同的 36 位小写带连字符形式；大写、无连字符、花括号、`urn:` 前缀都按取值非法处理。
- 服务端生成的 UUIDv7 在写事务内构造：时间部分取该命令的 `recorded_at`（负值按 0），随机部分取 SQLite `randomblob(10)`，由 `boh-domain` 的纯函数拼装。不用 `Uuid::now_v7()`。
  - `now_v7()` 在 uuid crate 内部读系统时钟，绕过时钟模块，`disallowed-methods` 也拦不住。
- 时间戳一律 `INTEGER`，UTC Unix **毫秒**。禁止存格式化的日期时间字符串。
  例外：表示门店当地日期的业务字段——`business_date`（营业日）和采购单的 `deliver_on`（要求到货日），格式 `'YYYY-MM-DD'`，它们是业务概念而不是时间点。
- **事件间顺序只看 `seq`**：重放、同步按 `seq`；FIFO 先扣盘盈批次，再按来源事件的 `seq` 升序，同一来源事件内按原 payload 的行序（`source_line_no`）升序。不按任何时间戳。
- `recorded_at` 取系统时钟原值，**不做单调钳制**。
  - 顺序已由 `seq` 保证；钳制会让一次跳到未来的时钟把之后的时间全部卡在未来。
- `occurred_at` 由相对校准计算（或补录时显式给出），`business_date` 由 `occurred_at` 计算（`SALES_IMPORTED` 例外，由命令显式给出）。规则见 `docs/domain.md`「时间」。
- 系统时间只通过时钟模块读取，业务代码显式接收时间。

### 只追加（Append-only）
- `store_events`、`processed_commands` 由数据库触发器禁止 `UPDATE` / `DELETE`；`store_meta` 是单行表，禁止修改和删除。
- `seq` 连续无空洞：`Ledger::append` 的 `INSERT` 不写 `seq` 列，由 SQLite 分配；连续性触发器拒绝跳号的显式 `seq`。
  - 触发器不拒绝恰好等于 `max(seq) + 1` 的显式值，所以「不写 `seq` 列」靠 review 保证。
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
- 规范化请求：把命令反序列化成强类型结构体再序列化，剥离 `command_id` 和 `sent_at`，按下述例外处理敏感字段后存进 `processed_commands.request`。
  普通业务命令只剥离这两个字段（`captured_at` 是业务内容）。比对 `command_type` + 规范化请求（文本，不用哈希）；所有含 PIN 或 PIN 重置授权秘密的命令（含员工主数据权限提升）适用以下例外。
- 敏感命令的每个秘密字段以独立随机盐的 Argon2id 验证值替代，参数强度不低于员工凭据；非秘密字段仍比对规范化文本，秘密字段用已保存的验证值判等，不重新生成哈希作文本比对。
- 敏感命令的成功记录绑定发起员工和 `device_id`；原结果重试按保存的原命令核对有效设备和秘密证明，只返回非秘密回执，不重新执行当前 PIN 校验、不重复修改主数据、不重新签发或恢复授权及会话。
- 秘密字段比对前执行设备锁和限速检查；比对失败计入同一设备和节点的 PIN 失败计数，幂等冲突明细只列字段名，不返回秘密原值、提交值或验证值。
- PIN（含确认值）和 PIN 重置授权秘密不得明文写入事件、数据库、WAL、备份、日志或留存响应；初始化和 CLI 仅在终端无回显输入 PIN，不接受 PIN 命令行参数。
- **幂等检查先于任何业务校验**。
  - 同一 `command_id` + 同一 `command_type` + 同一规范化请求 → 直接返回保存的响应（含 warnings），不重复执行。
  - 同一 `command_id`，但 `command_type` 或规范化请求不同 → `409 IDEMPOTENCY_CONFLICT`，响应中给出差异字段。
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
  | `boh_storage::testing` 中的函数 | 锁定测试及 `spec_support/` |
  | `boh_app::test_router`、`boh_app::test_node` | 锁定测试及 `spec_support/` |
  | `std::time::SystemTime::now`、`jiff::Timestamp::now`、`jiff::Zoned::now`、`jiff::tz::TimeZone::system`、`jiff::tz::TimeZone::try_system` | 时钟模块 |
  | `std::thread::sleep` | 无 |
  | `HashMap` / `HashSet`（`boh-domain`） | 无 |

  crate 级 `clippy.toml` 会覆盖根目录的配置，所以根目录的规则要完整复制进去。
- 禁止在上表以外的位置豁免这些 lint，禁止用 crate 级 `#![allow]` 放宽任何 deny 的 lint。
- Release 配置 `panic = "abort"`：任何 panic 都直接结束进程，由 systemd 重启。不要用 `catch_unwind` 吞 panic。
- 优雅关闭顺序：停止接收 HTTP → 停止同步 worker 和定时任务（执行完正在进行和已排队的备份） → `WriterHandle::shutdown()` 处理完已入队的写任务 → `wal_checkpoint(TRUNCATE)` → 退出。
  - checkpoint 因读事务未结束而未能截断 WAL（`CheckpointBusy`）时只记 `warn`，正常退出：已提交的事务已持久化在 WAL 中，下次打开时 SQLite 用保留的 WAL 读取并在需要时恢复已提交状态，checkpoint 在之后满足触发条件时执行。
    只有 `CheckpointBusy` 这样处理；I/O 错误、数据库损坏、关闭连接失败等照常报错。

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
| 锁定测试、CI、构建配置不被改动 | 锁定路径清单（`.github/CODEOWNERS`）+ `main` 分支保护（只接受 PR、CI 通过、禁止强推）+ Codex 与 Claude review 比对切片分支开头连续的 `spec:` / `docs:` 提交中的最后一个 |

---

## HTTP 约定

- 业务接口统一前缀 `/api/v1/`；`GET /health` 例外。
- 所有响应都是同一信封（`boh-app/src/http.rs`），`warnings` 总是存在，没有警告时为空数组：

```json
{ "success": true,  "data": { },  "warnings": [ { "code": "STOCK_SHORTFALL", "message": "...", "details": { } } ], "error": null }
{ "success": false, "data": null, "warnings": [], "error": { "code": "IDEMPOTENCY_CONFLICT", "message": "...", "details": { } } }
```

- 警告不改变 `success`。警告码见 `docs/domain.md`：`STOCK_SHORTFALL`、`ABSORBED_BY_COUNT`、`EXPIRES_BEFORE_OLDER_STOCK`、`CAPTURE_TIME_ADJUSTED`。
- 错误对象包含 `code`、`message`、`details`；`details` 总是存在，没有明细时为 `{}`。
- HTTP 状态码要和语义一致：校验失败 400、不存在 404、幂等冲突与业务冲突 409、内部错误 500。`code` 是稳定的大写蛇形字符串，客户端按 `code` 判断，不按 `message`。
- 通用错误码：结构或取值错误 `400 VALIDATION_FAILED`；引用的实体不存在 `404 REFERENCE_NOT_FOUND`。
- 其他通用错误码：未匹配的路由 `404 ROUTE_NOT_FOUND`；方法不允许 `405 METHOD_NOT_ALLOWED`；内部错误 `500 INTERNAL_ERROR`；缺少或无效的身份 `401 UNAUTHENTICATED`；角色不够 `403 FORBIDDEN`。
- 身份与权限通过后，请求体不是合法 JSON、缺少 `Content-Type: application/json`、字段缺失、未知字段、类型或取值不对，以及路径参数无法解析，一律 `400 VALIDATION_FAILED`。
  - axum 提取器默认返回纯文本的 415 / 422，必须转换成信封。
- 写命令按以下顺序处理，先命中的结果生效：身份与权限 → 请求结构与取值（反序列化成强类型命令）→ 幂等检查 → 业务校验。被前两步拒绝的请求不做幂等比对。
- 写命令成功一律 `200`，重试返回的原响应也是 `200`。
- `409 IDEMPOTENCY_CONFLICT` 的 `details` 是 `{"fields": [...]}`：`command_type` 不同时只列 `"command_type"`；否则列出规范化请求中取值不同或只在一方出现的顶层字段名，按字典序排列。规范化请求包含路径参数（如设备 ID）。
- 内部错误只记日志，不把 SQL / 内部细节返回给客户端。
- `GET /health` 不需要身份。`data` 的字段（时间都是 UTC Unix 毫秒）：

  | 字段 | 取值 |
  |---|---|
  | `status` | `"ok"` 或 `"degraded"` |
  | `schema_version` | 数据库的 `user_version` |
  | `clock_regression_ms` | `max(0, max(store_events.recorded_at) − now)`，账本为空时为 0；含义见 domain.md「时间」时钟异常 |
  | `last_backup_ok_at` | 本进程最近一次成功备份完成时的 `now()`；本进程还没有成功过时为 `null` |
  | `last_backup_seq` | 该次备份的 `n`（见「备份」）；同上为 `null` |
  | `last_backup_failed_at` | 本进程最近一次备份尝试失败时为该次失败时的 `now()`；最近一次尝试成功或还没有尝试时为 `null` |
  | `wal_size_bytes` | `<数据库文件>-wal` 的大小；文件不存在时为 0 |

  - `clock_regression_ms > 300000`，或 `last_backup_failed_at` 不是 `null`，`status` 为 `degraded`；否则为 `ok`。
    `degraded` 只是报告：不阻止任何写入，不让进程退出或重启。
  - 备份字段只反映本进程：`null` 表示「本进程还不知道」，不表示没有备份；重启会清掉失败状态。`ok` 只表示本进程暂未发现异常，不表示已有一份可用备份。
    判断备份是否过期、重启前的历史，留到需要「门店当前是否受有效备份保护」时另行设计。
  - 生成报告成功返回 `200`（含 `degraded`）；查询数据库或读取 WAL 文件失败（文件不存在除外）返回 `500 INTERNAL_ERROR`。监控必须看 `data.status`，不能只看状态码。
  - 后续加入的字段：认证实现后加 `auth_failures_last_hour`；同步实现后加 `max(seq) − acked_seq` 和最后一次成功同步的时间；
    库存切片实现不变量自检后加最近一次的结果（检查范围、结果结构、检查任务自身失败与发现不变量被破坏如何分别报告，随该切片定）。
  - 不变量自检由定时任务在读连接上执行：启动时一次，之后每个 UTC 整点后 30 分一次；`/health` 只返回最近一次的结果，不现场计算。

---

## 常用命令

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
scripts/ci-scan.sh
cargo run -p boh-server -- init deploy/config.dev.toml   # 新数据库先初始化，只写 store_meta；已初始化时拒绝
cargo run -p boh-server -- deploy/config.dev.toml
cargo run -p boh-server -- rebuild-projections deploy/config.dev.toml   # 只在服务停止时运行
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
- 备份恢复：恢复入口与操作步骤，以及恢复后的上行同步（恢复到旧备份会让 `seq` 回退，与总部已接收的事件冲突）。原则已定：恢复时清空会话、PIN 重置授权、设备注册、设备解锁码、锁定状态与失败计数，平板重新注册；员工 PIN 哈希保留。认证幂等记录不得绕过恢复后的设备注册校验。**延后**；恢复演练测试照常要求。

---

## Review guidelines

供 Codex 自动 review 和 Claude review 使用。

**P0（必须阻止合入）**
- 修改了 `.github/CODEOWNERS` 列出的锁定路径，而该改动所在提交的信息不以 `spec:` 或 `docs:` 开头。逐个列出这些文件。
- 在不以 `spec:` / `docs:` 开头的提交中让锁定测试（`crates/*/tests/spec_*.rs`、`spec_support/`、`golden/`、`crates/boh-storage/tests/schema.rs`）失效：
  修改预期值、删除用例、加 `#[ignore]`、用 `cfg` / feature 排除、在 `Cargo.toml` 中设置 `test = false` / `autotests = false`、改 CI 让测试不运行。
- 以上两条由 Claude review 逐个提交核对，Codex 不报。
  - Codex review 看到的是整个 PR 压成的一个提交（作者为 Codex，提交信息取 PR 标题），分不出改动属于哪个提交，报出的都是误报。
- 修改已合入 `main` 的迁移文件。

**P1**
- 违反「不可违反的规则」任一条，例如：另开写连接、绕过 `Ledger::append` 写 `store_events`、闭包内做网络 I/O 或 sleep、
  非测试代码 `unwrap()` / `expect()`、浮点参与业务计算、未经 `checked_*` 的业务算术、新增未在技术栈表登记的依赖。
- HTTP 响应不符合「HTTP 约定」：信封缺字段、状态码与语义不符、把 SQL 或内部细节返回给客户端。
- 写命令缺少幂等处理，或缺少重复提交同一 `command_id` 的测试。
- 认证接口、PIN 或令牌可经明文 HTTP 访问或传输。

`spec:` / `docs:` 提交是设计变更：检查内容是否自洽、是否与 `docs/domain.md` 一致；其中修改已合入的迁移仍按 P0 处理。
