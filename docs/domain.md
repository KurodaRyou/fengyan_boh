# BOH 领域模型

> 状态：**v2 草案**。第一轮业务问题已答复（见「已确认的业务决策」）。
> 标注 **【建议，待确认】** 的条目是 Claude 给出的默认方案，确认前不要实现依赖它的部分。
> 文末「待确认问题」中的 Q1（营业日）是**所有事件的前置条件**：`store_events.business_date` 非空，营业日规则落地前不能写入任何事件。

## 已确认的业务决策

| # | 问题 | 答复 | 设计影响 |
|---|---|---|---|
| 1 | 销售数据 | POS 销售暂不接入，未来以 CSV / Excel 导入（格式未定）。原料在**生产完成时**扣减 | 本期无销售事件；成品去向靠报损 + 盘点闭环，见「成品与销售」 |
| 2 | 批次粒度 | **全部物料**按批次 / 保质期追踪 | 库存只存在于批次上；扣减由写入线程按 FIFO 自动分配，见「批次」 |
| 3 | 单位 | 存在多单位（箱 / 袋 / 克），换算表由总部维护 | 账本只存基本单位；录入单位与换算系数快照进 payload，见「单位」 |
| 4 | 生产扣料 | 默认按配方自动扣，允许员工修改实际用量 | payload 同时记录 `planned_qty` 与实际 `qty` |
| 5 | 调拨 | 暂不存在 | `TRANSFER_*` 移到「未来功能」 |
| 6 | 操作人 | 需要，员工上班时登录终端 | 每条事件记录 `actor_id`（登录员工）与 `device_id`；登录方式见 Q4 |
| 7 | 总部 | 暂不开发总部，纯本地运行；为未来下发预留接口 | 主数据本期在门店本地维护，表结构按「未来由总部覆盖」设计，见「主数据」 |
| 8 | 盘点后的更正 | 被盘点覆盖过的行不再动库存，只更正报表（吸收） | 见「盘点吸收」 |
| 9 | 补录 | 允许补录到过去，记录精确的发生时间 | 事件新增 `occurred_at`，`business_date` 由它推出，见「发生时间与补录」 |
| 10 | 数量录错 | 按差额更正，保持原批次不变 | 新增 `QUANTITY_CORRECTED`，见「纠错规则」 |
| 11 | 冲销 / 更正时批次已被消耗 | 不拒绝：批次扣到 0，不足部分进账外缺口，提示盘点 | 见「纠错规则」；依赖账外缺口机制（Q2） |

## 两类数据

| 类别 | 例子 | 存储方式 | 来源 |
|---|---|---|---|
| **账本**（发生过的事） | 收货、生产、报损、盘点 | `store_events` 只追加 + 投影表 | 门店产生；outbox 照常写入，上行同步本期不实现 |
| **主数据**（当前定义） | 物料、单位换算、配方、供应商、员工、报损原因码、设备 | 普通状态表，整行覆盖 | **本期门店本地维护**；未来由总部下发覆盖 |

事件 payload 中引用主数据时，**同时快照计算所需的值**（换算系数、配方用量、分配到的批次）。
重放事件时只读 payload，**不再查询主数据、不重新计算**，以保证总部改了配方 / 换算后重放结果仍然一致。

## 主数据

| 表（计划） | 主键 | 关键字段 |
|---|---|---|
| `items` | `item_id` | `name`, `base_unit`（`g` / `ml` / `pcs`）, `kind`（`RAW` / `SEMI` / `FINISHED`）, `default_shelf_life_ms?`, `active` |
| `item_units` | `(item_id, unit_code)` | `base_qty_per_unit`（`i64 > 0`，例：1 箱面粉 = 25000 g） |
| `recipes` | `recipe_id` | `output_item_id`, `current_version`, `active` |
| `recipe_versions` | `(recipe_id, version)` | `yield_qty`（每批产出，基本单位）, `lines[{item_id, qty_per_batch}]`。**版本只增不改** |
| `suppliers` | `supplier_id` | `name`, `active` |
| `employees` | `employee_id` | `name`, `role`, `active`（登录凭据见 Q4） |
| `waste_reasons` | `reason_code` | `label`, `active`。预置：`EXPIRED`、`DAMAGED`、`PRODUCTION_DEFECT`、`TASTING`（试吃）、`OTHER` |
| `equipment` | `equipment_id` | `name`, `kind`（冷柜 / 烤箱…）, `active` |

