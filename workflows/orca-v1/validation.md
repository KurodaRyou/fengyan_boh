# Orca v1 验证记录

本文件区分**实际运行**和**纸面推演**。纸面推演只说明设计上如何处理，不证明能运行。

## 1. 环境（2026-10-14 实测）

| 项 | 结果 |
|---|---|
| Orca CLI | `/Applications/Orca.app/Contents/Resources/bin/orca`，1.4.224；`orca status` 为 `ready`；`skills get orchestration` 可用 |
| Claude Code | 2.1.300，Orca 以 `claude --dangerously-skip-permissions` 启动 worker（来自用户的 Orca 设置） |
| Codex | codex-cli 0.162.1；在新目录中被信任提示挡住（见 §2） |
| Antigravity CLI | `agy` 1.3.5（`~/.local/bin/agy`）；Orca agent id `antigravity`；`/boost` 是交互式斜杠命令，只在 prompt 开头生效 |
| 本仓库在 Orca 中 | 已注册，主检出为主 worktree；setup 钩子为 `pnpm install`、run-by-default |

## 2. 实际演练

临时 Git 仓库 `orca-drill`（会话 scratchpad 中，不涉及本仓库业务代码，没有推送、PR 或外部评论）。
协调者是本会话（Orca 终端，`coordinator_handle term_df9d4be5…`）。Run：`run_79d8ef10b526`。

| 顺序 | Task | Dispatch | agent | 结果 |
|---|---|---|---|---|
| 1 | `task_b0aa2fff88da` drill-design | `ctx_3b344ad1e4d5` | claude | `worker_done` succeeded（`msg_a5d0c82e7d8f`）；协调者核对 `notes/spec.txt` 内容后 `worker-release`（`closed_agent_terminal`） |
| 2 | `task_29343c28bca0` drill-implement（依赖 1，1 完成后由 `pending` 变 `ready`） | `ctx_bff99c08f492` | codex | `worker-start` 退出 1：`failedStage agent_readiness`，`lastError "Agent startup blocked: agent-trust-workspace"`；按回执 `recovery` 执行 `worker-release` |
| 2 重试 | 同上，`--retry-of ctx_bff99c08f492` | `ctx_5c487d1e9c5b` | claude | `worker_done` succeeded（`msg_f0b1af97ff86`）；核对 `notes/impl.txt` |
| 3 返修 | `task_ba16a0cad122` drill-implement-rework-1 | `ctx_99bd3c47c454` | 复用 `ctx_5c487d1e9c5b` 的终端（`--terminal`） | 协调者模拟 review 发现 R1（缺单位），交回原终端；`worker_done` succeeded；核对文件为两行 |

验证到的行为：
- 第一个 Task 完成后，协调者在同一回合内核对产物并派发第二个，不需要用户介入。
- 普通返修经 `worker-start --terminal` 交回原 worker 终端（同一 agent 会话）。
- 未 ack 时再次 `check` 返回同一批消息，`replayed: true`；ack 后不再返回。
- `check --wait --json` 在 stdout 之外（stderr）输出 keepalive 行；合并 stderr 后 JSON 解析会失败。
- 被复用过的终端，最后一个 Dispatch 的 `worker-release` 回执为 `retained`（`processAction: none`），需要确认无活动 Dispatch 后用 `terminal close` 关闭。
- `worker-list --run … --terminal-state reclaimable` 最终为空。

另做了一项放置测试（未作为 Task 运行）：
- `project setup-existing-folder --kind folder` 无法把普通文件夹注册为工作区（`Imported folder does not match the selected project identity.`）。
- 改用 `terminal create --command "cd <副本> && claude …"`：Claude Code 停在目录信任提示（`blockedReason: agent-trust-workspace`）。
  演练中**没有代为选择信任**，关闭了该终端。因此「人处理信任提示后，在副本中派发」这一步**未实测**；派发通道本身（上表 drill-agy-boost2）已在已信任目录实测。

Antigravity 测试 agent（同一 Run）：

| Task | Dispatch | 方式 | 结果 |
|---|---|---|---|
| `task_c63b3d34bc86` drill-agy-boost | `ctx_3566c89d0cf6` | `worker-start --agent antigravity`，spec 以 `/boost` 开头 | `worker_done` succeeded，文件正确；但 agy 历史记录中该 prompt 以 `You are working inside Orca…` 开头：`/boost` 在 TASK 块里，**没有触发 boost 模式** |
| `task_0da56fe6ac21` drill-agy-boost2 | `ctx_dde5d8506c94` | `terminal create --command "agy --dangerously-skip-permissions"` → `dispatch --return-preamble`（`injected: false`）→ `terminal send --text "/boost <preamble>" --enter` | agy 历史记录以 `/boost You are working inside Orca…` 开头；`worker_done` succeeded（`msg_da5896c82054`），文件正确；`worker-release` 回执 `retained / no_owned_resource`，手动 `terminal close` |

