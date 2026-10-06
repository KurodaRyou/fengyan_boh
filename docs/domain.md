# BOH 领域模型

本文件定义业务决策、事件目录、投影与业务规则。工程规则见 [AGENTS.md](../AGENTS.md)。
实现落地后，字段与表结构以代码为准（`boh-domain` 结构体、迁移 SQL），本文件只保留语义与不变量。
「待确认问题」中的条目，确认前不要实现依赖它的部分。

## 已确认的业务决策

| # | 主题 | 结论 | 详见 |
|---|---|---|---|
| 1 | 销售 | 每晚导入 POS 导出的 CSV / Excel，只进报表，不扣库存 | 销售导入 |
| 2 | 批次 | 全部物料按批次追踪；未指定批次的扣减按 FIFO 分配，盘盈批次最先扣 | 批次 |
| 3 | 单位 | 多单位，账本只存基本单位；离线期间换算系数变了，命令返回 409 由人工确认 | 单位 |
| 4 | 生产扣料 | 按配方自动扣，员工可改实际用量；生产界面记录开始时间 | 生产规则 |
| 5 | 盘点粒度 | 按物料下的各批次逐行清点数量；库存明细界面同样按批次逐行显示 | 盘点 |
| 6 | 盘点后的影响 | 实物发生在盘点之前、入账在盘点之后的影响，被那次盘点吸收：不动库存，只更正报表 | 盘点吸收 |
| 7 | 补录 | 允许通过补录入口填写过去的发生时间 | 时间 |
| 8 | 录错 | 只有数量错用数量更正（保持原批次）；其他错误整条冲销 | 纠错 |
| 9 | 账面不足 | 不拒绝：批次扣到 0，不足部分记入账外缺口，返回警告，下次盘点清零。冲销 / 更正时批次已被消耗也按此处理 | 批次、纠错 |
| 10 | 营业日 | 时区 `Asia/Shanghai`，日切 `04:00` | 时间 |
| 11 | 操作人 | 员工在终端上用 PIN 登录；平板需先注册；会话按班次有效 | 主数据、待确认问题 |
| 12 | 总部 | 本期不开发。主数据由总部维护统一的 JSON 主数据包，`code` 和 UUID 全部门店统一 | 主数据 |
| 13 | 不存在的业务 | 调拨、套餐、半批生产本期都不存在 | 未来功能 |

## 时间

| 字段 | 含义 | 来源 | 用途 |
|---|---|---|---|
| `seq` | 提交顺序 | SQLite 分配 | 重放、FIFO、同步。**顺序只看 `seq`** |
| `recorded_at` | 门店接收时间 | 系统时钟原值，不做钳制 | 审计、展示 |
| `occurred_at` | 业务发生时间 | 相对校准，或补录时由员工填写 | 计算 `business_date`；吸收判定 |
| `business_date` | 营业日 | 由 `occurred_at` 计算；`SALES_IMPORTED` 例外，由命令显式给出 | 报表归属 |

**相对校准**（实时录入的默认方式）：客户端录入时记 `captured_at`，发送时带 `sent_at`，两者都是平板本地时钟。服务端计算

```
lag         = sent_at − captured_at
occurred_at = recorded_at − lag
```

两个时间来自同一块时钟，相减后绝对偏差抵消。

- `lag < 0`：按 0 处理，返回警告 `CAPTURE_TIME_ADJUSTED`。
- `lag > 72h`：`400 CAPTURE_TOO_OLD`，改走补录入口。
- 生产命令另带 `started_captured_at`，同样换算出 `started_at` 写进 payload。

**补录**（显式入口，Q6 确认后实现）：

- 收货、生产、报损、订货、温度记录的命令可以显式提交 `occurred_at`（生产同时提交 `started_at`），不走相对校准。
- `occurred_at` 晚于 `recorded_at`：`400 INVALID_OCCURRED_AT`。超出锁账范围：`409 BOOKS_LOCKED`（见 Q6）。
- 盘点不能补录。
- 界面交互：所选那天该物料有盘点时，提示「X 点盘过该物料，这次发生在盘点前还是盘点后？」，按选择把 `occurred_at` 设在那次盘点时间的对应一侧。

**通用规则**：

- 一个命令产生的所有事件共用同一组 `recorded_at`、`occurred_at`、`business_date`。
- 冲销和数量更正的 `occurred_at`、`business_date` 取原事件的值，由写入线程填写，客户端不能指定。
- 首次提交时算出的 `occurred_at` 随事件保存；重试时返回原响应，不重新计算。
- **营业日**：配置项 `timezone = "Asia/Shanghai"`、`business_day_cutoff = "04:00"`。`occurred_at` 换算到门店当地时间，早于日切的算前一营业日。由 `jiff` 纯函数计算。修改日切配置不重算历史事件。
- **时钟异常**：`recorded_at` 不保证递增，这是预期行为。`/health` 暴露 `clock_regression_ms = max(0, max(recorded_at) − now)`，超过 5 分钟时状态为 `degraded`，**不拒绝写入**。