为未来总部下发预留的约定：
- 主键一律 UUIDv7（`reason_code` / `unit_code` 这类稳定代码除外），门店本地创建的行将来可原样被总部收编，不需要 ID 映射。
- 每行带 `revision INTEGER`（每次修改 +1）和 `updated_at`，作为将来下行同步的游标 / 冲突判断依据。
- 主数据**不删除，只停用**（`active = 0`），因为历史事件仍引用它。
- 主数据写入同样走 `Writer::call`，但**不产生 `store_events`**、不进 outbox。
- 本期通过本地管理接口维护（`/api/v1/admin/...`），权限见 Q4。未来接入总部后，这些接口关闭或只读。

## 单位

- 每个物料只有一个**基本单位**（`g` / `ml` / `pcs`）。账本、投影中的所有数量都是基本单位的 `i64`。
- 命令可以用任意已配置单位提交：`{ qty, unit_code }`。写入线程查 `item_units` 换算成基本单位，
  payload 同时写入换算后的 `qty` 与录入快照 `input { qty, unit_code, base_qty_per_unit }`。
- 换算系数必须是整数基本单位，不支持小数换算（如需要 1 磅 ≈ 454 g，由总部定义取整后的值）。
- 未配置的 `unit_code` → `400 UNKNOWN_UNIT`。

## 批次

全部物料都按批次追踪，**库存只存在于批次上**。

- **批次 ID**：每条收货行、每次生产产出由写入线程生成一个 `lot_id`（UUIDv7），写进 payload。供应商自己的批号是可选属性 `supplier_lot_no`，不作主键（同批号不同到期日的情况不会冲突）。
- **到期时间**：统一为 `expires_at`（UTC 毫秒，可空）。只有日期的保质期由**客户端**换算成门店当地当日结束时刻再提交。
  收货 / 生产未填写时，可用 `items.default_shelf_life_ms` 推算，推算结果写进 payload。
- **扣减分配**：所有减少库存的行（生产消耗、报损），客户端可以指定 `lot_id`；不指定时写入线程按 **FIFO** 分配：
  批次入库时间 `received_at` 升序（= 来源事件的 `occurred_at`，补录的收货按补录的发生时间排）→ `lot_id` 升序。
  分配结果作为 `allocations[{lot_id, qty}]` 写进 payload，重放时直接使用。指定的批次余量不足时，不足部分继续按 FIFO 分配。
- **账面不足**【建议，待确认】：所有批次都分配完仍不够时，**不拒绝命令**（现实中东西已经用掉了，系统不能挡住生产），
  剩余部分记为 `{ lot_id: null, qty }`，计入该物料的「账外缺口」投影（负数），由下一次盘点清零。
  同时在响应中返回警告 `STOCK_SHORTFALL`（`success` 仍为 `true`）。
- 不变量：批次余量 `remaining_qty >= 0`；账外缺口 `<= 0`；物料账面数 = 批次余量之和 + 账外缺口。

## 事件目录（本期）

所有数量是基本单位 `i64`，金额是 `i64` 分。`schema_version` 从 1 开始。
所有事件的 `actor_id`、`device_id`、`occurred_at`、`business_date` 存在 `store_events` 列中，不重复写进 payload（见「发生时间与补录」）。
下表中 `alloc` 指 `allocations[{lot_id | null, qty}]`，`input` 指单位录入快照（见「单位」）。
所有影响库存的行（收货行、生产的 `consumed` 行和 `output`、报损行、冲销行、更正行）都带 `absorbed_by`（见「盘点吸收」）；
被吸收的行没有 `alloc`，也不生成 `lot_id`。