另外：`cd <副本> && agy --dangerously-skip-permissions` 在新文件夹中同样停在信任提示（`blockedReason: agent-trust-workspace`），未代为选择，已关闭。

证据原始 JSON 保存在本次会话的 scratchpad（`drill-evidence/`），未提交进仓库。

## 3. 纸面推演

| 场景 | 处理（WORKFLOW.md 位置） | 依据 |
|---|---|---|
| 正常完成 | S0→S13 按 §5 各阶段证据推进 | 演练覆盖了「完成 → 核验 → 派发下一个」这一环 |
| 等待人工确认后继续 | §6：写 `STATE.md` 后只问一件事并结束回合；人回复后按 §9 核对再继续；H4 只接受逐条确认 | 推演 |
| worker 汇报失败 | §8.1 第 4 条分类；§8.2 只对已证明的失败重试隔离入口；累计三次失败即熔断转人 | 旧演练只覆盖原宿主通道；现行隔离通道未联调 |
| 超时或存活不明 | §8.6：空 wait 是检查点；三次后 `worker-list` 看 `nextAction`；`unverifiable` 不授权任何动作；不加派编辑者 | 推演（Orca 指南原则） |
| 重复消息 | §7.3 未 ack 重放；§8.1 按 Task / Dispatch 和已处理消息 id 去重 | 重放行为实测；去重为推演 |
| 协调者恢复 | §9：Run 持久，`run-use` 绑定后取积压消息，以 Git 与 Orca 为准修正 `STATE.md` | **推演**：`run-use` 换终端接管未实测 |
| 规范变更与基准后移 | §5 S10：追加 `docs:` / `spec:`、确认预期值、rebase、新基准、释放旧 review 终端并新建 review | 推演 |
| PR HEAD 更新后重新 review | §5 S11、§8.7：同一隔离 review 会话复审；记录触发和 HEAD，等 code / 已启用 security 本轮完成，再取全发现并核对 CI、PASS、PR HEAD | 推演；API 字段核对见 §7，完整协调者轮询未实测 |

## 4. 交付检查

2026-10-14 在 `docs/orca-workflow-v1`（基于 `main` 484de7d）加入本目录后运行：`cargo fmt --all -- --check` 退出 0；
`cargo clippy --workspace --all-targets -- -D warnings` 退出 0；`cargo test --workspace` 退出 0（34 组 `test result: ok`）；`scripts/ci-scan.sh` 退出 0。
本分支只新增 Markdown 文件，不改 Rust 代码和锁定路径。

WORKFLOW.md §10 的锁定路径命令已在真实历史上试过：`spec:` 提交 9254ea7 列出 19 个锁定文件；实现提交 b0b74ab 输出为空；
`git diff 9254ea7 9f7bc55 -- <锁定路径>` 为空。

## 5. 已知限制

- 协调者会话结束后没有任何东西会自动推进下一阶段；Orca 只保存 Run 和消息。
- Orca 不会因 GitHub 事件唤醒协调者；CI 和 Codex review 靠协调者在自己的回合里轮询。
- 新目录的 agent 信任提示需要人处理，黑盒副本每次都是新目录。
- 所有 worker 走 `dispatch --return-preamble` + `terminal send` 的低层通道，Orca 不托管其隔离实例，需协调者核验并关闭终端及实例。
- 上述旧演练没有沙箱，不能作为安全启动证明。现行 WORKFLOW.md §7.1.1 / §7.2 禁止宿主启动任一 worker agent；代码执行另经无凭据执行器。未通过全角色进程隔离、中继和导入器验收时阻塞，不沿用旧 `worker-start` 启动方式。

## 6. PR #28 返修验证（2026-10-14）

- 分页：直接取 WORKFLOW.md §8.7 的三个命令，仅把 endpoint 换成本地假 API，用假 token 执行真实 `gh api --paginate --slurp` 和 `jq`。
  每类集合跨三页：第一页普通用户和伪装 Codex 名称，后两页各一条真实 bot 身份记录；三个集合均只返回后两页的记录（共 9 次请求）。
  空集合返回 `[]`；第二页 HTTP 500 时整段命令非零退出，停止后续读取。验证的是翻页、汇总、身份筛选与失败传播，不是对真实 GitHub 的轮询演练。
