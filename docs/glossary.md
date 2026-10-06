# 术语表

本项目文档的规范用词。每个词只给一句话定义，规则以「详见」指向的章节为准。
英文名是规范名，正文可以用中文用词；同一个概念不要换别的说法。

## 开发流程

| 规范名 | 中文用词 | 定义 | 详见 |
|---|---|---|---|
| Vertical slice | 纵向切片，简称切片 | 一次交付的完整功能增量，贯穿迁移、事件、校验、投影、HTTP 接口和锁定测试。它是开发的拆分方式，不是代码架构：本项目不采用 Vertical Slice Architecture，crate 仍按层横向划分 | AGENTS「开发顺序」 |
| Walking skeleton | — | 第一个 vertical slice（温度记录）。业务最简单，用来让一条真实命令完整走通 write path、幂等、重放和 golden payload；之后的切片照它的模式写 | AGENTS「路线图」 |
| Locked test | 锁定测试 | 由 Claude 编写、人确认预期值的测试，实现 agent 不得修改或使其失效 | AGENTS「测试分工」 |
| Locked path | 锁定路径 | `.github/CODEOWNERS` 列出的路径，只能出现在 `spec:` / `docs:` 提交中 | AGENTS「协作分工」 |
| Review baseline | 比对基准 | 切片分支开头连续的 `spec:` / `docs:` 提交中的最后一个；review 时锁定路径相对它的 diff 必须为空 | AGENTS「测试分工」 |
| Golden payload | golden 样本 | 某个 `event_type@schema_version` 的一种 payload 结构分支的完整序列化 JSON，用来保证已发布的 payload 结构不变 | AGENTS「测试分工」 |

## 架构与存储

| 规范名 | 中文用词 | 定义 | 详见 |
|---|---|---|---|
| Write path | 写入路径 | 所有写命令共用的链路，与具体业务无关：`Writer::call` → `BEGIN IMMEDIATE` → 幂等检查 → 业务校验 → `Ledger::append`（写事件并更新投影）→ 写 `processed_commands` → `COMMIT` → 返回信封 | AGENTS「架构」 |
| Single writer | 唯一写连接 | 整个节点只有 `sqlite-writer` 线程持有的一个写连接，所有写操作排队经过它 | AGENTS「SQLite」 |
| Ledger | 账本 | `store_events` 表：只追加的事件流，是全部业务状态的唯一来源 | AGENTS「只追加」 |
| `seq` | 提交顺序 | 账本中事件的连续序号，由 SQLite 分配；事件间的一切顺序只看它 | AGENTS「ID 与时间」 |
| Projection | 投影 | 由事件推导出的当前状态表（库存余量、主数据表等），在写事件的同一事务内更新，可以从账本完整重建 | AGENTS「只追加」 |
| Replay / rebuild | 重放 / 重建 | 清空投影后按 `seq` 把全部事件重新 `projections::apply` 一遍，结果必须与原投影逐行一致 | AGENTS「只追加」 |
| Aggregate | 聚合 | 一组相关事件共用的标识（`aggregate_type` + 聚合 ID），`aggregate_version` 给出组内顺序。只是事件上的字段，不是代码中的类层次 | domain「事件目录」 |
| Upcast | upcast | 读取旧 `schema_version` 的 payload 时，把它转换成当前结构的函数 | AGENTS「只追加」 |
| Idempotency | 幂等 | 同一 `command_id` 重复提交时返回原结果、不重复执行；内容不同时返回 `409 IDEMPOTENCY_CONFLICT` | AGENTS「幂等」 |
| Canonical request | 规范化请求 | 命令反序列化成强类型结构体后再序列化、剥离 `command_id` 和 `sent_at` 得到的文本，用于幂等比对 | AGENTS「幂等」 |
| Envelope | 信封 | 所有 HTTP 响应共用的外层结构：`success`、`data`、`warnings`、`error` | AGENTS「HTTP 约定」 |
| Invariant self-check | 不变量自检 | 定时任务在读连接上检查投影是否满足不变量，`/health` 返回最近一次的结果 | domain「投影表」 |
| Sync cursor | 同步游标 | `sync_state.acked_seq`：总部已确认收到的最大 `seq`，上行同步从它之后推送 | AGENTS「上行同步」 |

## 业务领域

| 规范名 | 中文用词 | 定义 | 详见 |
|---|---|---|---|
| Business date | 营业日 | 门店当地日期，早于日切时刻（默认 `04:00`）的发生时间算前一营业日 | domain「时间」 |
| Relative calibration | 相对校准 | 用平板同一时钟的 `sent_at − captured_at` 加上节点的 `recorded_at`，算出 `occurred_at`，抵消平板时钟偏差 | domain「时间」 |
| Backfill | 补录 | 经显式入口直接提交过去的 `occurred_at`，不走相对校准 | domain「时间」 |
| Lot | 批次 | 一次收货行、生产产出或盘盈形成的一份库存，以 `lot_id` 标识 | domain「批次」 |
| Count-gain lot | 盘盈批次 | 盘点发现实物多于账面时新建的批次，FIFO 分配时最先扣 | domain「批次」 |
| Allocation | 分配 | 把一笔扣减落到具体批次（或账外缺口）的结果，写进 payload 的 `alloc`，重放时不重新计算 | domain「批次」 |
| Off-book shortfall | 账外缺口 | 扣减超过全部批次余量时，未落到批次的不足部分；下一次盘点该物料时清零 | domain「批次」 |
| Count absorption | 盘点吸收 | 实物发生在某次盘点观察时点之前、却在盘点之后才入账的库存影响，已被盘点数计入，因此不再改动库存，只更正报表 | domain「盘点吸收」 |
| Reversal | 冲销 | 追加一条 `EVENT_REVERSED`，整条抵消原事件 | domain「纠错」 |
| Quantity correction | 数量更正 | 追加一条 `QUANTITY_CORRECTED`，只改数量并保持原批次 | domain「纠错」 |
