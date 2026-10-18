# Rust、错误诊断与机器约束

实现和代码 review 必须遵守本文件；生命周期见 [operations/runtime.md](../operations/runtime.md#优雅关闭)。

## Rust

- 非测试代码禁止 `unwrap()` / `expect()`（clippy deny）；用 `?` 和明确的错误类型。
- `unsafe` 禁止（`forbid`）。
- 错误处理：
  - 库 crate 的错误类型不为第三方错误（`rusqlite::Error`、`std::io::Error` 等）实现 `From`。外部错误转成项目错误时附操作描述（`&'static str`，如「插入库存批次」「同步备份目录」），保留原错误作为 `source()`，构造函数用 `#[track_caller]` 记录调用位置。
    - 去掉 `From` 是为了让编译器强制补上下文：`?` 隐式转换会丢掉「正在做什么」。
  - 不用 `to_string()` 把底层错误压成字符串。
  - 只在出现新业务含义的位置附上下文，不逐层重复包装。rusqlite 行映射闭包内的 `?` 保留，在外层查询调用处附一次。
  - 不跨 crate 合成统一错误类型；`Writer::call` / `Readers::call` 的 `E: From<StorageError>` 不变。HTTP 错误码保持业务码，不用通用错误分类替代。
  - 每个内部错误只在一处记录一条完整诊断：source 链、调用位置和上下文字段。不逐层重复打印。
    - 请求路径：内部错误转换成 `ApiError` 时记录，带命令身份（`command_type`、`command_id`、发起员工、`device_id`）。闭包内的错误在写线程或阻塞线程上转换并记录；闭包外的内部错误（入队失败、`JoinError`）在 handler 侧转换并记录。
      - 不把闭包内错误的日志移到 handler：调用方放弃等待后，已入队的任务仍会执行，失败时只有任务所在线程还能记录。
    - 重建投影：错误附当前事件的 `seq`、`event_type`、`schema_version`。
    - 备份：错误附失败阶段（生成、校验、文件 fsync、分配备份序号、重命名、目录 fsync）和已确定的备份序号。
    - 启动、迁移、关闭：`main` 的 `anyhow` 附操作描述。
  - 诊断字段用允许清单：不记录完整命令、请求体、认证信息或秘密。返回客户端的仍是不含内部细节的 `500 INTERNAL_ERROR`。
- 日志只记应用响应摘要（请求日志）、内部错误诊断（见上）、实际发生的状态变化与有限的运维操作，不做全链路或永久审计。命令结果以 `processed_commands` 与账本为准，当前状态以 `/health` 为准；日志缺失不证明请求未收到或命令未提交。
- 请求日志：进入应用中间件并生成响应的每次 HTTP 请求记一行 `info`，时点为响应生成，耗时截至该时点；它不证明客户端收到响应。响应生成之前被放弃的请求不另记。字段只取允许清单：
  - 方法、匹配到的路由模板（如 `/api/v1/lots/{lot_id}`；未匹配时不记路径）、状态码、顶层错误码（成功时省略）、原因码（`error.details.confirmations[].code`，去重、按字节序排序）、`command_type` 与 `command_id`（写命令且已解析出时）、操作人与设备（身份已确定时）、耗时（毫秒）。
  - 不记请求体、查询参数的取值、PIN、令牌和响应内容。
  - 它是访问日志，不是诊断：`500` 只记 `INTERNAL_ERROR`；完整诊断按上面「每个内部错误只在一处记录」另记一条，同一个 `500` 有一条诊断和一条请求日志。
  - 用 axum 中间件实现，不新增依赖。
  - 平板上报的后台任务取消日志与请求日志写入同一输出，字段清单另行规定，见 [命令提交与结果未知](../business/command-submission.md#命令提交与结果未知)第 8 条（随补录入口切片）。
- `clippy.toml` 的 `disallowed-methods` / `disallowed-types`：

  | 禁用 | 只允许 `#[allow]` 的位置 |
  |---|---|
  | `rusqlite::Connection::open*` | `boh-storage/src/connection.rs`、备份模块 |
  | `boh_storage::testing` 中的函数 | 锁定测试及 `spec_support/` |
  | `boh_app::test_router`、`boh_app::test_node` | 锁定测试及 `spec_support/` |
  | `boh_storage::backup::Backup::hold` | `boh_app::TestNode::hold_backups`、`boh-app` / `boh-storage` 中备份相关的 `#[cfg(test)]` 单元测试 |
  | `std::time::SystemTime::now`、`jiff::Timestamp::now`、`jiff::Zoned::now`、`jiff::tz::TimeZone::system`、`jiff::tz::TimeZone::try_system`、`time::OffsetDateTime::now_utc`、`time::OffsetDateTime::now_local`、`time::UtcDateTime::now`、`x509_parser::time::ASN1Time::now`、`x509_parser::certificate::Validity::is_valid`、`x509_parser::certificate::Validity::time_to_expiration` | 时钟模块 |
  | `std::thread::sleep` | 无 |
  | `HashMap` / `HashSet`（`boh-domain`） | 无 |

  crate 级 `clippy.toml` 会覆盖根目录的配置，所以根目录的规则要完整复制进去。
- 禁止在上表以外的位置豁免这些 lint，禁止用 crate 级 `#![allow]` 放宽任何 deny 的 lint。
- Release 配置 `panic = "abort"`：任何 panic 都直接结束进程，由 systemd 重启。不要用 `catch_unwind` 吞 panic。

## 机器强制

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
| 外部错误附操作描述 | 库 crate 不为第三方错误实现 `From`，`?` 无法隐式转换 |
| 业务规则正确 | 锁定的验收用例，预期值由人工确认 |
| 锁定测试、CI、构建配置不被改动 | 锁定路径清单（`.github/CODEOWNERS`）+ `main` 分支保护（只接受 PR、CI 通过、禁止强推）+ Codex 与 Claude review 比对切片分支开头连续的 `spec:` / `docs:` 提交中的最后一个 |

---
