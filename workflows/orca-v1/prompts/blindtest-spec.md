# 测试 agent：规范审查（第 4.2 步）Task spec 模板

内容要求见 CLAUDE.md「开发顺序第 4.1 步写完后」：只写被测对象、要读的规范章节、要对照的锁定测试文件和场景覆盖面。
角色约束由 AGENTS.md「测试分工」规定（副本里有 AGENTS.md），这里不改写。副本准备与 worker 进入副本的方法见 WORKFLOW.md §7.1。

```text
Fengyan_BOH 切片「<切片>」独立测试审查，第一阶段（规范审查）。你的身份与约束见当前目录的 AGENTS.md「测试分工」。

工作目录：<副本绝对路径>（本次审查的全部对象）。
被测对象：<逐条>
要读的规范章节：<docs/… 章节逐条>
要对照的锁定测试文件：<路径逐条>
场景覆盖面：<逐条>

完成后按 Orca preamble 发 worker_done，--report-path 填报告的绝对路径（在 blindtest/ 下）。
需要提问时用 preamble 里的 ask 命令。
```
