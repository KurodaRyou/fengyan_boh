# Orca v1 运行状态：<切片>

<!-- 复制到 workdocs/orca/<切片>/STATE.md。只由协调者修改；每次推进先改这里再派发。过程文档，不是规范。 -->

## 基本信息
- workflow: orca-v1
- 切片: <切片名>　范围: <一句话>
- 分支: slice/<切片>　主检出: <绝对路径>　Orca worktree id: <repoId>::<路径>
- Run: <run_id>　协调者终端: <coordinator_handle>
- 授权（H0_AUTH，原话 + 时间）: 推送 <是/否>；开 PR <是/否>；PR 评论 <是/否>
- 工具: claude <版本>；实现 agent <codex 版本>；测试 agent <gemini|codex（记忆已关闭：人确认原话）>

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

## 黑盒验收
| 阶段 | 副本目录 | 来源提交 | 报告留存位置 | 结论 |
|---|---|---|---|---|

## 已处理消息
<!-- message id 列表，防止重复处理 -->

## 待处理问题
- 
