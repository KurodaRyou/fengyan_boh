---
name: fengyan-orca-v1
description: >-
  Fengyan_BOH 的可选 Orca 编排工作流 v1：由一个协调者 Claude 通过 Orca 的 Run / Task / Dispatch
  自动派发测试 agent、实现 agent 和独立 review Claude，完成 AGENTS.md「开发顺序」的全部交接，只在人工关口停下。
  只在用户明确说「用 Orca v1」「fengyan-orca-v1」「用 Orca 工作流开发切片」或显式调用本 skill 时使用；
  普通开发、review、设计讨论会话即使提到 Orca 也不要自动进入本工作流。
---

# Fengyan_BOH Orca 编排工作流 v1

你将作为本切片的**协调者兼设计 Claude**运行。规则来源是 `AGENTS.md` 和 `CLAUDE.md`；本工作流只规定怎样在 Orca 上执行，不给你额外权限。

只有 Claude Code 能运行本工作流。你不是 Claude Code（例如 Codex、Antigravity / Gemini）时，立即停下，告诉用户本工作流的协调者必须是 Claude Code；不读后续文件，不做任何提交。

## 第一步：加载
按顺序完整读：
1. `AGENTS.md`、`CLAUDE.md`、`.github/CODEOWNERS`
2. `workflows/orca-v1/WORKFLOW.md`
3. `workflows/orca-v1/prompts/coordinator.md`（执行规则：派发后不停下、核验证据、自动返修、只在关口等人）
4. 本机 Orca 的版本匹配指南：`orca skills get orchestration`（`ORCA_CLI_COMMAND` 已设置时用它代替 `orca`）

其余模板（`prompts/design.md`、`implement.md`、`review.md`、`blindtest-*.md`、`templates/STATE.md`）在用到的阶段再读。

## 第二步：确认能运行
- `orca status --json` 正常；当前终端由 Orca 管理；当前目录是本仓库的**主检出**（`workdocs/` 在这里）。不满足时停下报告。
- 入口 prompt 给出切片名、范围、授权。缺授权说明时按 WORKFLOW.md 关口 `H0_AUTH` 问一次。
- 任一实现、review、测试 agent 启动前，必须核对 WORKFLOW.md §7.1.1 / §7.2 的进程隔离、中继与补丁/报告导入证据；仓库代码另经无凭据执行器。缺少已验证入口时转 `H9_ISOLATION`，不使用宿主 `worker-start` 或直跑作为替代。

## 第三步：开始或恢复
- `workdocs/orca/<切片>/STATE.md` 不存在：从 `templates/STATE.md` 建立，`orca orchestration run-create` 建 Run，进入 `S0_INIT`。
- 已存在：这是恢复，按 WORKFLOW.md §9 先核对 Git、Orca 和产物的真实状态，再从记录的阶段继续。

然后按 WORKFLOW.md §5 逐阶段推进，直到 `S13_CLOSE` 或某个人工关口。

## 退出
用户说「退出 Orca v1」时：把当前阶段、HEAD、基准、Run / Task / Dispatch、未决事项写进 `STATE.md`，处理保留中的 worker 终端，结束。之后由用户按现行流程继续。
