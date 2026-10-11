@AGENTS.md

## Claude 的角色

在本项目中，Claude 负责架构、设计文档和 code review，实现代码主要由其他 agent 完成。
Review 时以 AGENTS.md 的「不可违反的规则」为检查清单，逐条核对；除非用户要求，不直接改实现代码。
本地 review（开发顺序第 6 步）的 `/code-review` 发现与清单重复的不再列；第 6 步的组成与通过条件见 AGENTS.md「开发顺序」。

开发顺序第 1 步：`workdocs/deferred-review-items.md` 存在时（不进版本库，只在本机），先读其中属于本切片的条目：规范结论写进第 1 步的 `docs:` 提交，需要锁定测试的在第 4 步随锁定测试一起审查、确认；全部写回后再删除对应条目。

开发顺序第 4.1 步写完后：先按下表用 `engineering:testing-strategy` 查覆盖缺口，再为测试 agent 准备独立测试审查（AGENTS.md「测试分工」）：
- 副本放在仓库同级的 `../fengyan-blindtest-<切片>-spec/`（命名见下），复制工作区（锁定测试此时尚未提交），排除 `.git`、`workdocs/`、`target/` 和与仓库无关的未跟踪文件（`outputs/`、`.claude/`、`boh-dev.db` 等）。本切片新增的锁定测试也是未跟踪文件，所以按项排除，不整体排除未跟踪文件。
- prompt 只写本切片的被测对象、要读的规范章节、要对照的锁定测试文件和场景覆盖面；角色约束由 AGENTS.md 规定，不在 prompt 中改写。
- 核对报告后，把副本移到废纸篓。

开发顺序第 4.3 步的 review prompt（AGENTS.md「开发顺序」第 6 步）：
- 写明切片分支、比对基准、本切片范围，以及实现 prompt 要求顺带落实的改动和小改动（逐条列出，review 时核对是否落实）；切片需要进程级黑盒验收时写明（AGENTS.md「测试分工」）。
- 不写设计讨论过程和对实现方式的设想；核对清单由 AGENTS.md 和本文件规定，不在 prompt 中改写。
- 末尾留位置给人粘贴实现 agent 的交付说明。交付说明只是实现方的陈述：改动范围以 git 为准，说明中的结论不代替核对。
- 在 review 会话以外收到交付说明时，不开始 review，给出 review prompt。

开发顺序第 6 步需要进程级黑盒验收时（AGENTS.md「测试分工」），在 review 开始时准备副本并给出测试 agent 的 prompt：
- 用 `git archive <HEAD>` 导出 review 时 HEAD 的已提交内容，解压到副本；不复制工作区，未跟踪文件（`outputs/`、`.claude/`、`boh-dev.db` 等）和未提交的改动都不进副本。
- 副本中写 `blindtest/SOURCE.md`：完整提交 SHA、比对基准和实际构建命令。构建产物放在 `blindtest/` 下，不更新锁文件，例如 `CARGO_TARGET_DIR=blindtest/target cargo build --locked -p boh-server`；`blindtest/` 以外与导出内容保持一致。
- prompt 只写本切片涉及的进程行为（配置、启动与拒绝启动、停止信号、子命令中的哪几项）、要读的规范章节和构建命令；角色约束由 AGENTS.md 规定，不在 prompt 中改写。
- 返修后需要补测或重验时，从新的 HEAD 重新导出副本，不在旧副本上改。
- 核对报告后，先把 `blindtest/SOURCE.md`、报告和必要的日志复制到 `workdocs/blindtest/<副本目录名>/`，供之后判断沿用和复审核对，再把副本移到废纸篓；切片 PR 合入后删除这些留存。

副本目录名区分阶段和返修轮次：`../fengyan-blindtest-<切片>-<阶段>[-r<轮次>]/`。阶段是 `spec`（第 4.2 步）或 `accept`（第 6 步），各自计数；首次不加轮次，第一次返修为 `r2`，例如 `fengyan-blindtest-stocktake-accept-r2`。

## Engineering 插件

按以下时机调用，只作补充，不替代本项目规则：

| 时机 | Skill | 用法 |
|---|---|---|
| 开发顺序第 4 步，人确认预期值之前 | `engineering:testing-strategy` | 对照 `docs/domain.md`、`docs/auth.md` 查锁定测试的覆盖缺口，列给人决定是否补 |
| 「尚未设计」事项的方案讨论 | `engineering:architecture`、`engineering:system-design` | 产出只放 `workdocs/`，结论按「文档分层」写回 `docs/` |
| 首次部署门店机之前 | `engineering:deploy-checklist` | 结合 `deploy/`、`docs/backup.md` 与「优雅关闭」生成检查表 |

- 不用 `engineering:documentation`：它的写法与「文档分层」冲突。
- 不用 `engineering:code-review`：与 `/code-review` 重复。
