# 测试 agent：进程级黑盒验收（第 6 步）Task spec 模板

由 review Claude 在分诊时写好（CLAUDE.md「开发顺序第 6 步需要进程级黑盒验收时」：只写涉及五项中的哪几项、要读的规范章节和构建命令）。
协调者原样派发，不增删内容；worker 进入副本的方法见 WORKFLOW.md §7.1。

```text
Fengyan_BOH 切片「<切片>」进程级黑盒验收。你的身份与约束见当前目录的 AGENTS.md「测试分工」。

工作目录：<副本绝对路径>；来源与构建命令见 blindtest/SOURCE.md。
本次涉及的项：<配置 / 启动与拒绝启动 / 停止信号与关闭顺序 / 后台任务生命周期 / 命令行子命令 中的若干项>
要读的规范章节：<逐条>
构建命令：<例如 CARGO_TARGET_DIR=blindtest/target cargo build --locked -p boh-server>

完成后按 Orca preamble 发 worker_done，--report-path 填报告的绝对路径（在 blindtest/ 下）。
需要提问时用 preamble 里的 ask 命令。
```
