# Fengyan BOH — 门店边缘节点

烘焙门店后厨的本地 Linux 节点，门店管理的 Android / iOS 原生 App 经局域网访问。节点和前端校时信号有效时，核心操作无需外网；信号失效须按 SOP 校准；无法校准或连不上节点时，未发出的记录改用纸面流程。结果未知的请求保留原 ID 重试。完整[部署与断网边界](docs/engineering/architecture.md#部署与断网)和[提交规则](docs/business/command-submission.md)按任务读取。

本文件是所有编码 agent（Codex、Antigravity、Claude Code 等）共用的项目规则。

任务所需的规范按[文档索引](docs/README.md)定位，再读取相关章节及其引用的共用规则；实现范围先看本文件的[路线图](#路线图)。独立测试审查的读取范围仍以[「测试分工」](docs/engineering/development.md#测试分工)为准。

---

## 协作分工

- 架构、设计文档、迁移、schema 与锁定测试由人 + Claude 负责；实现可由任意 agent 完成。测试 agent（Gemini）只做独立测试审查，只读、不修改仓库、不提交。
- 实现 agent 不得修改 `.github/CODEOWNERS` 列出的受保护路径。发现规则或锁定测试有错时停下并在交付说明提出，不修改、不绕过，不使测试失效。
- 锁定测试预期由人逐条确认，任何人都不得为了通过测试修改预期。规则、迁移、CI、构建配置用人或 Claude 的 `docs:` 提交，锁定测试用 `spec:` 提交；详细提交顺序、rebase 与 review 门禁见 [开发流程](docs/engineering/development.md)。
- `main` 只接受 PR，CI `check` 必须通过，禁止强推、删除；人确认后以 merge commit 合入。Claude 本地 review 通过后才能提 PR，合入 HEAD 必须与其最近结论一致。Codex 自动 review 只在 GitHub PR 打开后运行。
- [「尚未设计」](docs/domain.md#尚未设计动手前先讨论)和 [待确认问题](docs/domain.md#待确认问题) 在结论落地前不得实现依赖部分。`workdocs/` 不是规范，实现时不读取、不引用。

### 测试分工

必读 [测试分工与独立测试审查](docs/engineering/development.md#测试分工)：规范审查先写场景再读测试，禁读实现；进程黑盒仅在确认行为缺陷后读实现定位。副本、验收范围和返修证据要求按正文执行。

### 文档分层

必读 [文档分层](docs/engineering/development.md#文档分层)：规则正文只有一处；`AGENTS.md` 与 `docs/` 只写当前结论，保留适用条件、例外、边界和实现状态。已落地字段以代码和迁移为准。

## 开发顺序（每个新功能）

完整第 1–7 步及实现开始后变更设计的流程见 [开发顺序](docs/engineering/development.md#开发顺序每个新功能)。按当前步骤读取，不能跳过人工确认、独立测试审查、Claude review 或 PR 复审。

## 路线图

已完成：write path 与基础设施；设备主数据（walking skeleton）；温度记录；备份与 `/health`；其余主数据（物料、配方、供应商、报损原因）与库存投影；收货与批次号；库存明细与批次查询；报损；不变量自检；库存规则归属；定点小数；门店节点校时；异常分级与通用确认（含报损确认、节点请求日志）。迁移到 007。

当前实现边界：
- `init` 写入 `store_meta` 和预置报损原因；第一个店长随认证切片加入。
- 业务接口经 `Actor` 开发桩取得身份（[开发桩](docs/business/auth.md#开发桩)）。
- 局域网 HTTPS 已完成设计，尚未实现；配置与部署步骤随该切片生效，见 [docs/operations/tls.md](docs/operations/tls.md) 和 [deploy/README.md](deploy/README.md)。
- 报损不判定吸收，每行都不被吸收。
- 当前没有显式补录入口；连不上门店节点时只能记纸面，补录随补录入口切片。
- 第一版只有一家门店、没有总部：`MASTER_DATA_CHANGED` 只有 `source = LOCAL` 的写入口，由店长维护（归属见 [主数据](docs/business/master-data.md#主数据)）。

待做，按顺序：
1. 盘点（含盘点吸收：已有的报损一并实现吸收判定）→ 局域网 HTTPS → 员工认证（`EMPLOYEE` 投影、写接口、golden 样本与第一个店长）。
2. 扩展：生产 → 纠错（冲销、数量更正）→ 工作站前端（技术栈先在[「尚未设计」](docs/domain.md#尚未设计动手前先讨论)中定）→ 补录入口 → 销售导入。每一步配对应的验收用例。
   - 补录入口须在上线之前完成：连不上门店节点时它是唯一的入账路径。同一切片实现按任务编号查询和后台任务取消日志的上报接口。

---

## 技术栈（已定，不要替换）

完整登记表见 [技术栈](docs/engineering/architecture.md#技术栈已定不要替换)。Rust 2024、SQLite/rusqlite、tokio/axum；不用 ORM、Redis、消息队列、微服务或需要外部进程的依赖。节点是一个静态链接二进制和一个 SQLite 文件，工作站静态文件编进二进制。
新增依赖必须先在登记表写明用途并经人工批准，未登记的不得加入 `Cargo.toml`。

## 架构

完整调用路径见 [架构](docs/engineering/architecture.md#架构)。写入统一经 `Writer::call` → `ledger::execute` → `Ledger::append` → `projections::apply`；读取通常经 `Readers::call`，按编号核对命令的已定例外见 [命令提交](docs/business/command-submission.md#8-节点侧按编号查询与取消日志)。

### Crate 划分

[模块职责](docs/engineering/architecture.md#crate-划分)：domain 是纯类型与规则，只依赖 serde / uuid / thiserror / jiff，禁止数据库、HTTP 和系统 I/O（含读时钟）；storage 管存储，app 编排，server 管进程。保持函数、结构体、原生 SQL，不加 trait 抽象工厂、Repository 或 DDD 层次。

### 库存规则归属

[完整职责与调用限制](docs/engineering/architecture.md#库存规则归属)：共同纯规则在 `boh_domain::inventory`，事务内决策读取在 `service::inventory`，只读事件的投影写入在 `boh_storage::inventory`。只共享相同语义；不复制分配或吸收规则。预检和提交走同一计算路径。

## 不可违反的规则

以下是常驻摘要；实现和 review 必须同时读取所涉及的完整专题。Claude 逐条核对本节及链接正文，不能只按摘要检查。

### 开发阶段与上线

- **上线**指第一家门店节点开始记录真实业务。当前状态：**尚未上线**。上线时由人在这里写明日期。
- 上线之前没有业务数据。任何结构变动都直接修改，不升版本号，不做新旧兼容：
  - 事件 payload 直接改结构，不升 `schema_version`，不写 upcast；
  - 迁移直接改已有文件，不新增编号；
  - golden 样本、schema 测试的校验和、锁定测试及其他测试随之修改（锁定路径照常经 `spec:` / `docs:` 提交，预期值由人确认）。开发库和测试数据直接重建。
- 上线之后，[「迁移」](docs/engineering/storage.md#迁移)、[「只追加」](docs/engineering/storage.md#只追加append-only)中禁止修改迁移文件和事件 payload 结构的规则生效。

### SQLite

[完整连接与事务规则](docs/engineering/storage.md#sqlite)：唯一写连接由 `sqlite-writer` 独占，写入只经 `Writer::call`，事务用 `BEGIN IMMEDIATE`。读连接 `query_only = ON`，`Readers::call` 包 DEFERRED 事务。提交/回滚后仍在事务中的读连接记 error 并 abort；普通查询失败且成功回滚不触发。
每个连接设置 WAL、`busy_timeout = 5000`、`synchronous = FULL`、外键与递归触发器 ON；备份只读连接不设 query_only，备份校验连接不设 journal_mode。所有表 STRICT。禁止跨 await 持有 Connection，禁止读写闭包内网络 I/O 或 sleep；写任务超过 50ms 记 warn。测试原始连接只允许锁定测试及 spec_support 使用。

### 备份

细则（文件名、流程、保留策略、触发、关闭、测试要求）见 [docs/operations/backup.md](docs/operations/backup.md)。
- 在 `spawn_blocking` 上执行，不占写线程；连接只用[「SQLite」](docs/engineering/storage.md#sqlite)中的备份连接。
- 新备份经完整性与 `seq` 校验、文件 fsync、原子重命名和目录 fsync 之后才算成功；失败只记日志并反映在 `/health`，不重试、不退出进程、不影响写入。
- 禁止并发备份。收到停止信号后不再接受新的触发，先执行完正在进行和已排队的备份及闭店流程中的自检，再关写线程。
- **恢复演练写成测试**：从备份恢复、清空投影并重建，结果必须和原库一致。只验证备份文件生成了不算数。

### 不变量自检

细则（检查项、报告结构、触发、关闭、测试要求）见 [docs/operations/invariant-check.md](docs/operations/invariant-check.md)。
- 启动时一次，之后随闭店流程（先备份，再自检）每天一次；没有独立的定时触发。所有检查经同一个入口串行执行。
- 一次检查只用一个 `Readers::call`，全部检查项看到同一个快照。
- 结果只放在进程内存中，经 `/health` 报告；不写数据库，不自动修复。发现违规或检查失败都不拒绝启动、不阻止写入和备份、不让进程退出或重启。

### 迁移

[迁移规则](docs/engineering/storage.md#迁移)：每次迁移与 `user_version` 同事务，数据库版本高于程序时拒绝启动；先锁定 schema 再实现。上线后不改已有迁移，上线前按「开发阶段与上线」直接改结构。

### ID 与时间

[完整 ID、时间与校验规则](docs/business/time.md)：主键 UUIDv7，文本为 36 位小写带连字符；例外为账本 seq、批次号及投影行号。客户端 ID 必须校验；服务端 UUID 时间取事务内 recorded_at（负值取 0），随机位取 SQLite randomblob(10)，不用 now_v7。
时间戳用 UTC Unix 毫秒；当地业务日期用 YYYY-MM-DD（批次号 YYYYMMDD）。顺序看 seq，FIFO 看批次号。recorded_at 不钳制；occurred_at 由前端提供，主数据/初始化取 recorded_at，纠错取原事件；销售营业日由命令显式给出。只经时钟模块读系统时间。
节点与前端信号按“上次校时 ≤ 现在 ≤ 上次校时 + 7 天”有效；节点无效拒绝新写入为 503 CLOCK_NOT_SYNCED，幂等重试优先；前端无效或发生时间领先超过 1 分钟为 400 CLIENT_CLOCK_NOT_SYNCED。完整边界、例外、持久化失败与补录要求必须读专题。

### 只追加（Append-only）

[完整账本与重建规则](docs/engineering/storage.md#只追加append-only)：store_events、processed_commands 禁止改删，store_meta 不改删。仅 Ledger::append 写事件且不写 seq 列；决策先完成并写进 payload，同事务更新投影。apply 只读事件、使用 domain 类型解析，不查主数据或重新分配/判定吸收；在线与重建共用，按 seq 完整重建。
纠错追加事件；认证状态表不进事件流、不参与重建。上线后 payload 改结构须升版本并 upcast；GOODS_RECEIVED@1 无法 upcast，迁移 007 及重建拒绝它。上线前例外见「开发阶段与上线」。

### 幂等

[完整幂等契约](docs/engineering/http.md#幂等)：写命令带 UUIDv7 command_id，结果未知用原 ID、原内容重试。强类型规范化后剥离 command_id 和 clock_synced_at，敏感字段按认证例外处理；比较 command_type 与规范化文本。先幂等后业务校验，同内容返回保存响应（含 warnings），不同内容 409 IDEMPOTENCY_CONFLICT；仅成功命令落 processed_commands。命令/事件类型只用 Vec/BTreeMap，不用 HashMap/HashSet。

### 认证

细则（设备注册、登录、PIN 设定与重置、限速、敏感命令的幂等、开发桩）见 [docs/business/auth.md](docs/business/auth.md)。
- PIN（含确认值）和 PIN 重置授权秘密不得明文写入事件、数据库、WAL、备份、日志或留存响应；初始化和 CLI 仅在终端无回显输入 PIN，不接受 PIN 命令行参数。
- 设备令牌、PIN、会话令牌、PIN 重置授权、注册码、解锁码只经 HTTPS 或本机回环地址（门店机工作站）传输；节点未配置 TLS 时不注册认证接口。监听与启动检查见 [docs/operations/tls.md](docs/operations/tls.md)。

### 上行同步（只定契约，本期不实现）

- 不建 outbox。连续的 `seq` 就是同步游标。开发同步时新增迁移建单行表 `sync_state`（`acked_seq`、`last_attempt_at`、`last_error`、`consecutive_failures`）。
- 推送 `seq > acked_seq` 的事件，按 `seq` 升序分批，批次头携带 `store_id`。事件带 `store_events` 的全部列，包括 `clock_synced_at`。
- **严格按序**：遇阻就重试并报警，不跳号，没有 `dead` 状态。
- 总部按 `(store_id, seq)` 严格按序、幂等地接收，先原样落库再异步处理；解析不了的事件进入总部隔离区，不退回门店。
- 失败按指数退避。无网络时 worker 安静退避：连接状态变化（断开、恢复）记 `info`，单次重试记 `debug`；不报错、不影响请求路径。

### 数值

[数值、数量与单位](docs/business/inventory.md#数值)：禁止浮点业务计算，金额 i64 分，数量 i64 千分之一基本单位（最多 3 位小数），系数 i64 十亿分之一（最多 9 位）。HTTP/payload 数量与系数用小数字符串。仅单位换算处舍入，中间值 i128；业务算术 checked_*，release 开 overflow-checks。输入上限、库存上限及舍入边界必须读专题。

### Rust

必读 [Rust、错误与日志规则](docs/engineering/rust.md#rust)，涉及服务生命周期另读 [优雅关闭](docs/operations/runtime.md#优雅关闭)。非测试禁止 unwrap/expect，禁止 unsafe；外部错误附操作描述、保留 source 与调用位置，不压成字符串，每个内部错误只记一次完整诊断，秘密不进日志或客户端错误。
禁止在允许清单外豁免 lint，禁止 crate 级 allow 放宽 deny。release panic=abort，不 catch_unwind。正常关闭须排空已接受任务再关 Writer、checkpoint；NTP 等待可取消，已开始的持久化须完成；只有 CheckpointBusy 可警告后正常退出。

### 机器强制

[机器强制表](docs/engineering/rust.md#机器强制)与 [lint 豁免清单](docs/engineering/rust.md#rust)。检查通过不能替代规则核对或真实进程验收。

## HTTP 约定

必读 [HTTP、写命令与异常分级](docs/engineering/http.md)。统一信封 success/data/warnings/error，warnings 恒为数组；失败响应的 error.details 恒存在。身份权限 → 结构取值 → 幂等 → 节点校时 → 业务校验；成功和原样重试均 200。不得返回 SQL/内部细节。
局域网 HTTPS、本机回环 HTTP 的绑定、Host 限制与 release TLS 门禁按 [operations/tls.md](docs/operations/tls.md)；尚未实现边界见路线图。`GET /health` 无需身份，完整字段、问题清单与降级语义见 [健康状态](docs/operations/runtime.md#健康状态get-health)。

## 常用命令

交付前必须本地通过：

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/ci-scan.sh
```

初始化、运行、停服重建与生产构建命令见 [运行命令](docs/operations/runtime.md#常用命令)。交付说明列出修改文件、对应开发步骤、未决问题；之后按开发流程 review。

## 尚未设计（动手前先讨论）

完整待设计范围与已定边界见 [尚未设计](docs/domain.md#尚未设计动手前先讨论)：前端技术栈与本地存储、工作站、上下行同步、异地复制、恢复与业务连续性。动手前先讨论；已定的时间、提交、传输和恢复原则不得视为尚未决定。

## Review guidelines

Codex 自动 review 与 Claude review 必须读 [完整 P0/P1 清单](docs/engineering/development.md#review-guidelines)，并核对本文件「不可违反的规则」及对应专题。逐提交检查锁定路径及测试失效的两项由 Claude 执行，Codex 不报；其余规则照常。
