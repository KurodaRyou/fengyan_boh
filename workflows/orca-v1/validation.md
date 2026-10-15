# Orca v1 验证记录

本文件区分**实际运行**和**纸面推演**。纸面推演只说明设计上如何处理，不证明能运行。

## 1. 环境（2026-10-14 实测）

| 项 | 结果 |
|---|---|
| Orca CLI | `/Applications/Orca.app/Contents/Resources/bin/orca`，1.4.224；`orca status` 为 `ready`；`skills get orchestration` 可用 |
| Claude Code | 2.1.300，Orca 以 `claude --dangerously-skip-permissions` 启动 worker（来自用户的 Orca 设置） |
| Codex | codex-cli 0.162.1；在新目录中被信任提示挡住（见 §2） |
| Gemini | `command -v gemini` 无结果：**本机不可用**，测试 agent 只能按 WORKFLOW.md §8.4 换成 Codex |
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
  演练中**没有代为选择信任**，关闭了该终端。因此 WORKFLOW.md §7.1 中「信任后 `worker-start --terminal` 接管」这一步**未实测**。

证据原始 JSON 保存在本次会话的 scratchpad（`drill-evidence/`），未提交进仓库。

## 3. 纸面推演

| 场景 | 处理（WORKFLOW.md 位置） | 依据 |
|---|---|---|
| 正常完成 | S0→S13 按 §5 各阶段证据推进 | 演练覆盖了「完成 → 核验 → 派发下一个」这一环 |
| 等待人工确认后继续 | §6：写 `STATE.md` 后只问一件事并结束回合；人回复后按 §9 核对再继续；H4 只接受逐条确认 | 推演 |
| worker 汇报失败 | §8.1 第 4 条分类；§8.2 只对已证明的失败 `--retry-of`；三次失败即熔断转人 | 演练覆盖了启动失败的 recovery 与重试 |
| 超时或存活不明 | §8.6：空 wait 是检查点；三次后 `worker-list` 看 `nextAction`；`unverifiable` 不授权任何动作；不加派编辑者 | 推演（Orca 指南原则） |
| 重复消息 | §7.3 未 ack 重放；§8.1 按 Task / Dispatch 和已处理消息 id 去重 | 重放行为实测；去重为推演 |
| 协调者恢复 | §9：Run 持久，`run-use` 绑定后取积压消息，以 Git 与 Orca 为准修正 `STATE.md` | **推演**：`run-use` 换终端接管未实测 |
| 规范变更与基准后移 | §5 S10：追加 `docs:` / `spec:`、确认预期值、rebase、新基准、释放旧 review 终端并新建 review | 推演 |
| PR HEAD 更新后重新 review | §5 S11：同一 review 终端复审新增提交，黑盒沿用 / 补测 / 重验，授权后 `@codex review`，新 PASS 的 HEAD 必须等于 PR HEAD | 推演；GitHub 轮询命令未实测 |

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
- 本机没有 Gemini CLI。
- Orca 启动的 Claude worker 带 `--dangerously-skip-permissions`：实现 agent 和测试 agent 的写入边界只靠规则和 review 约束，没有沙箱。