## 主数据

| 实体（聚合类型） | 内容 |
|---|---|
| `ITEM` | 物料：`code`、名称、基本单位（`g` / `ml` / `pcs`）、类别（`RAW` / `SEMI` / `FINISHED`）、`default_shelf_life_ms?`；**含单位换算** |
| `RECIPE` | 配方：`code`、产出物料；**含全部版本**，每个版本有每批产出和每批用料。版本只增不改 |
| `SUPPLIER` | 供应商 |
| `EMPLOYEE` | 员工：姓名、角色、启用状态。**不含凭据** |
| `WASTE_REASON` | 报损原因：`code`、名称。初始化时预置 `EXPIRED`、`DAMAGED`、`PRODUCTION_DEFECT`、`TASTING`、`OTHER` |
| `EQUIPMENT` | 设备：名称、类型（冷柜 / 烤箱…） |

- 主键一律 UUIDv7。`code` 列唯一、人可读、各门店统一（物料的 `code` 用于销售导入）。
- 每行带 `revision`，每次变更 +1。主数据只停用（`active = 0`），不删除。
- 每次变更写一条 `MASTER_DATA_CHANGED`：聚合类型是实体名，聚合 ID 是行的主键，`aggregate_version` 是变更后的 `revision`；payload 是 `{entity, source, snapshot}`，`source`（`LOCAL` / `HQ_PACKAGE`）。主数据表是投影，可以从事件流重建。
- 内容没有变化的变更不写事件（导入主数据包时逐行比对快照）。
- **认证状态不进事件流**：`employee_credentials`（PIN 的 Argon2id 哈希）、设备注册、会话都是普通状态表，不同步，不参与重建。
- **系统操作人**：初始化和主数据包导入在没有登录员工时，`actor_id` 使用保留 ID `00000000-0000-7000-8000-000000000000`。
- **初始化**：`boh-server init` 写入 `store_meta`，再创建第一个店长账号。启动时配置里的 `store_id` 和 `store_meta` 不一致，拒绝启动。

### 员工认证

- **设备注册**：店长在新平板上登录一次，服务端签发设备令牌，作为 `device_id` 的来源。
- **员工登录**：PIN 由服务端用 Argon2id 校验，签发会话令牌（服务端状态表）。普通员工 4～6 位，店长（能注册设备）6 位。会话按班次有效（结束判定见 Q7）。
- **在线限速**（PIN 能安全使用的前提）：
  - 失败次数**按员工**计数，跨所有设备，登录和设备注册共用同一计数；成功一次清零。
  - 连续失败 5 次后锁定该员工，锁定时长从 5 分钟起每次翻倍，上限 1 小时；锁定期间不影响该员工已有的会话。
  - 整个节点每小时失败超过 50 次时，所有 PIN 校验额外延迟 5 秒，并在 `/health` 中报告 `auth_failures_last_hour`。
- 客户端不缓存 PIN 哈希。
- **传输加密**：设备令牌、PIN、会话令牌只经 HTTPS 传输。局域网 HTTPS（证书方案见 Q9）是认证切片的前置条件；节点未配置 TLS 时不注册认证接口，release 构建启用认证但未配置 TLS 时**拒绝启动**。
- 认证落地之前，业务切片只依赖 `Actor` 提取器。开发桩只在 debug 构建中可用；release 构建配置了开发桩时**拒绝启动**。

## 单位

- 每个物料只有一个基本单位。账本、投影中的数量都是基本单位的 `i64`。换算系数必须是**正整数**基本单位（`> 0`）。
  - 主数据写入时校验（本地编辑和总部包导入都一样），系数不合格的变更整条拒绝，不进入事件流。
- 命令用 `input {qty, unit_code, base_qty_per_unit}` 提交，其中系数是客户端录入时看到的值。写入线程用当前换算核对：
  - `unit_code` 未配置：`400 UNKNOWN_UNIT`；
  - 系数和当前值不一致：`409 UNIT_CONVERSION_CHANGED`，不入账，由人工确认后重新提交。
- payload 同时写入换算后的 `qty` 和 `input` 快照。

## 批次

全部物料都按批次追踪。账面数 = 批次余量之和 + 账外缺口。

