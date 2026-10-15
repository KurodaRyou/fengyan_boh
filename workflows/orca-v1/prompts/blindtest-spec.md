# 测试 agent：规范审查（第 4.2 步）Task spec 模板

内容要求见 CLAUDE.md「开发顺序第 4.1 步写完后」：只写被测对象、要读的规范章节、要对照的锁定测试文件和场景覆盖面。
角色约束由 AGENTS.md「测试分工」规定（副本里有 AGENTS.md），这里不改写。副本准备与 worker 进入副本的方法见 WORKFLOW.md §7.1。
spec 中不写 `/boost`：协调者发送时在整段 preamble 前加 `/boost `。

```text
Fengyan_BOH 切片「<切片>」独立测试审查，第一阶段（规范审查）。你的身份与约束见当前目录的 AGENTS.md「测试分工」。

工作目录：<隔离环境内副本绝对路径>（本次审查的全部对象）；隔离入口与报告路径映射已按 WORKFLOW.md §7.1.1 核验。
被测对象：<逐条>
要读的规范章节：<docs/… 章节逐条>
要对照的锁定测试文件：<路径逐条>
场景覆盖面：<逐条>

完成后经受限中继发送 Orca preamble 中的 worker_done，--report-path 填隔离环境内 blindtest/ 下普通报告文件的绝对路径，由中继按已核验映射导入。
需要提问时经同一中继发送 preamble 中的 ask；不连接宿主 Orca socket。
```
