# 实现 agent 的 Task spec 模板

由设计 Claude 在第 4.3 步填写，协调者作为 `task-create --spec` 原样派发。spec 必须自包含：实现 agent 不读 `workdocs/`，也看不到协调者的对话。

## 首轮（S7_IMPL）

```text
Fengyan_BOH 切片「<切片>」实现（AGENTS.md「开发顺序」第 5 步）。

Target：主检出 <绝对路径>，分支 slice/<切片>，在比对基准 <完整 SHA>（<提交标题>）之上开发。
本切片范围：
- <逐条：命令 / 事件 / 投影 / 接口；对应 docs/ 章节与锁定测试文件>
需要顺带落实的已生效改动：
- <逐条，写明出处：docs/… 章节或锁定测试名；没有写「无」>
不涉及规范的小改动：
- <逐条，内容写全；没有写「无」>

Constraints：遵守 AGENTS.md 全部规则（含「协作分工」中实现 agent 不得修改的锁定路径）；锁定测试有错时停下来在交付说明里提出。
Ownership：只提交实现提交；提交信息不得以 spec: 或 docs: 开头。不读取、不引用 workdocs/。
Observable acceptance：
1. 在最终 HEAD 上通过 cargo fmt --all、cargo clippy --workspace --all-targets -- -D warnings、cargo test --workspace、scripts/ci-scan.sh；工作区干净。
2. 交付说明写到 <主检出的上级目录>/fengyan-orca-<切片>/delivery-r1.md：改了哪些文件、对应「开发顺序」哪几步、未决问题、最终 HEAD。
3. 按 Orca preamble 发 worker_done：--outcome 如实填写，--report-path 填上面的交付说明路径。
需要协调者回答的问题用 preamble 里的 ask 命令，不要在终端里等人回答。
```

## 返修（S9_REWORK，复用原实现终端）

```text
Fengyan_BOH 切片「<切片>」返修第 <k> 轮。

Target：主检出 <绝对路径>，分支 slice/<切片>，当前 HEAD <完整 SHA>，比对基准 <完整 SHA>。
需要修改的问题（来自 review，已逐条抄录）：
- R<k>.<n>：<问题、位置 文件:行、依据（AGENTS.md / docs/ 章节）、期望的结果>
不要改动：<review 明确认为不需要改的部分；没有写「无」>

Constraints、Ownership 同首轮。
Observable acceptance：四项检查在新 HEAD 上通过；交付说明写到 <…>/fengyan-orca-<切片>/delivery-r<k>.md，逐条说明每个 R<k>.<n> 的处理和对应提交；按 preamble 发 worker_done。
```

规范变更后（S10）的返修在 spec 开头写明：分支已 rebase，新的比对基准是 <SHA>，先 `git status` / `git log` 确认本地状态。