- **批次 ID**：每条收货行、每次生产产出、每个盘盈由写入线程生成一个 `lot_id`（UUIDv7），写进 payload。供应商批号 `supplier_lot_no` 是可选属性，不作主键。
- **到期时间** `expires_at`（UTC 毫秒，可空）只用于过期提醒，不参与分配。只有日期的保质期由客户端换算成门店当地当日结束时刻再提交；未填写时可用 `default_shelf_life_ms` 推算，推算结果写进 payload。
- **FIFO 分配**：未指定 `lot_id` 的扣减，先扣 `origin = 'COUNT_GAIN'` 的批次，其余按来源事件的 `seq` 升序。补录的收货按入账顺序排队。
- 客户端可以指定 `lot_id`；指定批次余量不足时，不足部分继续按 FIFO 分配。
- **账面不足**：全部批次分配完仍不够时不拒绝，剩余部分记入账外缺口，返回警告 `STOCK_SHORTFALL`。下一次包含该物料的盘点会把账外缺口清零。
- 分配结果连同来源写进 payload：`alloc[{lot_id?, qty, source}]`。`qty` 为正数，不带 `lot_id` 表示账外缺口；`source`（`SPECIFIED` / `FIFO` / `SHORTFALL`，纠错另有 `CORRECTION` / `REVERSAL`，见「纠错」）。重放时直接使用，不重新分配。
- 新批次的 `expires_at` 早于同物料现存最老的批次时，返回警告 `EXPIRES_BEFORE_OLDER_STOCK`。
- **承诺边界**：批次是账面推定，不是实物证据。追溯报告分开显示「员工指定」「系统推定」「账外 / 吸收」三类。

## 事件目录

所有数量是基本单位 `i64`，金额是 `i64` 分。`schema_version` 从 1 开始。
`actor_id`、`device_id`、`occurred_at`、`business_date` 存在 `store_events` 的列中，不重复写进 payload。
`input` 见「单位」，`alloc` 见「批次」。影响库存的行（盘点行除外），要么带 `alloc`（或新建的 `lot_id`），要么带 `absorbed_by_event_id`（被吸收，见「盘点吸收」），二者取一。
带 `?` 的字段可选：缺省时省略该键，payload 中不出现 `null`。字段之间的 `a | b` 表示二者恰有一个出现；枚举值写作（`A` / `B`）。

| event_type | aggregate_type | payload 要点 | 投影影响 |
|---|---|---|---|
| `GOODS_RECEIVED` | `RECEIPT` | `supplier_id`, `lines[{item_id, qty, input, lot_id \| absorbed_by_event_id, supplier_lot_no?, expires_at?, line_cost_cents}]` | 每行新建一个批次；被吸收时不建 |
| `PRODUCTION_BATCH_COMPLETED` | `PRODUCTION_BATCH` | `recipe_id`, `recipe_version`, `batch_count`, `started_at?`, `output{item_id, planned_qty, qty, lot_id \| absorbed_by_event_id, expires_at?}`, `consumed[{item_id, planned_qty, qty, alloc \| absorbed_by_event_id}]` | 原料按分配扣减（或被吸收）；成品新建批次 |
| `WASTE_LOGGED` | `WASTE_RECORD` | `lines[{item_id, qty, input, reason_code, alloc \| absorbed_by_event_id}]` | 按分配扣减（或被吸收） |
| `STOCK_COUNT_SUBMITTED` | `STOCK_COUNT` | `purpose`（`CLOSING` / `AUDIT`）, `started_seq`, `lines[{item_id, lot_id?, counted_qty}]` | 无 |
| `STOCK_ADJUSTED` | `STOCK_COUNT` | `lines[{item_id, lot_id?, book_qty, counted_qty, delta}]`, `new_lots[{lot_id, item_id, qty}]` | 批次和账外缺口按 delta 变化；新建盘盈批次；写 `inventory_counts` |
| `PURCHASE_ORDER_SUBMITTED` | `PURCHASE_ORDER` | `supplier_id`, `lines[{item_id, qty, input}]`, `deliver_on` | 无 |
| `TEMPERATURE_LOGGED` | `TEMPERATURE_READING` | `equipment_id`, `celsius_x10`, `note?` | 食安记录 |
| `QUANTITY_CORRECTED` | 与原事件相同 | `corrected_event_id`, `reason`, `lines[{line_ref, item_id, physical_at, old_qty, new_qty, delta, input?, line_cost_cents?, alloc \| absorbed_by_event_id}]` | 按差额调整（或被吸收），见「纠错」 |
| `EVENT_REVERSED` | 与原事件相同 | `reversed_event_id`, `reason`, `lines[{line_ref, item_id, physical_at, qty, alloc \| absorbed_by_event_id}]` | 精确取反（或被吸收），见「纠错」 |
| `SALES_IMPORTED` | `SALES_DAY` | `source`（`CSV` / `XLSX`）, `file_name`, `lines[{item_id, qty, amount_cents}]`, `ignored_rows`（整数） | 覆盖 `daily_sales` 中该营业日；**不写库存流水** |
| `MASTER_DATA_CHANGED` | 实体名 | `entity`, `source`, `snapshot` | 覆盖对应的主数据表 |

