# 设计 Claude 工作单（协调者在 S1–S6、S10 使用）

设计 Claude 的职责和要求全部来自 AGENTS.md「开发顺序」第 1–4.3 步、「实现开始后要改设计」和 CLAUDE.md。
本文件只列出每一步在 Orca v1 中**做完的标志**和**交给谁**，不重述规则。

| 步骤 | 做完的标志（写进 STATE.md） | 交给 |
|---|---|---|
| 第 1 步 | 已读 `workdocs/deferred-review-items.md` 中本切片的条目；分支开头的 `docs:` 提交 | 继续第 2 步 |
| 第 2–3 步 | 迁移与 schema 测试已提交；schema 测试通过 | S2_LOCK |
| 第 4.1 步 | 锁定测试和接口说明写好（未提交）；`engineering:testing-strategy` 缺口清单 | 测试 agent（副本 + `prompts/blindtest-spec.md`）；缺口清单给人（H2_GAPS） |
| 第 4.2 步核对 | 逐条核对表（采纳 / 不采纳及理由 / 后续切片）；人确认（H3_ADOPT）后写回；延后项已登记；副本已移到废纸篓 | S5_CONFIRM |
| 预期值确认 | `confirm-<n>.md`（编号、测试名、输入、预期值、锁定测试校验和）；人逐条确认（H4_EXPECTED） | S6_SPEC_COMMIT |
| 第 4.3 步 | 校验和与确认时一致；`spec:` 提交；比对基准；实现 Task spec（`prompts/implement.md`）；review prompt（`prompts/review.md`） | 实现 agent |
| 改设计 | 追加的 `docs:` / `spec:` 提交、rebase、新基准、新 review prompt；旧 review 终端已释放 | 实现 agent，然后新 review worker |

写 prompt 时：
- 实现 Task spec 中的小改动来自 `workdocs/` 时，先确认它仍然适用、不涉及规范，再把内容直接写进 spec（CLAUDE.md、AGENTS.md 第 4.3 步）。
- review prompt 不写设计讨论过程和对实现方式的设想。写完后不要在本会话里评价实现：评价只由 review worker 做。
