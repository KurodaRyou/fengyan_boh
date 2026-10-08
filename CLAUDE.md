@AGENTS.md

## Claude 的角色

在本项目中，Claude 负责架构、设计文档和 code review，实现代码主要由其他 agent 完成。
Review 时以 AGENTS.md 的「不可违反的规则」为检查清单，逐条核对；除非用户要求，不直接改实现代码。
本地 review（开发顺序第 6 步）在清单核对之后再跑 `/code-review`，补查清单以外的问题，与清单重复的不再列；两者都通过才给出可以提 PR 的结论。

开发顺序第 1 步：先读 `workdocs/deferred-review-items.md` 中属于本切片的条目，写回 `docs/` 或锁定测试后删除对应条目。

开发顺序第 4.1 步写完后：先按下表用 `engineering:testing-strategy` 查覆盖缺口，再为测试 agent 准备独立测试审查（AGENTS.md「测试分工」）：
- 副本放在仓库同级的 `../fengyan-blindtest-<切片>/`，复制工作区（锁定测试此时尚未提交），排除 `.git`、`workdocs/`、`target/`。
- prompt 只写本切片的被测对象、要读的规范章节、要对照的锁定测试文件和场景覆盖面；角色约束由 AGENTS.md 规定，不在 prompt 中改写。
- 核对报告后，把副本移到废纸篓。

## Engineering 插件

按以下时机调用，只作补充，不替代本项目规则：

| 时机 | Skill | 用法 |
|---|---|---|
| 开发顺序第 4 步，人确认预期值之前 | `engineering:testing-strategy` | 对照 `docs/domain.md` 查锁定测试的覆盖缺口，列给人决定是否补 |
| 「尚未设计」事项的方案讨论 | `engineering:architecture`、`engineering:system-design` | 产出只放 `workdocs/`，结论按「文档分层」写回 `docs/` |
| 首次部署门店机之前 | `engineering:deploy-checklist` | 结合 `deploy/`、`docs/backup.md` 与「优雅关闭」生成检查表 |

- 不用 `engineering:documentation`：它的写法与「文档分层」冲突。
- 不用 `engineering:code-review`：与 `/code-review` 重复。