- `reason_code` 的值对应 `WASTE_REASON` 的 `code`。
- 温度记录每条读数一个新的 `TEMPERATURE_READING` 聚合。
- 一次盘点的 `STOCK_COUNT_SUBMITTED`、`STOCK_ADJUSTED` 属于同一个 `STOCK_COUNT` 聚合，version 分别为 1、2。
- **可纠错事件的聚合版本**：原事件是 version 1，每次数量更正 +1，冲销是该聚合的最后一个版本。

### 生产规则

- 计划用量 = 配方版本中每批用量 × `batch_count`；计划产出 = 每批产出 × `batch_count`。只有整数乘法，没有除法和舍入。
- 命令携带 `recipe_version`，按该版本计算。版本存在即可，不要求是当前版本。
- 员工可以修改每行实际 `qty`、增加配方外的物料（`planned_qty = 0`）、把某行改为 0，也可以修改实际产出（烤坏的直接少计产出，不另记报损）。
- 半成品（面团、馅料）也是物料，可以作为另一个配方的原料。
- 生产界面在员工点「开始生产」时记录 `started_captured_at`（只存在客户端，提交时一起带上）。用料的实物时点取开始时间，见「盘点吸收」。

## 盘点

1. **开始盘点**：客户端调用只读接口 `GET /api/v1/stock-counts/snapshot`，拿到 `started_seq = max(seq)` 和当时每个物料的批次明细（`lot_id`、余量、到期时间、供应商批号）及账外缺口。开始盘点**不产生事件**。
2. **提交盘点**：命令携带 `started_seq` 和逐批次的清点结果，同一事务内写入 `STOCK_COUNT_SUBMITTED` 和 `STOCK_ADJUSTED`。
3. **盘点范围**：只针对 `lines` 里出现的物料。对其中每个物料，范围是 `source_seq <= started_seq` 的批次加上该物料的账外缺口。盘点期间新建的批次不在范围内。
   - `lines` 中的 `lot_id` 不属于该物料或不在范围内：`400 LOT_NOT_IN_COUNT_SCOPE`；同一批次出现两次：`400 DUPLICATE_COUNT_LINE`。
4. **逐批次调整**：
   - 范围内每个批次：`delta = counted_qty − remaining_qty`。范围内没有列出的批次按 `counted_qty = 0` 处理。
   - 账外缺口清零。
   - 不带 `lot_id` 且 `counted_qty > 0`（找到了对不上批次的货）：新建一个 `COUNT_GAIN` 批次，写进 `new_lots`。每个物料最多一行不带 `lot_id`。
   - `book_qty`、`delta` 和新批次都写进 payload，重放时不重新计算。`STOCK_ADJUSTED.lines` 包含范围内余量非 0 或被清点的每个批次，另外每个物料恰有一行不带 `lot_id`：`book_qty` 是账外缺口，`counted_qty` 是无批次的清点数（没有时为 0）。投影把这一行拆成账外缺口的 `−book_qty`（归零）和 `new_lots` 中的新批次。
5. **盘点期间有变动**：被盘物料在 `seq > started_seq` 之后有 `qty_delta ≠ 0` 的流水时，盘点照常入账，同时返回警告 `MOVED_DURING_COUNT`（列出这些物料），由店长决定是否重盘。不做差分自动合并（清点持续好几分钟，实物在过程中已经反映了一部分消耗，自动合并必然重复扣减）。
6. 被盘的每个物料写一行 `inventory_counts`（零差异也写）：`book_qty` 是范围内批次余量与账外缺口之和，`counted_qty` 是清点合计。
7. **操作规范**（写进门店 SOP，系统不强制）：盘点只数货架上的实物，不含在制品；清点期间暂停领用和收货。

## 盘点吸收

**原则**：一次盘点把账面校准到观察时点的实物数量。某个库存影响如果在这次盘点**入账之后**才入账，但实物影响发生在观察时点**之前**，盘点已经把它算进去了，它不应再改动账面。

**定义**：

- 库存影响行 e：某个事件对物料 I 的一行影响，名义变动量 `nominal_qty`。
- 实物时点 t(e)：