| event_type | aggregate_type | 含义 | payload 要点 | 投影影响 |
|---|---|---|---|---|
| `GOODS_RECEIVED` | `RECEIPT` | 供应商到货 | `supplier_id`, `lines[{item_id, qty, input, lot_id, supplier_lot_no?, expires_at?, line_cost_cents}]` | 每行新建一个批次 |
| `PRODUCTION_BATCH_COMPLETED` | `PRODUCTION_BATCH` | 一批产品完成 | `recipe_id`, `recipe_version`, `batch_count`, `output{item_id, planned_qty, qty, lot_id, expires_at?}`, `consumed[{item_id, planned_qty, qty, alloc}]` | 原料按 alloc 扣减；成品新建一个批次 |
| `WASTE_LOGGED` | `WASTE_RECORD` | 报损 / 试吃 | `lines[{item_id, qty, input, reason_code, alloc}]` | 按 alloc 扣减 |
| `STOCK_COUNT_SUBMITTED` | `STOCK_COUNT` | 盘点结果（绝对值） | `purpose`（`CLOSING` / `AUDIT`）, `lines[{item_id, lot_id \| null, counted_qty}]` | 无直接影响，见「盘点」 |
| `STOCK_ADJUSTED` | `STOCK_COUNT` | 盘点差异调整 | `lines[{item_id, lot_id \| null, book_qty, counted_qty, delta}]`, `new_lots[{lot_id, item_id, qty, expires_at?}]` | 批次 / 账外缺口 ± delta；新建盘盈批次 |
| `PURCHASE_ORDER_SUBMITTED` | `PURCHASE_ORDER` | 向供应商订货（本地记录） | `supplier_id`, `lines[{item_id, qty, input}]`, `deliver_on` | 无 |
| `TEMPERATURE_LOGGED` | `EQUIPMENT` | 冷柜 / 烤箱温度记录 | `equipment_id`, `celsius_x10`, `note?` | 食安记录 |
| `EVENT_REVERSED` | 与原事件相同 | 冲销一条录错的事件 | `reversed_event_id`, `reason`, `lines[{item_id, qty, alloc, absorbed_by}]` | 逐行取反或被盘点吸收，见「纠错」 |
| `QUANTITY_CORRECTED` | 与原事件相同 | 更正录错的数量 | `corrected_event_id`, `reason`, `lines[{line_ref, item_id, old_qty, new_qty, delta, input, alloc, line_cost_cents?, absorbed_by}]` | 按差额调整，见「纠错」 |

`absorbed_by` 的结构：`null`（未被吸收）或 `{event_id, business_date, purpose}`（吸收它的那次 `STOCK_ADJUSTED`）。

### 发生时间与补录
每条事件有两个时间：

- `created_at`：写入线程的录入时间，单调递增（见 AGENTS.md）。只用于排序重放，不代表业务时间。
- `occurred_at`：事情实际发生的时间（UTC 毫秒）。`business_date` 由 `occurred_at` 按门店时区和日切时间推出（见 Q1）。

规则：

- **实时录入**：命令不带 `occurred_at`，写入线程取 `occurred_at = created_at`。
- **补录**：收货、生产、报损、订货、温度记录的命令可以带过去的 `occurred_at`。写入线程校验：
  - 不晚于当前时间，否则 `400 INVALID_OCCURRED_AT`；
  - 推出的 `business_date` 晚于锁账日，否则 `409 BOOKS_LOCKED`（见下）。
- **盘点不允许补录**：盘点命令不接受 `occurred_at`，盘点时间就是录入时间。
- **冲销的发生时间 = 原事件的 `occurred_at`**，由写入线程填写，客户端不能指定。所以冲销的 `business_date` 就是原事件的营业日。
- **客户端交互**：补录时，如果所选那天该物料有盘点，界面提示「X 点盘过该物料，这次发生在盘点前还是盘点后？」，
  按选择把 `occurred_at` 设在那次盘点时间的对应一侧。员工不需要记准确时刻。
  从冲销发起的「重新录入」，`occurred_at` 默认带原事件的值。
