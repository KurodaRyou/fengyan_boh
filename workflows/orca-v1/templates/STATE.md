# Orca v1 运行状态：<切片>

<!-- 复制到 workdocs/orca/<切片>/STATE.md。只由协调者修改；每次推进先改这里再派发。过程文档，不是规范。 -->

## 基本信息
- workflow: orca-v1
- 切片: <切片名>　范围: <一句话>
- 分支: slice/<切片>　主检出: <绝对路径>　Orca worktree id: <repoId>::<路径>
- Run: <run_id>　协调者终端: <coordinator_handle>
- 授权（H0_AUTH，原话 + 时间）: 推送 <是/否>；开 PR <是/否>；PR 评论 <是/否>
- 工具: claude <版本>；实现 agent <codex 版本>；测试 agent <agy 版本（boost）| codex（关闭记忆）及换用原因>

## 当前
- stage: <S0_INIT … S13_CLOSE>
- wait: <none | workers | human:<关口> | blocked:<原因>>
- HEAD: <完整 SHA>（核对时间）
- 比对基准: <完整 SHA>（<提交标题>）
- 下一动作: <具体命令或要问人的一句话>

## 角色与 Orca 对应
| 角色 | 轮次 | Task | Dispatch | 终端 | 状态（照抄 Orca） |
|---|---|---|---|---|---|
| 测试 agent（spec） | 1 | | | | |
| 实现 agent | r1 | | | | |
| review Claude | 分诊 r1 | | | | |

## 人工确认
| 关口 | 时间 | 人的原话 | 覆盖的条目 / 版本（校验和、提交） |
|---|---|---|---|

## 检查与结论
| 时间 | HEAD | 内容（四项检查 / 锁定路径 / review verdict / CI） | 结果与出处 |
|---|---|---|---|

## 全角色隔离与导入
<!-- 没有通过 WORKFLOW.md §7.1.1 / §7.2 时记录 human:H9_ISOLATION；构建隔离不代表 agent 进程已隔离。 -->
| 阶段/角色 | 隔离实例/镜像 | 入口/配置哈希 | 输入 SHA/内容清单哈希 | 内外路径映射 | 探针结果 | 中继版本与 Task/Dispatch 绑定 |
|---|---|---|---|---|---|---|

| 角色/会话 | 输入宿主 SHA | 输出类型/暂存哈希 | 导入器版本/校验结果 | 隔离 SHA → 宿主 SHA/树 | 最终宿主 SHA 检查 | 保留/销毁证据 |
|---|---|---|---|---|---|---|

## 黑盒验收
| 阶段 | 副本目录 | 来源提交 | 报告留存位置 | 结论 |
|---|---|---|---|---|

## 已处理消息
<!-- message id 列表，防止重复处理 -->

## GitHub review 轮次
<!-- 完成证据与发现去重分开；按 WORKFLOW.md §8.7 核对，旧 HEAD 或旧轮次不放行。 -->
- PR: <号/URL>　轮次: <n>　目标 HEAD: <完整 SHA>
- 要求的类型: <code / code + security；配置依据>
- 触发前快照: <review id；汇总评论 id/updated_at/正文哈希及各类型完成状态>
- 前轮终态/串行证据或本轮 run 关联: <含 push 自动触发；未归属运行存在时阻塞>
- 触发: <PR 创建或评论 id/URL/GitHub 时间>　状态: <pending / confirmed / unknown>
| 类型 | 状态 | 完成记录 id/URL | 完整 SHA（短 SHA 解析依据） | 完成时间 | updated_at/正文哈希 | 相对触发前新增/变化 |
|---|---|---|---|---|---|---|
| code | waiting | | | | | |
| security（启用时） | waiting | | | | | |
- 完成后的全量读取与处理结论: <时间；集合/id/updated_at/正文哈希 → 处理结论>
- 推进前复核: <PR HEAD；CI check 对应 SHA；Claude PASS HEAD；各类 Codex 完成 SHA>

## 待处理问题
- 