| 影响来源 | t(e) |
|---|---|
| 收货、报损、生产产出 | 事件的 `occurred_at` |
| 生产用料 | 生产的 `started_at`；没有时取 `occurred_at` |
| 冲销行、更正行 | 原行的 t |

- 盘点 c 对物料 I 的观察时点 o(c)：盘点事件的 `occurred_at`。

**规则**：写入 e 时，在已入账、覆盖了物料 I、且 o(c) > t(e) 的盘点中，取 o(c) 最早的一次：

```sql
SELECT e.id FROM inventory_counts c JOIN store_events e ON e.seq = c.event_seq
WHERE c.item_id = ?1 AND c.observed_at > ?2
ORDER BY c.observed_at, c.event_seq LIMIT 1
```

查到时 e 被它吸收：

- `qty_delta = 0`，`absorbed_by_event_id = c 的 STOCK_ADJUSTED 事件 id`，`lot_id = NULL`，`alloc_source = 'ABSORBED'`；
- 不分配批次，不新建批次，不改动账外缺口；
- 事件照常入账，payload 记录 `absorbed_by_event_id`，重放时直接读取，不重新判定；
- 返回警告 `ABSORBED_BY_COUNT`，列出受影响的物料和对应的盘点。

已入账盘点的 `seq` 一定小于正在写入的 e，所以只需比较时间。原行被吸收时，它的冲销 / 更正用同一个 t，也会被同一次盘点吸收。
同一事件中可以有的行被吸收、有的行正常生效（例如原料盘过、成品没盘过）。

**报表含义**：被吸收的行照常计入自己的类别（报损、收货…），同时冲减吸收它的那次盘点的差异。盘点差异里原本就含着这笔数量，扣掉后剩下的才是真实差异（公式见「销售导入」）。

**边界**：

- t(e) 落在盘点进行期间时，归属判断不了，一律按已吸收处理并返回警告；靠「盘点」第 7 条的操作规范降到最少。
- 被吸收的收货不建批次。供应商、批号、金额只留在事件里，对账和应付不受影响。

## 纠错

不提供编辑接口。所有纠错都是追加事件：

| 情况 | 用法 |
|---|---|
| 物料、供应商、配方都对，只是**数量**错了 | `QUANTITY_CORRECTED`：按差额调整，原批次不变 |
| 物料 / 供应商 / 配方录错、重复提交、整条不该存在 | `EVENT_REVERSED` 整条冲销，需要的话重新录入 |

**适用范围**：

- `GOODS_RECEIVED`、`PRODUCTION_BATCH_COMPLETED`、`WASTE_LOGGED`：可以更正，也可以冲销。
- `PURCHASE_ORDER_SUBMITTED`、`TEMPERATURE_LOGGED`：只能冲销（`lines` 为空）。
- 盘点类事件、`EVENT_REVERSED`、`QUANTITY_CORRECTED`、`MASTER_DATA_CHANGED`、`SALES_IMPORTED` 不能被纠错（盘点录错就重盘；主数据再改一次；销售重新导入），提交时返回 `400 EVENT_NOT_CORRECTABLE`。
- 原事件不存在：`404 EVENT_NOT_FOUND`。

**通用规则**：

- 写入前读取原事件所在聚合的全部事件。已有 `EVENT_REVERSED`：`409 ALREADY_REVERSED`。同一事件可以更正多次。
- 原事件某一行的**有效数量** = 原值 + 之后全部更正的差额；有效分配同理。
- 每行按「盘点吸收」判定（t 取原行的值）。被吸收的行不动库存，只更正报表。
- 每行的 `physical_at` 是原行的 t，由写入线程填写。生产用料的 t 是 `started_at`，与事件的 `occurred_at` 不同，所以必须写进 payload，重放时不查原事件。
- 纠错行沿用原行的 `kind`（由聚合类型和 `line_ref` 确定，不查原事件），营业日取原行的值。所以按 `kind` 汇总时纠错自动抵消原记录，并回写到原来那天的日报。
- 纠错行的 `alloc`：方向上，更正 `delta > 0` 与原行相同、`delta < 0` 与原行相反，冲销与原行相反；来源上，对原批次 / 原账外缺口的增减和退回记 `CORRECTION`（冲销记 `REVERSAL`），追加分配记 `FIFO`，扣回不足的部分记 `SHORTFALL`。
- **扣回时批次余量不够**（原批次已被后续扣减）：批次扣到 0，不足部分记入账外缺口，返回 `STOCK_SHORTFALL`，界面提示「建议现在盘点该物料」。下一次包含该物料的盘点会清零，不会在盘点差异里重复出现。
- **退回会使账外缺口大于 0**（原分配含账外缺口，而该缺口已被之后一次观察时点更早的盘点清零，只在 `MOVED_DURING_COUNT` 之后出现）：`409 COUNT_REQUIRED`。先盘点该物料，新盘点的观察时点晚于原行，纠错随后会被吸收。