- **锁账日**【建议，待确认】：门店配置 `books_locked_through`（`'YYYY-MM-DD'`，空 = 不锁）。
  `business_date` 不晚于它的补录和冲销都被拒绝（`409 BOOKS_LOCKED`）。将来由总部关账后下发。见 Q6。

### 生产规则
- 计划用量 = 配方版本中 `qty_per_batch × batch_count`；计划产出 = `yield_qty × batch_count`。只有整数乘法，没有除法和舍入。
- 员工可以修改每行实际 `qty`、增加配方外的物料（`planned_qty = 0`）、把某行改为 0；也可以修改实际产出 `qty`（烤坏的直接少计产出，不另记报损）。
- 半成品（面团、馅料）也是物料，可以作为另一个配方的原料，规则相同。

### 盘点规则
盘点给出的是**绝对值**，与「只追加增量」冲突，所以拆成两条事件，在同一事务内写入：
1. `STOCK_COUNT_SUBMITTED`：记录员工数到的数。
2. `STOCK_ADJUSTED`：写入时读取当前投影的账面数 `book_qty`，`delta = counted_qty - book_qty`。

- 盘点以**物料**为单位：出现在 `lines` 中的物料，它所有账面非零的批次都视为被盘点，未列出的批次按 `counted_qty = 0` 处理；
  该物料的账外缺口同时清零（`lot_id: null` 行）。未出现的物料完全不动（允许部分盘点 / 循环盘点）。
- 数到了系统里没有的货：提交 `lot_id: null, counted_qty > 0`，写入线程新建一个盘盈批次，写进 `new_lots`。
- `book_qty` 作为快照写进 payload，重放时直接用 `delta`，不重新计算，以保证确定性。

### 成品与销售【建议，待确认】
本期没有销售数据，成品账面只会因生产增加。闭环方式：
1. 闭店时，当天不能再卖的成品先按 `WASTE_LOGGED`（`EXPIRED` 等）报损；
2. 再对成品做 `purpose = CLOSING` 的盘点，盘点差异（负的 `delta`）即**推定售出**，在 `daily_item_summary` 中单独统计。

将来接入销售导入后，销售会成为独立事件扣减成品，闭店盘点差异回归「真实差异」的含义，旧事件不受影响。

### 盘点吸收
盘点按物料给出绝对值。一条影响库存的行，如果它发生之后该物料被盘点过，它的数量影响已经包含在那次盘点的实数里了，
再动库存就会重复计算。这种行被那次盘点**吸收**。补录、冲销、冲销后的重新录入都用同一条规则。

**判定**（写入时逐行进行）：查找该物料 `occurred_at` **严格晚于**这一行 `occurred_at` 的**第一次** `STOCK_ADJUSTED`（查 `item_count_log`）。
实时录入的行发生在所有已有盘点之后，永远不会被吸收。

- **找到 → 吸收**：`absorbed_by` 记录那次盘点的 `event_id`、`business_date`、`purpose`。**库存投影不动**（不分配批次、不新建批次、不动账外缺口）。
  报表上，这一行照常计入自己 `business_date` 的类别（收货 / 生产产出 / 生产消耗 / 报损），
  同时把**相反数量**计入那次盘点的差异（`purpose = AUDIT` 计入盘点调整，`CLOSING` 计入推定售出）。剩下的盘点差异就是真实差异。
  - 例（补录）：上午扔了 50 忘了录，下午盘点 delta = −50。补录报损 50（发生在盘点前）→ 被吸收：报损 +50，盘点调整 +50 → 0。
  - 例（冲销 + 重新录入）：报损误录 500（实际 50），之后盘点 delta = +450。冲销被吸收：报损 −500，盘点调整 −500；
    重新录入报损 50（发生时间同原事件）被吸收：报损 +50，盘点调整 +50。结果报损 50、盘点调整 0，库存全程不动。
  - 例（成品）：成品误报损 10，闭店盘点推定售出 50。冲销被吸收：报损 −10、推定售出 +10 → 60。