- 隔离探针：使用本机已有 Alpine 镜像 `sha256:ab3fe4defd29ba6231229a4d41440ac8bde8218e85870e53876277faa24b35c4`，不拉取镜像。
  `docker run --rm --pull=never --read-only --network=none --user=65534:65534 --cap-drop=ALL --security-opt=no-new-privileges --pids-limit=64 --memory=64m`，
  只读挂载临时输入，单独挂载可写 `blindtest/`，临时 HOME 放在 tmpfs；输出挂载点先在只读输入里建好。
  允许输出写入成功；输入写入、读取宿主假凭据和 Git hook 哨兵、修改主检出哨兵均失败；符号链接与路径穿越不能绕过。
  宿主父进程的假 token/SSH agent/AWS 环境变量未进入容器，Docker/Orca socket 不存在，非 root、no-new-privileges 和无路由检查通过。全程不访问真实凭据。
- 静态核对：首次派发、S7 协调者复跑、S8 黑盒/复现、失败重试、换 Codex 和恢复都引用同一个隔离前置条件；缺证据进入 `H9_ISOLATION`，不得降级为宿主执行。
- 仓库检查：在 `b006497` 的临时导出副本上应用本次 Markdown 改动，`cargo fmt --all`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（431 passed、0 failed、0 ignored）和 `scripts/ci-scan.sh` 均通过。
- **未验证、不能据此启用**：真实 agy/Codex agent、模型专用认证与受限出口、无凭据构建执行器、Orca 受限中继、报告导入防链接/越界和完整自动派发尚未联调。
  容器探针只证明这组容器参数下的文件/环境边界；不是 §7.1.1 的入口验收。当前没有已验证入口，相关阶段必须阻塞，不能用本段记录解除 `H9_ISOLATION`。

## 7. PR #28 第二轮返修核对（2026-10-14）

- 真实 GitHub API：读取 `pulls/28/reviews`，两条针对 `cc182dbfb8f0a339ff78e91d90aa6fceb7cc7bf0` 的 review（5528999580、5529034692）确有完整 `commit_id`、`submitted_at`、`state: COMMENTED` 和 Bot 身份。汇总评论 6184531864 由原 id 编辑，分别列 code/security 的完成时间和短 SHA，因此不能只按 id 去重，也不能只看一类完成。
- 文档路径静态核对：首次、普通返修、失败重试、信任提示后继续、换 agent、规范变更及恢复均要求 agent 自身隔离；可执行派发块只连接已验收入口，不再有宿主 `worker-start`。实现首轮/返修和 review 各分步的自包含 prompt 都指定隔离输入/输出、受限中继与无凭据执行器。
- 本轮检查：在 `cc182db` 的临时导出副本应用 Markdown 候选后，`cargo fmt --all`、格式检查、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`scripts/ci-scan.sh` 均退出 0；Markdown 围栏与文内 Bash 语法检查通过。Rust、迁移、测试及构建配置与输入提交一致。
- 最小攻击路径的文档推演：直接从实现/review agent 写宿主 `.git/hooks` 或 `.git/config`，无需启动构建，现由 agent 进程隔离门槛拒绝；经补丁/报告返回路径尝试同样写入，须被角色、路径、链接和基准校验拒绝。普通实现系列仍保留作者/逐提交边界，review 保留同会话复审，最终检查绑定导入后的宿主 SHA。
- **未做运行验证**：本仓库仍没有完整隔离入口、中继/导入器；没有启动真实 agent 或实测补丁导入。上条仅是文档边界核对，不证明 OS 强制隔离和导入器可用，`H9_ISOLATION` 继续阻塞相关阶段。

以下为 §8.7 的人工状态推演，不是自动协调者测试：

| 输入状态 | 应有结果 |
|---|---|
| 新 HEAD 已触发，只有旧 HEAD 完成记录或空集合，CI 已绿 | 等待，不能进入 S12 |
| 同 HEAD 再触发，沿用触发前相同 review id/完成内容 | 等待本轮新完成 |
| 同 HEAD 前轮仍在运行，可能在新触发后才写入 review | 等前轮终态再触发；缺关联/串行证据则阻塞，迟到结果不能充当本轮完成 |
| 本轮 code 完成，security 仍在运行 | 等待 security |
| 汇总评论 id 不变，但两类完成内容/时间已更新且各自绑定本轮 HEAD | 重新读取并核验，允许进入发现分诊 |
| 只改评论更新时间、旧完成行不变，或只有点赞 | 不当成本轮完成 |
| 本轮两类完成、全量发现已处理，CI/PASS/PR HEAD 一致 | 可以进入 S12 等人合入 |
| 完成后出现更新的运行中记录、读取失败或 HEAD 再变 | 撤销放行；等待、阻塞或新建轮次 |