**数量更正 `QUANTITY_CORRECTED`**：

- `line_ref` 定位原事件中的一行：`lines[i]`（收货、报损）、`consumed[i]`、`output`（生产），`i` 为原数组下标。`item_id` 必须与该行一致，否则 `400 LINE_MISMATCH`。
- `old_qty` 是该行当前有效数量，`delta = new_qty − old_qty`，`delta ≠ 0`，`new_qty >= 0`。原行带 `input` 时（收货、报损），新数量照常带 `input`；生产的行不带。
- 收货行可同时更正 `line_cost_cents`。生产只更正数量，不更正 `batch_count` 和配方（录错配方要冲销）。
- 未被吸收的行：

  | 行类型 | `delta > 0` | `delta < 0` |
  |---|---|---|
  | 新建批次的行（收货行、生产 `output`） | 原批次 + delta | 从原批次扣回，不足部分进账外缺口 |
  | 扣减库存的行（生产 `consumed`、报损行） | 按 FIFO 追加分配 | 按有效分配的**逆序**退回（最后分配的先退，账外缺口部分退回账外缺口） |

**整条冲销 `EVENT_REVERSED`**：

- 逐行处理原事件中影响库存的每一行，数量取有效数量。
- 未被吸收的行精确取反：扣减类的行按有效分配加回原批次 / 账外缺口；新建批次的行从原批次扣回（不足部分进账外缺口）。

## 销售导入

开发顺序排在库存切片之后。

**规则：销售只进报表，不扣库存。** 成品库存只由收货、生产产出、报损、生产用料和盘点决定。
导入与闭店盘点的先后无法保证；销售如果扣库存，先盘点再导入时同一批销售会被扣两次。原料在生产完成时已经扣减。

- 导入文件由我方定义字段，用户按此配置 POS 导出。**每个营业日、每个商品一行的汇总**：

  | 列 | 类型 | 说明 |
  |---|---|---|
  | `business_date` | `YYYY-MM-DD` | 整个文件必须相同 |
  | `item_code` | 文本 | 物料的 `code` |
  | `qty` | 整数 | 售出数量，基本单位；退货为负 |
  | `amount_cents` | 整数 | 实收金额，单位分 |

- Excel / CSV 在**平板浏览器中解析**，转成统一的 JSON 命令提交。Rust 端只处理 JSON，不为 Excel 引入依赖。命令走普通写入管道，幂等规则不变。
- 营业日取自文件内容，由命令显式给出，不按 `occurred_at` 计算。一个文件只能包含一个营业日。
- 出现未知 `item_code` 时**整份拒绝**（`400 UNKNOWN_ITEM_CODE`），列出未知编码。前厅现做、不经后厨的商品由「忽略编码清单」过滤，计入 `ignored_rows`。
- **同一营业日可以重复导入**：每个营业日一个 `SALES_DAY` 聚合，由投影 `sales_days` 映射。第一次导入时生成 UUIDv7，之后每次 version +1；`daily_sales` 以最新版本为准。不需要冲销。
- **遗漏提醒**：已导入销售但当天没有闭店盘点，或已闭店盘点但当天没有导入销售，界面提示。

**未解释损耗**（按物料分窗口计算：窗口的边界是每个营业日该物料最后一次 `CLOSING` 盘点，同一营业日的重盘在同一窗口内；被吸收的行计入吸收它的那次盘点所在窗口）：

```
未解释损耗 = −Σ(窗口内 ADJUST 的 qty_delta) + Σ(窗口内被吸收行的 nominal_qty) − Σ(窗口内销售 qty)
```

例：生产 100、报损 8、闭店实数 5（调整 −87）、销售 84 → 未解释损耗 3。

- 调整量按净和计算：先盘为 80、再重盘为 90，两次调整 −20、+10，净和 −10。
- 窗口跨越多个营业日时，报表标注跨越的日期。
- 结果为负（销售比盘亏还多）时作为异常显示，通常是漏录生产或盘点数错。

## 投影表

表结构随库存切片进入 002，下面的 SQL 是设计意图，**不冻结**：