- **没找到 → 正常生效**：`absorbed_by = null`，照常分配 / 新建批次（冲销则为精确取反，见下）。
- 同一条事件里可以两种行混在一起（例如生产的原料盘过、成品没盘过）。
- 判定结果写进 payload，重放时直接使用，不重新判定（与盘点 `book_qty` 快照同一原则）。

### 纠错规则
不提供「编辑」接口。两种纠错方式，都是追加事件，`occurred_at` 由写入线程填为原事件的值，客户端不能指定：

| 情况 | 用法 |
|---|---|
| 物料、供应商、配方都对，只是**数量**错了 | `QUANTITY_CORRECTED`：按差额调整，原批次保持不变 |
| 物料 / 供应商 / 配方录错、重复提交、整条不该存在 | `EVENT_REVERSED` 整条冲销，需要的话再重新录入（`occurred_at` 同原事件） |

通用规则：

- 适用事件：`GOODS_RECEIVED`、`PRODUCTION_BATCH_COMPLETED`、`WASTE_LOGGED` 两种都可以；
  `PURCHASE_ORDER_SUBMITTED`、`TEMPERATURE_LOGGED` 只能冲销（`lines` 为空）。
- 盘点类事件、`EVENT_REVERSED`、`QUANTITY_CORRECTED` 本身不能被冲销或更正（盘点录错就重新盘点）。
- 已被冲销的事件不能再冲销或更正（`409 ALREADY_REVERSED`）。同一事件可以被更正多次。
- 冲销和更正都与原事件属于同一 aggregate，原事件的**当前有效状态** = 原事件 + 之后的全部更正，
  按 `aggregate_version` 顺序读取该 aggregate 的事件得到，不需要额外投影。
- 每行按「盘点吸收」判定（发生时间 = 原事件的 `occurred_at`）。被吸收的行不动库存，只更正报表。
- **扣回时批次余量不够**（原批次或原分配的批次已被后续扣减）：批次扣到 0，不足部分计入账外缺口，
  返回 `STOCK_SHORTFALL` 警告，不拒绝。界面应提示「建议现在盘点该物料」。
  下一次包含该物料的盘点会把所有批次和账外缺口重置为实数，差额自动清零，不需要人工处理。

**数量更正 `QUANTITY_CORRECTED`**：

- `line_ref` 定位原事件中的一行：`lines[i]`（收货、报损）、`consumed[i]`、`output`（生产），`i` 为原数组下标。
  `item_id` 必须与该行一致（不一致 → `400 LINE_MISMATCH`），物料录错要用冲销。
- `old_qty` 是该行当前有效数量的快照，`delta = new_qty - old_qty`，`delta ≠ 0`，`new_qty >= 0`。新数量照常带单位录入快照 `input`。
- 收货行可同时更正 `line_cost_cents`。生产只更正数量，不更正 `batch_count` / 配方（录错配方要冲销）。
- 未被吸收的行按差额调整库存，分配结果写进 `alloc`：

  | 行类型 | `delta > 0` | `delta < 0` |
  |---|---|---|
  | 新建批次的行（收货行、生产 `output`） | 原批次 + delta | 原批次扣回，不够部分进账外缺口 |
  | 扣减库存的行（生产 `consumed`、报损行） | 按 FIFO 追加分配（与新扣减相同） | 按该行有效分配的**逆序**退回（最后分配的先退，`lot_id: null` 部分退回账外缺口） |

- 例：面粉收货录成 1000（实际 100），生产已从 L1 扣 300，L1 剩 700，之后没盘点。更正为 100，`delta = −900`：
  L1 扣 700 到 0，剩余 200 进账外缺口，账面合计 −200，返回 `STOCK_SHORTFALL`。
  账面为负说明另有 200 实际来自其他批次（其他批次账面多了 200）。下次盘点面粉时：其他批次按实数减 200，账外缺口 +200 清零，
  物料合计差异为 0——这笔错误已由更正解释，不会在盘点差异里重复出现。

**整条冲销 `EVENT_REVERSED`**：

