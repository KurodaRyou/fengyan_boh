# Orca 编排工作流 v1（可选）

用 Orca 把 AGENTS.md「开发顺序」的交接自动化：用户启动一次，协调者负责派发、等待、核验、返修和阶段交接，
只在人工关口或确实无法推进时停下来。

- **可选**：只有用户明确调用 `fengyan-orca-v1` skill 或明确说用 Orca v1 时才启用。普通会话照现行流程，不进入本工作流。
- **不是新规则**：规则仍以 `AGENTS.md`、`CLAUDE.md` 为准。本工作流不扩大任何角色的权限，不跳过任何人工确认、独立审查或提交边界。

## 文件

| 文件 | 用途 |
|---|---|
| [WORKFLOW.md](WORKFLOW.md) | 角色、阶段、交接证据、人工关口、异常处理、恢复 |
| [prompts/coordinator.md](prompts/coordinator.md) | 协调者入口：执行规则（防止中途停下） |
| [prompts/design.md](prompts/design.md) | 设计 Claude 各步的完成标志 |
| [prompts/implement.md](prompts/implement.md) | 实现 agent 的首轮 / 返修 Task spec 模板 |
| [prompts/review.md](prompts/review.md) | review Claude 的分诊、正文、黑盒核对、复审、Codex 发现模板 |
| [prompts/blindtest-spec.md](prompts/blindtest-spec.md)、[prompts/blindtest-accept.md](prompts/blindtest-accept.md) | 测试 agent 两个阶段的模板 |
| [templates/STATE.md](templates/STATE.md) | 每个切片的运行记录模板（复制到 `workdocs/orca/<切片>/`） |
| [validation.md](validation.md) | 实际演练证据、纸面推演、未验证部分 |
| `.agents/skills/fengyan-orca-v1/SKILL.md` | 显式调用入口 |

## 前提

- Orca 已运行（`orca status --json`），本仓库已在 Orca 中注册；协调者是在**主检出**的 Orca 终端里运行的 Claude Code。
- 本机有 `claude` 和实现 agent（默认 `codex`）；测试 agent 用 Gemini，不可用时按 WORKFLOW.md §8.4。
- 黑盒副本等新目录第一次启动 agent 时会弹出信任提示，需要人在 Orca 终端里自己选择（关口 `H7_TRUST`）。

## 启动

在主检出的 Orca 终端中打开 Claude Code，粘贴：

```text
用 Orca v1 工作流开发切片「<切片名>」：先完整读 .agents/skills/fengyan-orca-v1/SKILL.md 并按它执行。
范围：<一句话或指向 AGENTS.md 路线图的条目>。
授权：推送切片分支 <是/否>；开 PR <是/否>；PR 评论（@codex review、不采纳理由）<是/否>。合入由我自己做。
```

中途退出后恢复，用同一条 prompt；协调者会发现已有的 `workdocs/orca/<切片>/STATE.md`，先核对真实状态再继续。

## 人工关口

设计决定（H1）、覆盖缺口取舍（H2）、外来结论写回前（H3）、**逐条确认预期值**（H4）、缺授权时的推送 / PR / 评论（H5）、合入（H6）、agent 信任提示（H7）。
详见 WORKFLOW.md §6。其余交接（派发测试 agent、交回设计 Claude、派发实现、启动 review、普通返修、复审）自动进行。

## 退出

- 暂停：直接结束协调者会话。正在跑的 worker 会做完当前 Task，结果留在 Orca 的 Run 收件箱里；**没有协调者时不会进入下一阶段**。
- 改回现行流程：告诉协调者「退出 Orca v1」。它把当前阶段、HEAD、基准和未决事项写进 `STATE.md` 并结束；
  之后照 CLAUDE.md 手动转发 prompt，`STATE.md` 里的 prompt 与结论可以直接沿用。保留的 worker 终端由协调者在退出前释放，或按人的要求保留。