```sql
CREATE TABLE inventory_lots (
    lot_id          TEXT PRIMARY KEY,
    item_id         TEXT NOT NULL,
    origin          TEXT NOT NULL CHECK (origin IN ('RECEIPT', 'PRODUCTION', 'COUNT_GAIN')),
    source_seq      INTEGER NOT NULL REFERENCES store_events(seq),  -- FIFO 排序键
    remaining_qty   INTEGER NOT NULL CHECK (remaining_qty >= 0),
    expires_at      INTEGER,
    supplier_lot_no TEXT
) STRICT;

CREATE TABLE inventory_unallocated (                -- 账外缺口
    item_id TEXT PRIMARY KEY,
    qty     INTEGER NOT NULL CHECK (qty <= 0)
) STRICT;

CREATE TABLE inventory_movements (                  -- 每条库存影响按批次展开，一行一条
    event_seq       INTEGER NOT NULL REFERENCES store_events(seq),
    line_no         INTEGER NOT NULL,
    item_id         TEXT NOT NULL,
    lot_id          TEXT,                            -- NULL：账外缺口，或被吸收
    kind            TEXT NOT NULL CHECK (kind IN ('RECEIPT', 'PRODUCE', 'CONSUME', 'WASTE', 'ADJUST')),
    alloc_source    TEXT NOT NULL CHECK (alloc_source IN
                    ('NEW_LOT', 'SPECIFIED', 'FIFO', 'SHORTFALL', 'COUNT', 'CORRECTION', 'REVERSAL', 'ABSORBED')),
    nominal_qty     INTEGER NOT NULL,                -- 按申报内容应有的变动量
    qty_delta       INTEGER NOT NULL,                -- 实际作用于账面的变动量
    absorbed_by_event_id TEXT REFERENCES store_events(id), -- 直接取自 payload，重放不查其他事件
    physical_at     INTEGER NOT NULL,                -- 实物时点 t(e)
    business_date   TEXT NOT NULL,
    PRIMARY KEY (event_seq, line_no),
    CHECK ((absorbed_by_event_id IS NULL AND qty_delta = nominal_qty)
        OR (absorbed_by_event_id IS NOT NULL AND qty_delta = 0 AND lot_id IS NULL))
) STRICT;

CREATE TABLE inventory_counts (                     -- 每次盘点的每个物料一行，零差异也写
    event_seq   INTEGER NOT NULL REFERENCES store_events(seq),
    item_id     TEXT NOT NULL,
    observed_at INTEGER NOT NULL,                    -- 盘点事件的 occurred_at
    book_qty    INTEGER NOT NULL,
    counted_qty INTEGER NOT NULL,
    PRIMARY KEY (event_seq, item_id)
) STRICT;
CREATE INDEX idx_inventory_counts_item ON inventory_counts(item_id, observed_at);
```

- `alloc_source`：新建批次（含盘盈批次）`NEW_LOT`；盘点对已有批次和账外缺口的调整 `COUNT`；被吸收 `ABSORBED`；其余直接取 payload 中 `alloc` 的 `source`。
- **不变量**（测试和定时自检都检查）：
  - 批次余量 `remaining_qty` = 该批次所有流水的 `qty_delta` 之和；
  - 账外缺口 = 该物料 `lot_id IS NULL` 的流水 `qty_delta` 之和，且 `<= 0`；
  - 账面数（视图 `inventory_on_hand`）= 批次余量之和 + 账外缺口。
- **日报**：视图，按 `business_date, item_id, kind` 聚合流水。收货、产出、用料、报损按 `nominal_qty`（申报值，含被吸收的行），盘点调整按 `qty_delta`。不维护增量日结表。
- `sales_days(business_date PRIMARY KEY, aggregate_id, version, source_event_id)`；`daily_sales(business_date, item_id, qty, amount_cents, source_event_id)`。
- 主数据表（`items`、`recipes`、`suppliers`、`employees`、`waste_reasons`、`equipment`）也是投影。
- 所有投影都必须能通过 `boh-server rebuild-projections` 从 `store_events` **按 `seq`** 完整重建，结果与在线写入逐行一致。

## 已知限制

- **批次追溯是账面推定**。员工实际拿的批次和系统推定的不一致时，批次余量会偏离实物，直到下一次盘点。
- **被吸收的收货不建批次**。那批货在批次层面的来源由盘点产生的 `COUNT_GAIN` 批次代替。被吸收的冲销也不会把错误收货建出的批次扣掉，这些批次按 FIFO 自然消耗。
- **盘点期间的实物变动**只检测并警告，靠操作规范避免，系统不自动修正。
- **时钟**：系统时钟跳到未来再拨回时，期间事件的 `business_date` 是错的，不能自动修正；`/health` 会暴露这种情况。
- **门店节点长时间宕机**时没有离线写入能力，改用纸质登记，恢复后走补录入口。
- **客户端暂存**：只承诺页面不刷新期间，命令进入 `IndexedDB` 队列，界面显示「待提交 N 条」。客户端不维护库存镜像，不做批次分配。排队命令补交被拒时，界面列出命令和原因，由人工处理。

