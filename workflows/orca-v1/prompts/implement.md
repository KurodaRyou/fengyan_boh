# 实现 agent 的 Task spec 模板

由设计 Claude 在第 4.3 步填写，协调者作为 `task-create --spec` 原样派发。spec 必须自包含：实现 agent 不读 `workdocs/`，也看不到协调者的对话。

## 首轮（S7_IMPL）

```text
Fengyan_BOH 切片「<切片>」实现（AGENTS.md「开发顺序」第 5 步）。

Target：隔离内临时 Git 库 <隔离路径>，输入 HEAD <完整 SHA>，比对基准 <完整 SHA>（<提交标题>）。宿主目标分支为 slice/<切片>，你不可访问它。
隔离身份：<已按 WORKFLOW.md §7.1.1 / §7.2 验收的 agent 入口、实例/会话及配置标识>。
无凭据执行器：<入口与调用方式>；输出根：<隔离内路径>；本轮允许改动的非锁定路径：<逐项>。
agent 自身及所有工具均在隔离实例内，只编辑和提交隔离库；不能访问宿主主检出、Git 元数据、凭据或 Orca socket。模型连接用专用最小凭据；仓库脚本、格式化、构建、测试和复现另经无凭据执行器。
入口/身份无法核验时用受限 preamble ask 报告 H9_ISOLATION 并停止，不回退宿主；中继只接收当前 Task/Dispatch 的消息与允许输出。
本切片范围：
- <逐条：命令 / 事件 / 投影 / 接口；对应 docs/ 章节与锁定测试文件>
需要顺带落实的已生效改动：
- <逐条，写明出处：docs/… 章节或锁定测试名；没有写「无」>
不涉及规范的小改动：
- <逐条，内容写全；没有写「无」>

Constraints：遵守 AGENTS.md 全部规则（含「协作分工」中实现 agent 不得修改的锁定路径）；锁定测试有错时停下来在交付说明里提出。
Ownership：只在隔离库提交实现提交；提交信息不得以 spec: 或 docs: 开头。不读取、不引用 workdocs/。不导出 Git 配置/hooks 或 agent 配置，只输出本轮有序补丁系列与普通交付说明；宿主导入由协调者完成。
Observable acceptance：
1. 经无凭据执行器在最终隔离 HEAD 的副本上通过 cargo fmt --all、cargo clippy --workspace --all-targets -- -D warnings、cargo test --workspace、scripts/ci-scan.sh；格式化后提交并重新验证，隔离库工作区干净。
2. 输出 <输出根>/patches/ 中的有序补丁系列，每条列输入 base/parent、树、作者、标题；交付说明写到 <输出根>/delivery-r1.md：文件、开发顺序步骤、未决问题、输入/最终隔离 SHA。协调者按 §7.2 校验导入、记录 SHA 映射，再对宿主新 HEAD 复跑检查，导入前不得声称宿主已交付。
3. 经受限中继发 worker_done：--outcome 如实填写，--report-path 只填上述隔离输出内路径，由中继映射到固定宿主目的地。
需要协调者回答的问题用 preamble 里的 ask 命令，不要在终端里等人回答。
```

## 返修（S9_REWORK，复用原实现终端）

```text
Fengyan_BOH 切片「<切片>」返修第 <k> 轮。

Target：原隔离会话 <实例/会话标识>，已导入本轮宿主 HEAD <完整 SHA>，比对基准 <完整 SHA>，临时 Git 库 <隔离路径>。
隔离身份及输出根：<重新核验的入口/配置、隔离输出路径>；无凭据执行器：<入口/调用方式>；允许改动的非锁定路径：<逐项>。
agent 及所有工具只能在隔离实例内编辑/提交，不能访问宿主主检出、Git 元数据、凭据或 Orca socket；模型用专用最小凭据，构建/测试/格式化/复现另经无凭据执行器。缺证据用受限 ask 报告 H9_ISOLATION，不回退宿主。
需要修改的问题（来自 review，已逐条抄录）：
- R<k>.<n>：<问题、位置 文件:行、依据（AGENTS.md / docs/ 章节）、期望的结果>
不要改动：<review 明确认为不需要改的部分；没有写「无」>

Constraints、Ownership：遵守 AGENTS.md；只改允许的非锁定路径；锁定测试有错时报告。只在隔离库作实现提交，标题不得以 spec: / docs: 开头，不读 workdocs/。只导出本轮有序补丁与普通报告，不导出 Git/agent 配置或 hooks。
Observable acceptance：cargo fmt --all、严格 workspace Clippy、cargo test --workspace、scripts/ci-scan.sh 在最终隔离 SHA 通过；工作区干净。输出补丁系列（每条 base/parent、树、作者、标题）及 <输出根>/delivery-r<k>.md，逐条说明返修项和隔离提交。按受限 preamble 发 worker_done，report-path 只在输出根内；宿主导入、SHA 映射与复跑由协调者按 §7.2 完成。
```

规范变更后（S10）的返修在 spec 开头写明：宿主分支已 rebase，新的比对基准是 <SHA>，隔离库已重新导入相应快照/历史，先核对本轮输入 SHA。冲突解决同样只交付隔离补丁。
