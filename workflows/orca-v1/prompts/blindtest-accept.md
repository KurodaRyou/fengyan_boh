# 测试 agent：进程级黑盒验收（第 6 步）Task spec 模板

由 review Claude 在分诊时写好（CLAUDE.md「开发顺序第 6 步需要进程级黑盒验收时」：只写涉及五项中的哪几项、要读的规范章节和构建命令）。
协调者原样派发，不增删内容；worker 进入副本的方法见 WORKFLOW.md §7.1。
spec 中不写 `/boost`：协调者发送时在整段 preamble 前加 `/boost `。

```text
Fengyan_BOH 切片「<切片>」进程级黑盒验收。你的身份与约束见当前目录的 AGENTS.md「测试分工」。

工作目录：<隔离环境内副本绝对路径>；来源与构建命令见 blindtest/SOURCE.md。隔离入口与报告路径映射已按 WORKFLOW.md §7.1.1 核验。
本次涉及的项：<配置 / 启动与拒绝启动 / 停止信号与关闭顺序 / 后台任务生命周期 / 命令行子命令 中的若干项>
要读的规范章节：<逐条>
构建命令：<例如 CARGO_TARGET_DIR=blindtest/target cargo build --locked -p boh-server>

完成后经受限中继发送 Orca preamble 中的 worker_done，--report-path 填隔离环境内 blindtest/ 下普通报告文件的绝对路径，由中继按已核验映射导入。
需要提问时经同一中继发送 preamble 中的 ask；不连接宿主 Orca socket。
```
