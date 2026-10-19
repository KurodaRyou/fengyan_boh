# SQLite、账本与重建

所有涉及数据库连接、写事务、事件或投影的任务必读。当前阶段与上线前例外见 [AGENTS.md](../../AGENTS.md#开发阶段与上线)；测试入口见 [storage 接口](../interfaces/storage.md)。

## SQLite

- 每个连接打开时都设置（见 `boh-storage/src/connection.rs`；备份校验连接例外，见下）：
  `journal_mode = WAL`、`busy_timeout = 5000`、`synchronous = FULL`、`foreign_keys = ON`、`recursive_triggers = ON`。
  - 用 `FULL` 而不是 `NORMAL`：`NORMAL` 在断电时可能回滚已经回复客户端“成功”的事务。不要改回去。
  - `recursive_triggers` 关闭时，`INSERT OR REPLACE` 会绕过只追加触发器，静默替换账本行。不要关。
- **只有一个写连接**，由 `sqlite-writer` 线程独占。所有写操作必须走 `Writer::call`。禁止在任何地方另开写连接。
  `open_writer` / `open_reader` / `migrate` / `spawn_writer` 为 `pub(crate)`，对外只暴露 `boh_storage::open()`（公开 API 见 [storage 测试接口](../interfaces/storage.md#公开-api)）。
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

## 迁移

- 上线之后，迁移文件**禁止修改**，只能新增下一个编号的文件（上线之前见[「开发阶段与上线」](../../AGENTS.md#开发阶段与上线)）。schema 测试对每个迁移文件做内容校验和。
- 每个迁移在一个事务中执行并设置 `user_version`。数据库版本高于程序支持的版本时**拒绝启动**。
- 先写迁移和 schema 测试（`crates/boh-storage/tests/schema.rs`），测试通过、schema 锁定后再写业务实现。

## 只追加（Append-only）

- `store_events`、`processed_commands` 由数据库触发器禁止 `UPDATE` / `DELETE`；`store_meta` 是单行表，禁止修改和删除。
- `seq` 连续无空洞：`Ledger::append` 的 `INSERT` 不写 `seq` 列，由 SQLite 分配；连续性触发器拒绝跳号的显式 `seq`。
  - 触发器不拒绝恰好等于 `max(seq) + 1` 的显式值，所以「不写 `seq` 列」靠 review 保证。
- **只有 `Ledger::append` 能写 `store_events`**，并在同一处调用 `projections::apply`。
- 纠错 = 追加一条冲销 / 更正事件，绝不修改原事件。
- 当前状态（库存余量等）放在**投影表**中，在产生事件的**同一事务内**同步更新。投影必须能从事件流按 `seq` 完整重建。
  - `projections::apply` 只读事件本身（`store_events` 的列和 payload），不查主数据，不重新做决策（分配、吸收）。在线写入和 `rebuild-projections` 共用同一个 `apply`。
  - 业务决策在 `append` 之前完成，结果写进 payload。
  - `projections::apply` 用 `boh-domain` 的 payload 类型经 `serde_json` 解析 payload，不手写 JSON 结构校验。
    - golden 锁定的类型同时约束写入端和重放端；upcast 在类型上进行。
- 主数据也走事件流（`MASTER_DATA_CHANGED`），主数据表是投影。
  例外：员工凭据、设备注册、会话等认证状态表不进事件流、不同步、不参与重建。
- 上线之后，事件 payload 结构不得修改：改结构只能升 `schema_version`，再写 upcast（上线之前见[「开发阶段与上线」](../../AGENTS.md#开发阶段与上线)）。
  - 例外：`GOODS_RECEIVED@1` 缺少生成批次号所需的信息，不能 upcast，也不推测；账本中有 `@1` 收货时，迁移 007 报错、节点拒绝启动（见 [收货接口](../business/operations.md#收货接口)）。