## 验收用例

写成锁定测试之前，预期数值由人工逐条确认（见 AGENTS.md「测试分工」）。初始状态：账面 = 实物。

| 用例 | 过程 | 预期 |
|---|---|---|
| 生产期间盘点 | 面粉 100；09:10 开始生产，取 20；09:20 盘点得 80（调整 −20）；09:30 完工，用料 20，`started_at = 09:10` | 用料被吸收；账面 **80**；返回 `ABSORBED_BY_COUNT` |
| 生产没带开始时间 | 同上，没有 `started_at` | 不被吸收，账面 **60** |
| 迟到报损 | 100；09:00 报损 10 留在平板队列；09:20 盘点得 90（调整 −10）；09:40 报损送达 | 被吸收；账面 **90**；未解释损耗 **0** |
| 补录报损 | 100；15:00 盘点得 50（调整 −50）；16:00 补录报损 50，`occurred_at = 10:00` | 被吸收；账面 **50**；报损 **50**；未解释损耗 **0** |
| 盘点后冲销 | 100；09:00 误报损 10，账面 90；09:20 盘点得 100（调整 +10）；10:00 冲销报损 | 被吸收；账面 **100**；报损合计 **0**；未解释损耗 **0** |
| 闭店重盘 | 成品账面 100；先盘为 80，再盘为 90；销售 0 | 调整净和 **−10**；未解释损耗 **10** |
| 指定批次不足 | 批次 A 5、B 10；指定 A 扣 8 | 分配 `A 5 SPECIFIED` + `B 3 FIFO` |
| 按批次盘点 | 批次 A 5、B 10，账外缺口 −2；清点 A 4、B 10、无批次 3 | A 调整 −1；账外缺口 +2 归零；新建 `COUNT_GAIN` 批次 3；账面 **17** |
| 换算变化 | 离线录入「2 袋，每袋 25000 g」；提交前换算改为 20000 | `409 UNIT_CONVERSION_CHANGED`，不入账 |
| 销售示例 | 生产 100、报损 8、闭店实数 5（调整 −87）、销售 84 | 未解释损耗 **3** |
| 幂等 | 同一命令发送 2 次，`sent_at` 不同 | 第二次原样返回首次响应（含 warnings），`seq` 不增加 |
| 盘点期间变动 | `started_seq = 10`；seq 11 领用了该物料；seq 12 提交盘点 | 照常入账，返回 `MOVED_DURING_COUNT` |
| 盘点后的正常业务 | 09:20 盘点；10:30 报损并提交 | 不被吸收，正常扣减 |
| 冲销时批次已消耗 | 收货批次 L 10，已用掉 3，之后没有盘点；冲销这次收货 | L 扣到 **0**；账外缺口 **−3**；返回 `STOCK_SHORTFALL` |
| 数量更正 | 收货录成 1000（实际 100），批次 L1；生产从 L1 扣 300；之后没有盘点；更正为 100 | L1 **0**；账外缺口 **−200**；返回 `STOCK_SHORTFALL` |
| 更正多次后冲销 | 报损 10 → 更正为 8 → 更正为 6 → 冲销 | 冲销退回 **6**；报损合计 **0** |
| 重复冲销 | 同一事件冲销两次 | 第二次 `409 ALREADY_REVERSED` |
| 冲销后更正 | 冲销后再更正同一事件 | `409 ALREADY_REVERSED` |

## 未来功能（本期不实现）

- **门店调拨**：`TRANSFER_SENT` / `TRANSFER_RECEIVED`，含在途归属规则。
- **总部同步**：上行契约见 AGENTS.md「上行同步」；下行为主数据包导入（`source = HQ_PACKAGE`）。总部从事件自行计算报表，必须区分被吸收的行和正常生效的行。总部关账后可下发锁账日（见 Q6）。
- **采购对接**：采购单与收货的关联、在途库存。

## 待确认问题

- **Q6 锁账日**：默认方案是店长在管理界面设置锁账日，`business_date` 不晚于它的补录和纠错返回 `409 BOOKS_LOCKED`；另外默认最多补录 7 天以内。**阻塞补录入口**。
- **Q7 会话的「班次」如何结束**：默认方案是员工主动登出，或登录满 12 小时。**阻塞认证切片**。
- **Q8 主数据管理接口权限**：默认方案是只允许店长角色。**阻塞主数据管理接口**。
- **Q9 局域网证书**：默认方案是总部私有 CA 为每个门店节点签发证书，平板初始化时安装一次根证书；备选是公网域名 + ACME DNS 验证（平板无需配置，但续期依赖联网）。**阻塞认证切片**。