- 逐行处理原事件中影响库存的每一行，按原顺序写进 `lines`，数量为该行**当前有效数量**（含之前的更正）。
- 未被吸收的行精确取反：扣减类的行按有效分配把数量加回同一批次 / 账外缺口；新建批次的行把有效数量从原批次扣回（不够部分进账外缺口，见上）。

**报表日期**：所有事件（包括冲销、更正）按自己的 `business_date` 计入 `daily_item_summary`；
冲销和更正的营业日就是原事件的营业日，所以更正自然回写到原来那天。吸收行的盘点差异更正计入 `absorbed_by.business_date`。

## 投影表（计划）

| 表 | 主键 | 内容 |
|---|---|---|
| `inventory_lots` | `lot_id` | `item_id`, `remaining_qty (>= 0)`, `expires_at?`, `supplier_lot_no?`, `source_event_id`, `received_at`（来源事件的 `occurred_at`，FIFO 排序用） |
| `inventory_unallocated` | `item_id` | 账外缺口 `qty (<= 0)` |
| `inventory_on_hand` | — | **视图**，不是表：按物料汇总批次余量 + 账外缺口 |
| `daily_item_summary` | `(business_date, item_id)` | 当日收货 / 生产产出 / 生产消耗 / 报损 / 盘点调整 / 闭店推定售出（按事件的 `business_date`；含吸收行对盘点差异的更正，见「盘点吸收」） |
| `item_count_log` | `(item_id, event_id)` | 每次 `STOCK_ADJUSTED` 涉及的物料：`occurred_at`, `business_date`, `purpose`；按 `(item_id, occurred_at)` 建索引，供吸收判定查「某时刻之后第一次盘点」 |

每张投影表都必须能通过 `boh-server rebuild-projections`（待实现）从 `store_events` 按 `created_at` 顺序完整重建。

## 未来功能（本期不实现）

- **销售导入**：POS 销售以 CSV / Excel 导入，格式未定。
- **门店调拨**：`TRANSFER_SENT` / `TRANSFER_RECEIVED`，含在途归属规则。
- **总部同步**：上行推送 outbox、下行主数据覆盖。届时本地主数据管理接口关闭或改为只读。
  - 总部从事件自行计算报表，必须区分吸收行（`absorbed_by` 非空）和正常生效的行。
  - 补录和冲销会落到过去的营业日，与离线门店迟到的事件一样会修改历史日期。总部关账后下发 `books_locked_through`，
    门店不再接受该日及之前的补录 / 冲销。关账前已产生、关账后才到达总部的事件如何处理
    （常规做法：作为前期调整记入当前未关账期间），在总部设计时确定。门店侧事件格式已自包含，不需为此改动。
  - 吸收行引用的 `STOCK_ADJUSTED` 是另一个 aggregate，可能晚到，所以 `absorbed_by` 自带 `business_date` 和 `purpose`，
    总部不需等待那条盘点事件。
- **采购对接**：采购单与收货的关联、在途库存。

## 待确认问题

1. **营业日（阻塞项）**：门店时区和日切时间。建议在门店配置文件中设置 `timezone`（如 `Asia/Shanghai`）和 `business_day_cutoff`（如 `04:00`，凌晨 4 点前的操作算前一营业日），由写入线程用 `jiff` 从 `occurred_at` 计算 `business_date`。是否接受？
2. **账面不足**：同意「不拒绝、记入账外缺口、盘点清零」吗？（见「批次」）#11 已采用同一机制，若此条不接受，#11 需重新讨论。
3. **成品闭店盘点**：同意用「报损 + 闭店盘点差异」推定成品售出吗？（见「成品与销售」）
4. **员工登录**：用 4–6 位 PIN 还是刷工牌？登录会话多久过期（整个班次 / 闲置 N 分钟）？主数据管理接口是否只允许店长角色？
5. ~~冲销时批次已被消耗~~：已确认，见「已确认的业务决策」#11。
6. **锁账日**：本期 `books_locked_through` 由谁设置——门店配置文件，还是店长通过管理接口设置？是否需要默认的补录期限（例如最多补录 7 天内）？（见「发生时间与补录」）
