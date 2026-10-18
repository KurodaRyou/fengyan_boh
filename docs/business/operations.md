# 业务操作与验收

按任务读取对应操作章节及验收表中的相关行，无需通读其他操作。写命令共同规则见 [engineering/http.md](../engineering/http.md)，时间与校验见 [business/time.md](time.md)，库存操作还须读 [business/inventory.md](inventory.md)。状态以各节和 [路线图](../../AGENTS.md#路线图) 为准。

## 温度记录接口

| 方法与路径 | `command_type` | 请求体 / 查询参数（`boh_domain::temperature`） | 权限 |
|---|---|---|---|
| `POST /api/v1/temperature-readings` | `temperature.log` | `LogTemperature` | 已认证员工 |
| `GET /api/v1/temperature-readings` | — | `TemperatureQuery` | 已认证员工 |

- 写命令成功的 `data` 是 `{"temperature_reading": 行}`，查询的 `data` 是 `{"temperature_readings": [行…]}`；行为 `boh_domain::temperature::TemperatureReading`。
- **取值**：
  - `celsius_x10` 是 0.1 °C 的整数，`-500`～`5000`（-50.0～500.0 °C），两端可取。它只拦截单位或数量级录错，不是食安阈值，不产生警告。
  - `note` 可以省略；出现时非空、首尾不能有空白字符、最多 200 个字符（按 Unicode 字符计），不接受 `null`。首尾以外的空白和换行原样保存。
- **新建**：读数 ID 是聚合 ID，写 `TEMPERATURE_LOGGED`。
  - 设备不存在：`404 REFERENCE_NOT_FOUND`，`entity` 为 `EQUIPMENT`。停用的设备照常受理；不限制 `equipment_type`。
- **查询**：`business_date`（`'YYYY-MM-DD'`，真实日期）必填，`equipment_id` 可选；不分页。按 `occurred_at` 升序，相同时按 `seq` 升序；含停用设备的读数。
  - 参数缺失、取值非法、未知或重复的参数：`400 VALIDATION_FAILED`。`equipment_id` 合法但不存在时返回空数组。
- 投影 `temperature_readings` 每条读数一行，字段以迁移 003 为准。冲销在投影和查询中的体现随纠错切片设计。

## 收货接口

| 方法与路径 | `command_type` | 权限 |
|---|---|---|
| `POST /api/v1/receipts` | `receipt.create` | 已认证员工 |

- 请求体与行见 `boh_domain::receiving` 的 `CreateReceipt`、`Receipt`；`input` 见[「单位」](inventory.md#单位)。
- 写命令成功的 `data` 是 `{"receipt": 行}`；行的 `lines` 与 payload 的 `lines` 相同。
- **取值**：
  - `lines` 非空。同一物料可以有多行，各行分别建批次。
  - `input` 见[「单位」](inventory.md#单位)。
  - `line_cost_cents` 是该行总金额（分），JSON 整数，`0`～`99999999`（两端可取，即最多 999999.99 元）。
    理由：拦截多打位数的录入错误；以后累加成本时不会超出平板 JS 能精确表示的整数范围。
  - `manufacturer_lot_no` 可以省略；出现时非空、首尾不能有空白字符、最多 64 个字符（按 Unicode 字符计），不接受 `null`。
  - `produced_on`（生产日期）、`expires_on`（到期日）必填，是包装标签上的门店当地日期，格式 `'YYYY-MM-DD'`（真实日期），`produced_on <= expires_on`，`expires_on` 不晚于 `9998-12-31`（`expires_at` 由次日当地 0 点换算，须在时间戳范围内）。标签只印保质期时长时，由客户端按日历推算 `expires_on`。
- **新建**：收货 ID 是聚合 ID，每行按[「批次」](inventory.md#批次)生成一个批次号 `lot_id`，写 `GOODS_RECEIVED@2`。
  - payload 每行：`qty` 是 `input` 的单位换算结果（见[「单位」](inventory.md#单位)）；`expires_at` 是 `expires_on` 在门店时区（配置 `timezone`）的当日最后一毫秒，即次日当地 0 点的 UTC 毫秒减 1。
    - 理由：`projections::apply` 不读配置，换算结果必须写进 payload。
  - 批次日期是 `occurred_at` 的门店当地日历日期，与下面 `INVALID_LOT_DATES` 的 D 相同。
- **业务校验**：
  - 供应商、物料不存在：`404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": "SUPPLIER" / "ITEM", "id"}`。
  - `UNKNOWN_UNIT`、`UNIT_CONVERSION_CHANGED`：见[「单位」](inventory.md#单位)。
  - 日期与收货时间矛盾：`400 INVALID_LOT_DATES`，`details` 为 `{"line", "item_id", "reason"}`。以 `occurred_at` 在门店时区的日历日期 D 判定（不是营业日）：`produced_on > D` 时 `reason = "PRODUCED_IN_FUTURE"`；`expires_on < D` 时 `reason = "EXPIRED_ON_RECEIPT"`。等于 D 都允许。
    - 用日历日期而不是营业日：日切前（如凌晨 2 点）收到当天生产的货是正常的。
  - 流水号用尽：`409 LOT_SERIAL_EXHAUSTED`（见[「批次」](inventory.md#批次)）。
  - 库存状态上限：`409 STOCK_LIMIT_EXCEEDED`（见[「数量」](inventory.md#数量)）。同一物料的多行累计检查：每行看到前面各行新建的批次。
- **警告**：按 `line` 升序的 `EXPIRES_BEFORE_OLDER_STOCK`（见[「批次」](inventory.md#批次)）。
- **吸收**：收货行新建批次，不被盘点吸收（见「盘点吸收」）。
- **投影**：每行在 `inventory_lots` 建一个 `origin = 'RECEIPT'` 的批次（`lot_id` 及由它拆出的批次日期和流水号，`source_line_no` 是行下标，`remaining_qty = qty`），在 `inventory_movements` 写一条 `kind = 'RECEIPT'`、`alloc_source = 'NEW_LOT'` 的流水，`physical_at = occurred_at`。`line_cost_cents`、`produced_on` 只在事件中。
- **`GOODS_RECEIVED@1` 不支持**：`@1` 的 `lot_id` 是 UUID，payload 中没有生成批次号所需的类型和编码，不能 upcast，也不推测。
  - 账本中有 `@1` 收货时，迁移 007 报错并拒绝执行（节点不能启动）；重建投影遇到 `@1` 收货也报错。
  - 门店节点从空库 `init` 开始，不会有 `@1` 收货；只影响开发库，开发库需要重新初始化。

## 报损接口

| 方法与路径 | `command_type` | 权限 |
|---|---|---|
| `POST /api/v1/waste-records/precheck` | —（只读，不是写命令） | 已认证员工 |
| `POST /api/v1/waste-records` | `waste.log` | 已认证员工 |

报损先预检；账面不足的行由员工确认实际报损数量后，再正式提交。系统不自动确认，也不自动重提。预检连不上节点时按[「命令提交与结果未知」](command-submission.md#命令提交与结果未知)的未发出处理。

**正式提交**：

- 请求体与行见 `boh_domain::waste` 的 `LogWaste`、`WasteRecord`；`input` 见[「单位」](inventory.md#单位)。`lot_id` 是员工指定的批次，`reason_code` 是 `WASTE_REASON` 的 `code`，`acknowledged` 见下方「不足确认」。
- 写命令成功的 `data` 是 `{"waste_record": 行}`；行的 `lines` 与 payload 的 `lines` 相同。
- **取值**：
  - `lines` 非空。同一物料、同一批次都可以出现在多行（例如不同原因）。
  - `input` 见[「单位」](inventory.md#单位)。
  - `reason_code` 非空、首尾不能有空白字符。
  - `lot_id` 可以省略；出现时符合批次号格式（见[「批次」](inventory.md#批次)），不接受 `null`。
  - `acknowledged` 取值见[「异常分级」](../engineering/http.md#异常分级)确认；实时报损的可确认集合只有 `STOCK_SHORTFALL`。规范化后的 `acknowledged` 保留在规范化请求中，参与幂等比对。
- **新建**：报损 ID 是聚合 ID，写 `WASTE_LOGGED`。
  - payload 每行：`qty` 是 `input` 的单位换算结果（见[「单位」](inventory.md#单位)）；`lot_id` 取请求中指定的批次，未指定时省略；`item_book_qty`、`lot_book_qty` 和 `alloc` 按下方「逐行处理」得出。
  - payload 结构与键顺序见 `boh_domain::waste::WasteLogged`；样本 `crates/boh-app/tests/golden/WASTE_LOGGED@1/`。
  - `acknowledged` 不写进 payload：不足的触发条件可以从 payload 中的账面值判定（见[「异常分级」](../engineering/http.md#异常分级)原则）。
- **业务校验**：不足确认在其余业务校验都通过之后判定。
  - 物料不存在：`404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": "ITEM", "id"}`。指定的批次不存在：`details` 为 `{"entity": "LOT", "id"}`。
  - 报损原因不存在：`404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": "WASTE_REASON", "code"}`。
  - 指定余量为 0 或为负的批次照常受理（按下方规则需要确认）。
  - 指定的批次属于其他物料：`400 LOT_ITEM_MISMATCH`，`details` 为 `{"line", "item_id", "lot_id"}`。
  - 物料还没有任何批次（从未收货入库）：`409 ITEM_HAS_NO_LOTS`，`details` 为 `{"line", "item_id"}`。带确认也拒绝：确认只处理数量不足，不授予报损资格。
    - 依据是 `inventory_lots` 中有没有该物料的批次，不是 `inventory_on_hand` 的行（视图对每个物料都有一行）。批次由收货建立（生产产出随生产切片加入），余量为 0 后也保留，所以曾经入库、现已耗尽的物料照常按下方规则处理。
  - 库存状态上限：`409 STOCK_LIMIT_EXCEEDED`（见[「数量」](inventory.md#数量)）。按下方「逐行处理」检查每行之后的账面，需要确认的行按确认后的结果检查；前面的正常行也不入账。
  - `UNKNOWN_UNIT`、`UNIT_CONVERSION_CHANGED`：见[「单位」](inventory.md#单位)。
- **逐行处理**：按 `lines` 顺序，每行看到的账面是同一命令中前面各行处理之后的值。
  1. **吸收**：按「盘点吸收」判定（随盘点切片实现；在此之前每行都不被吸收）。被吸收的行写 `absorbed_by_event_id`，不需要确认，带了确认也忽略，不改变后续各行看到的账面。
  2. **账面**：`item_book_qty` 是该行之前的物料净账面（该物料全部批次余量之和，含负数，加账外缺口，即视图 `inventory_on_hand` 的值）。指定批次的行另记 `lot_book_qty`，即该行之前该批次的余量。被吸收的行同样记录。
  3. **是否需要确认**（未被吸收的行）：

     | 行 | 需要确认的条件 |
     |---|---|
     | 指定批次 | `qty > lot_book_qty`（批次余量为 0 或为负也算）或 `qty > item_book_qty` |
     | 不指定批次 | `qty > item_book_qty` |

     报损量等于账面时不需要确认。
  4. **分配**（未被吸收的行）：
     - 指定批次：整行扣在该批次上，`alloc` 恰好一项 `{lot_id, qty, "SPECIFIED"}`，不转去扣其他批次，该批次余量可以为负。
     - 不指定批次：按[「批次」](inventory.md#批次)的 FIFO 顺序，在该物料余量大于 0 的批次中逐个扣到 0 或扣完为止，每个批次一项，记 `FIFO`；仍有剩余时，剩余部分一项，不带 `lot_id`，记 `SHORTFALL`（记入账外缺口）。
       - 净账面不小于报损量时，正余量批次之和一定足够，所以只有经过确认的行才会产生 `SHORTFALL`。
     - `alloc` 每项 `qty` 为正，合计等于该行 `qty`。
- **不足确认**（规则见[「异常分级」](../engineering/http.md#异常分级)确认）：
  - 行内 `acknowledged` 含 `STOCK_SHORTFALL` 表示员工确认这一行的数量确实发生，允许账面不足时入账。带确认的行按提交时的账面入账，不足程度比预检时更大也有效。
  - 全部行处理完后，存在需要确认而没带确认的行时整条命令不入账，返回 `409 CONFIRMATION_REQUIRED`；每个这样的行一项 `{line, code: "STOCK_SHORTFALL", details: {item_id, qty, item_book_qty, lot_id?, lot_book_qty?}}`，`lot_id` 与 `lot_book_qty` 同时出现（指定批次的行）。
- **重提**：
  - 只有明确收到拒绝（如 `CONFIRMATION_REQUIRED`）之后，才可以用同一个 `command_id` 修改内容重提：被拒绝的命令不落库。确认后重提时 `occurred_at` 沿用首次录入的值；超过 72 小时返回 `CAPTURE_TOO_OLD`，改走补录。
  - 结果未知（超时、断线）时按[「命令提交与结果未知」](command-submission.md#命令提交与结果未知)由后台任务用原 ID、原内容重试，不能直接加确认。首次已成功时，原内容重试返回首次响应，即使已超过 72 小时（幂等检查先于时间校验）。
  - 成功之后用同一个 ID 增加、去掉或改变确认：`409 IDEMPOTENCY_CONFLICT`，`fields` 为 `["lines"]`。
  - 修改某行的物料、批次或数量后，客户端清除该行的确认，重新预检。
- **警告**：被吸收的行返回 `ABSORBED_BY_COUNT`（见「盘点吸收」；吸收判定随盘点切片实现，现在不产生）。
  - `STOCK_SHORTFALL` 不是警告码（见[「异常分级」](../engineering/http.md#异常分级)）：不足已在入账前提示并确认，事件中有提交时的账面。
- **投影**：
  - 未被吸收的行：`alloc` 按顺序每项写一条 `inventory_movements`，`kind = 'WASTE'`，`alloc_source` 取该项的 `source`，`lot_id` 取该项的 `lot_id`（`SHORTFALL` 为 `NULL`），`nominal_qty = qty_delta = −qty`，`physical_at = occurred_at`。
    带 `lot_id` 的项把该批次的 `remaining_qty` 减去 `qty`；`SHORTFALL` 项把该物料的账外缺口减去 `qty`，`inventory_unallocated` 没有该物料的行时新建。
  - 被吸收的行：写一条 `kind = 'WASTE'`、`alloc_source = 'ABSORBED'`、`lot_id` 为 `NULL`、`nominal_qty = −qty`、`qty_delta = 0` 的流水，`absorbed_by_event_id` 取自 payload，不改动批次和账外缺口。
  - `reason_code`、`item_book_qty`、`lot_book_qty` 只在事件中。

**预检**：

- 请求体见 `boh_domain::waste::PrecheckWaste`，只有 `lines`，取值规则同正式提交；不接受 `command_id`、`occurred_at`、`clock_synced_at` 和行内的 `acknowledged`。
- 在一个读事务中执行，不写事件、`processed_commands` 或任何表；没有幂等处理。
- 校验与正式提交相同（取值、引用、`LOT_ITEM_MISMATCH`、`ITEM_HAS_NO_LOTS`、单位、库存状态上限），错误码与 `details` 也相同。不做时间校验，也不做吸收判定（没有发生时间）。
- 成功的 `data` 是 `{"lines": [{line, item_id, lot_id?, qty, item_book_qty, lot_book_qty?, alloc}], "confirmations": [...]}`：`lines` 每行一项，按正式提交的逐行处理计算，假设全部行都不被吸收、需要确认的行都已确认；`confirmations` 列出正式提交不带确认时需要确认的项，结构与排序同 `409 CONFIRMATION_REQUIRED` 的 `details.confirmations`，没有时为空数组。`warnings` 为空数组。
- 结果是按当前账面的估算：正式提交在写事务内重新计算，两次请求之间账面可能变化。界面把 `alloc` 显示为「按当前账面预计扣减」。

## 生产规则

实现状态：尚未实现，随生产切片实现。

- 计划用量 = 配方版本中每批用量（基本单位）× `batch_count`；计划产出 = 每批产出 × `batch_count`。`batch_count` 是正整数，只有定点数量乘整数，没有除法和舍入。
- 命令携带 `recipe_version`，按该版本计算。版本存在即可，不要求是当前版本。
- 员工可以修改每行实际 `qty`、增加配方外的物料（`planned_qty = 0`）、把某行改为 0，也可以修改实际产出（烤坏的直接少计产出，不另记报损）。
- 半成品（面团、馅料）也是物料，可以作为另一个配方的原料。
- 生产界面在员工点「开始生产」时记录 `started_at`（只存在客户端，提交时一起带上）。用料的实物时点取开始时间，见「盘点吸收」。

## 盘点

实现状态：尚未实现，随盘点切片实现。

1. **盘点表与草稿**：客户端按库存明细生成盘点表，不调用专门的盘点接口，不产生事件。库存明细查询返回物料余量不为 0 的批次、账外缺口及门店节点当前的 `business_date`（见[「库存明细查询」](inventory.md#库存明细查询)）。
   - 创建盘点时选择要盘的物料：物料列表每行最左侧有勾选框，表头最左侧提供全选、反选；按勾选结果生成单物料、部分物料或全部物料的盘点表（全部 N 种物料中的 1–N 种）。选中的每个物料仍按第 3 条的完整批次范围清点和校验。
   - 每个批次行显示批次号、收货或生产时间、到期日、生产商批号。盘点表不预填清点数量，有空格时客户端不允许提交。
   - 余量为 0 的已有批次确认有实物时，按标签上的批次号经[「批次查询」](inventory.md#批次查询)找到，补加到盘点表。
   - 草稿按设备保存在 App 的本地存储中，App 重启、设备休眠后都能恢复。草稿保存清点数及物料 / 批次标识、营业日和重试所需的命令信息，不保存库存余量，不改变「客户端不维护库存镜像」的规则。
   - 提交成功后删除草稿；提交被拒时保留已填的数，刷新盘点表后只补新出现的批次行。
   - 首次提交时生成的 `command_id` 存进草稿。结果未知时草稿锁定为只读，按[「命令提交与结果未知」](command-submission.md#命令提交与结果未知)转入后台任务；工作站核对为没有收到时，先在平板取消，再重新清点，用新命令提交。
   - **草稿跨营业日不能提交**：草稿保存打开盘点表时门店节点返回的当前 `business_date`；首次提交或被拒后重新提交前，用库存明细查询返回的当前 `business_date` 核对，跨营业日时清空草稿、重新清点。客户端不自己计算时区和日切。结果未知的命令先取回结果或在平板取消，再处理草稿的营业日。
2. **提交盘点**：命令携带 `purpose`（`CLOSING` / `AUDIT`）和逐批次的清点结果，`occurred_at` 取员工点「完成盘点」时刻的 App 时间，同一事务内写入 `STOCK_COUNT_SUBMITTED` 和 `STOCK_ADJUSTED`。不加审批，提交即调整，录错就重盘。
   - 同一营业日可以分几次提交，每次盘一组物料；同一营业日分次提交的 `CLOSING` 盘点各自入账。拆分的单位是物料：一个物料在一次提交中清点它全部存放位置的实物，不能按存放区域拆到几次提交。本期不加存放位置字段。
3. **范围与校验**：只针对 `lines` 里出现的物料。对其中每个物料，范围是该物料的全部批次（含余量为 0 的）加上账外缺口，范围与余量一律以门店节点提交时的值为准。
   - 填入的数量一律视为实有数量，没填不等于 0。被盘物料中余量不为 0（含负余量）的每个批次都必须出现在 `lines` 中，实物没有就填 0；缺少时返回 `400 COUNT_LINE_MISSING`，列出缺少的批次，整条命令不入账。打开盘点表之后新建的批次同样校验，客户端刷新盘点表后补填。
   - 余量为 0 的批次可以不列，也可以补加。每行都必须带 `lot_id`，缺少时 `400 VALIDATION_FAILED`：盘点只调整已有批次，不新建批次。
   - 每行带 `input`（见[「单位」](inventory.md#单位)），`counted_qty` 是换算后的基本单位数量。`input.qty` 不能为负，0 合法；任一行为负时 `400 VALIDATION_FAILED`，整条命令不入账，即使各行合计为正。
   - 盘点成功后，本次被盘物料的各批次余量非负、账外缺口为 0、物料账面非负；之后的业务按各自的规则处理。
   - 尚未完成收货或生产登记的实物不能混入已有批次的清点数：暂停该物料的盘点，先完成登记，再重新清点该物料；来源不明的实物按 [盘点](../operations/sop.md#盘点)处理。
   - `lines` 中的 `lot_id` 不属于该物料或不在范围内：`400 LOT_NOT_IN_COUNT_SCOPE`；同一批次出现两次：`400 DUPLICATE_COUNT_LINE`。
4. **逐批次调整**：
   - 对每个列出的批次：`delta = counted_qty − remaining_qty`，`remaining_qty` 取提交时的余量（可为负，例如余量 −3 盘得 2，`delta` 为 +5）；未列出的零余量批次不调整。
   - 账外缺口清零。
   - `book_qty`、`delta` 都写进 payload，重放时不重新计算。`STOCK_ADJUSTED.lines` 包含范围内余量不为 0 或被清点的每个批次，另外每个物料恰有一行不带 `lot_id`：`book_qty` 是账外缺口，`counted_qty` 为 0，`delta = −book_qty`（归零）。
5. **提交响应**：响应 `data` 返回服务端计算的 `lines[{item_id, lot_id?, book_qty, counted_qty, delta}]`，与第 4 条的调整 payload 一致。界面以响应为准，不显示客户端自己计算的差异。同一 `command_id`、同一规范化请求重发时，原样返回首次的响应（含 warnings），不按当前库存重新计算，`seq` 不增加。
6. **盘点投影**：被盘的每个物料写一行 `inventory_counts`（零差异也写）：`book_qty` 是提交时范围内批次余量与账外缺口之和，`counted_qty` 是清点合计。分次提交分别写入。
7. **操作规范**：盘点只数实物，清点到提交期间被盘物料不得变动，见 [盘点](../operations/sop.md#盘点)。
   系统不检测盘点期间的变动，违反规范时差异会算错，由店长重盘。

## 盘点吸收

实现状态：部分实现。报损被吸收行的 payload 类型和投影重放已支持；写入路径的吸收判定随盘点切片实现。

**原则**：一次盘点把账面校准到观察时点的实物数量。某个库存影响如果在这次盘点**入账之后**才入账，但实物影响发生在观察时点**之前**，盘点已经把它算进去了，它不应再改动账面。
盘点只能看到盘点时已在账上的批次，所以只吸收扣减类影响（报损、生产用料）以及对盘点时已存在批次的纠错；新建批次的收货行、生产产出不吸收（见「边界」）。

**定义**：

- 库存影响行 e：某个事件对物料 I 的一行影响，名义变动量 `nominal_qty`。
- 实物时点 t(e)：

| 影响来源 | t(e) |
|---|---|
| 报损 | 事件的 `occurred_at` |
| 生产用料 | 生产的 `started_at`；没有时取 `occurred_at` |
| 冲销行、更正行 | 原行的 t（原行是收货行、生产产出时为原事件的 `occurred_at`） |

- 盘点 c 对物料 I 的观察时点 o(c)：盘点事件的 `occurred_at`。

**规则**：写入 e 时，在已入账、覆盖了物料 I、且 o(c) > t(e) 的盘点中，取 o(c) 最早的一次。e 是冲销行或更正行、且原行新建了批次（收货行、生产产出）时，候选盘点还必须在该批次建立之后入账（`c.event_seq` 大于该批次的 `source_event_seq`，见「边界」）：先按这个条件筛选，再取 o(c) 最早的。

```sql
SELECT e.id FROM inventory_counts c JOIN store_events e ON e.seq = c.event_seq
WHERE c.item_id = ?1 AND c.observed_at > ?2
  AND (?3 IS NULL OR c.event_seq > ?3)
ORDER BY c.observed_at, c.event_seq LIMIT 1
```

`?1` 是物料，`?2` 是 t(e)，`?3` 只在上述情况传原批次的 `source_event_seq`，其余影响（报损、生产用料，以及对它们的冲销、更正）传 `NULL`。

查到时 e 被它吸收：

- `qty_delta = 0`，`absorbed_by_event_id = c 的 STOCK_ADJUSTED 事件 id`，`lot_id = NULL`，`alloc_source = 'ABSORBED'`；
- 不分配批次，不新建批次，不改动账外缺口；
- 事件照常入账，payload 记录 `absorbed_by_event_id`，重放时直接读取，不重新判定；
- 返回警告 `ABSORBED_BY_COUNT`，列出受影响的物料和对应的盘点。

已入账盘点的 `seq` 一定小于正在写入的 e，所以除上述新建批次行的纠错外，只需比较时间。原行被吸收时，它的冲销 / 更正用同一个 t，也会被同一次盘点吸收。
同一事件中可以有的行被吸收、有的行正常生效（例如原料盘过、成品没盘过）。

**报表含义**：被吸收的行照常计入自己的类别（报损、收货…），同时冲减吸收它的那次盘点的差异。盘点差异里原本就含着这笔数量，扣掉后剩下的才是真实差异（公式见「销售导入」）。

**边界**：

- 新建批次的行（收货行、生产产出）不吸收，照常建批次：盘点时这批货还没有批次号，按 [盘点](../operations/sop.md#盘点)不会被清点进已有批次，而是先登记、再单独清点。
- 冲销或更正新建批次的行时，只有在建批次事件入账之后提交的盘点（`c.event_seq` 大于该批次的 `source_event_seq`）才能吸收它；更早入账的盘点看不到这个批次。

## 纠错

实现状态：尚未实现，随纠错切片实现。

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
- **扣回时批次余量不够**（原批次已被后续扣减）：需要确认（`STOCK_SHORTFALL`，见[「异常分级」](../engineering/http.md#异常分级)），确认页提示「建议现在盘点该物料」。
  - 数量更正：批次真实存在，差额直接从原批次扣，原批次余量可以为负。下一次盘点该批次时校准。
  - 整条冲销：收货或生产实际没有发生，批次扣到 0，不足部分记入账外缺口（差额不属于任何真实批次）。下一次包含该物料的盘点会清零，不会在盘点差异里重复出现。
- **退回会使账外缺口大于 0**（原分配含账外缺口，而该缺口已被一次入账在后、观察时点却早于原行的盘点清零，只在违反盘点操作规范时出现）：`409 COUNT_REQUIRED`。先盘点该物料，新盘点的观察时点晚于原行，纠错随后会被吸收。

**数量更正 `QUANTITY_CORRECTED`**：

- `line_ref` 定位原事件中的一行：`lines[i]`（收货、报损）、`consumed[i]`、`output`（生产），`i` 为原数组下标。`item_id` 必须与该行一致，否则 `400 LINE_MISMATCH`。
- `old_qty` 是该行当前有效数量，`delta = new_qty − old_qty`，`delta ≠ 0`，`new_qty >= 0`。原行带 `input` 时（收货、报损），新数量照常带 `input`；生产的行不带。
- 收货行可同时更正 `line_cost_cents`，取值同「收货接口」。生产只更正数量，不更正 `batch_count` 和配方（录错配方要冲销）。
- 未被吸收的行：

  | 行类型 | `delta > 0` | `delta < 0` |
  |---|---|---|
  | 新建批次的行（收货行、生产 `output`） | 原批次 + delta | 从原批次扣回，原批次余量可以为负 |
  | 扣减库存的行（生产 `consumed`、报损行） | 指定批次的报损行从原批次追加扣减（可为负）；其他按 FIFO 追加分配 | 按有效分配的**逆序**退回（最后分配的先退，账外缺口部分退回账外缺口） |

**整条冲销 `EVENT_REVERSED`**：

- 逐行处理原事件中影响库存的每一行，数量取有效数量。
- 未被吸收的行精确取反：扣减类的行按有效分配加回原批次 / 账外缺口；新建批次的行从原批次扣回（不足部分进账外缺口）。

## 销售导入

实现状态：尚未实现，随销售导入切片实现。

**规则：销售只进报表，不扣库存。** 成品库存只由收货、生产产出、报损、生产用料和盘点决定。
导入与闭店盘点的先后无法保证；销售如果扣库存，先盘点再导入时同一批销售会被扣两次。原料在生产完成时已经扣减。

- 导入文件由我方定义字段，用户按此配置 POS 导出。**每个营业日、每个商品一行的汇总**：

  | 列 | 类型 | 说明 |
  |---|---|---|
  | `business_date` | `YYYY-MM-DD` | 整个文件必须相同 |
  | `item_code` | 文本 | 物料的 `code` |
  | `qty` | 小数，最多 3 位 | 售出数量，基本单位；退货为负 |
  | `amount_cents` | 整数 | 实收金额，单位分 |

- Excel / CSV 在**前端 App 中解析**，转成统一的 JSON 命令提交。Rust 端只处理 JSON，不为 Excel 引入依赖。命令走普通的 write path，幂等规则不变。
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

## 已知限制

- **盘点期间的实物变动**：系统不检测，靠操作规范禁止，违反时由重盘纠正。

## 验收用例

写成锁定测试之前，预期数值由人工逐条确认（见 [测试分工](../engineering/development.md#测试分工)）。初始状态：账面 = 实物。
已锁定的主题，完整算例在锁定测试中，这里只留一个代表例子；尚未实现的切片的用例在「待实现」表中。

### 已锁定

| 主题 | 代表例子 | 预期 | 锁定测试（`crates/boh-app/tests/`） |
|---|---|---|---|
| 报损不足确认与分配 | 只有批次 A 5；报损 8 g，不指定批次 | 不带确认时 `409 CONFIRMATION_REQUIRED`（`STOCK_SHORTFALL`）；确认后 `A 5 FIFO` + `3 SHORTFALL`；A **0**，账外缺口 **−3**，账面 **−3**；`warnings` 中没有 `STOCK_SHORTFALL` | `spec_waste.rs` |
| 批次号与 FIFO | 面粉：09-10 08:00 收货一行；09-10 14:00 收货两行；09-11 收货一行；09-12 入账一张 `occurred_at` 为 09-09、早于 `recorded_at` 不到 72 小时的收货 | 批次号依次为 `RAW-FLOUR-20260910-001`、`…-002`、`…-003`、`…-20260911-001`、`…-20260909-001`；FIFO 先扣 `…-20260909-001` | `spec_lot_numbers.rs`、`spec_waste.rs` |
| 批次号重建 | 同次收货 A、B 后清空投影、按 `seq` 重建，再报损 8 g，不指定批次 | 批次号不变；分配 `A 5 FIFO` + `B 3 FIFO` | `spec_replay.rs` |
| 收货日期 | 10-08 02:00 收货（营业日 10-07），`produced_on` 10-08 | 照常受理；`produced_on` 10-09 时 `400 INVALID_LOT_DATES`，`reason` `PRODUCED_IN_FUTURE` | `spec_receiving.rs` |
| 单位换算 | 录入「2 袋，每袋 25000 g」后、提交前换算改为 20000 | `409 UNIT_CONVERSION_CHANGED`，不入账 | `spec_receiving.rs`、`spec_waste.rs` |
| 幂等 | 同一命令发送 2 次，`clock_synced_at` 不同 | 第二次原样返回首次响应（含 warnings），`seq` 不增加 | 各接口的 `spec_*.rs` |
| 单位换算与舍入 | 物料基本单位 g，单位 oz 系数 `28.349523125`；收货 `1.25` oz | `qty` **`35.437`**（35.43690390625 四舍五入，中点远离 0）；换算结果为 0 时 `400 VALIDATION_FAILED` | `spec_receiving.rs`、`spec_waste.rs`、`spec_recipes.rs` |
| 录入上限与库存状态上限 | 面粉账面 `999990000` g；一次收货两行各 `5` kg，再收货 `0.001` g | 前者之后账面 `1000000000`，照常受理；后者 `409 STOCK_LIMIT_EXCEEDED`，`details` `{line 0, item_id}`，不入账 | `spec_receiving.rs`、`spec_waste.rs` |
| 新批次余量越界 | 面粉批次 A −1000000000 g、B 0，净账面 −1000000000 g；收货 `1.001` 个 `max`（系数 `1000000000`） | 净账面只到 `1000000`，但新批次余量超过上限：`409 STOCK_LIMIT_EXCEEDED`，`details` `{line 0, item_id}`，不带 `lot_id`，不入账；改为 `1` 个 `max` 时新批次恰好为上限，照常受理 | `spec_waste.rs` |
| 小数字符串 | 数量 `"5.500"`；同一 `command_id` 再以 `"5.5"` 重发 | payload 与响应中为 **`"5.5"`**；重发返回首次响应；数量写成 JSON 数字时 `400 VALIDATION_FAILED` | `spec_receiving.rs` |

### 待实现

| 用例 | 过程 | 预期 |
|---|---|---|
| 生产期间盘点 | 面粉 100；09:10 开始生产，取 20；09:20 盘点得 80（调整 −20）；09:30 完工，用料 20，`started_at = 09:10` | 用料被吸收；账面 **80**；返回 `ABSORBED_BY_COUNT` |
| 生产没带开始时间 | 同上，没有 `started_at` | 不被吸收，账面 **60** |
| 生产开始晚于完成 | 实时录入或补录的 `started_at = 09:30`、`occurred_at = 09:10` | `400 INVALID_PRODUCTION_TIME`；库存、事件和 `processed_commands` 不变 |
| 生产开始等于完成 | `started_at = occurred_at`，其余内容合法 | 时间顺序合法，照常处理 |
| 开始时间超过 72 小时 | `occurred_at` 比 `recorded_at` 早 1 小时，`started_at` 早 72 小时零 1 ms | `400 CAPTURE_TOO_OLD`；不入账 |
| 用料改为 0 | 配方每批用面粉 100 g，`batch_count` 1，没有符合吸收条件的盘点；员工把面粉的实际 `qty` 改为 0 | 面粉行 `planned_qty` **100**、`qty` **0**、`alloc` 为空数组；面粉账面不变，不写该行流水 |
| 配方外物料 | 黄油有足够的正余量批次，没有符合吸收条件的盘点；生产时增加配方外的黄油 5 g | 黄油行 `planned_qty` **0**、`qty` **5**，按 FIFO 扣减 |
| 迟到报损 | 100；09:00 报损 10，提交时结果未知；09:20 盘点得 90（调整 −10）；平板后台任务 09:40 用原 ID 重试送达 | 被吸收；账面 **90**；未解释损耗 **0** |
| 补录报损 | 100；15:00 盘点得 50（调整 −50）；16:00 补录报损 50，`occurred_at = 10:00` | 被吸收；账面 **50**；报损 **50**；未解释损耗 **0** |
| 盘点后冲销 | 100；09:00 误报损 10，账面 90；09:20 盘点得 100（调整 +10）；10:00 冲销报损 | 被吸收；账面 **100**；报损合计 **0**；未解释损耗 **0** |
| 盘点后的正常业务 | 09:20 盘点；10:30 报损并提交 | 不被吸收，正常扣减 |
| 盘点后补录收货 | 09:00 到货一袋面粉未登记，09:20 盘点面粉时未清点它；09:40 补录收货，`occurred_at = 09:00` | 不被吸收，照常建批次；账面 = 盘点数 + 这袋面粉 |
| 冲销盘点后补录的收货 | 上例补录的收货录错，10:00 冲销 | 不被 09:20 的盘点吸收（盘点时该批次不存在）；从该批次扣回 |
| 发生时点等于观察时点 | 面粉 100；09:20 盘点得 90（调整 −10）；之后入账一条 `occurred_at = 09:20` 的报损 10 | 不被吸收（盘点的观察时点须晚于实物时点）；正常扣减，账面 **80** |
| 多次盘点取观察最早的 | 面粉 100；09:20 盘点得 90（调整 −10）；09:40 盘点得 85（调整 −5）；之后送达 09:00 的报损 10 | 被 09:20 的盘点吸收；账面 **85** |
| 较早观察的盘点较晚入账 | 面粉 100，09:00 实物报损 10 g 未入账；09:40 观察的盘点先入账，得 90（调整 −10）；09:20 观察的盘点（提交时结果未知、之后送达）后入账，得 90（调整 0）；之后送达 09:00 的报损 10 | 被 09:20 观察的盘点吸收（观察时点最早，不按 `seq`）；账面 **90** |
| 同一命令部分吸收 | 面粉 100、黄油 50；09:00 一次报损面粉 10、黄油 5，提交时结果未知；09:20 只盘面粉得 90；平板后台任务 09:40 用原 ID 重试送达 | 面粉行被吸收，面粉账面 **90**；黄油行正常扣减，黄油账面 **45**；`ABSORBED_BY_COUNT` 只涉及面粉 |
| 冲销被吸收的报损 | 面粉 100；09:00 报损 10，提交时结果未知；09:20 盘点得 90（调整 −10）；平板后台任务 09:40 用原 ID 重试送达，被吸收；10:00 冲销这条报损。该窗口内没有其他调整或吸收行，销售 0 | 冲销同样被 09:20 的盘点吸收；账面 **90**；报损合计 **0**；未解释损耗 **10**（盘点少的 10 不再由报损解释） |
| 按批次盘点 | 批次 A 5、B 10，账外缺口 −2；清点 A 4、B 10 | A 调整 −1；账外缺口 +2 归零；账面 **14** |
| 盘点负余量批次 | 批次 A −3；清点 A 2 | A 调整 **+5**；A **2** |
| 盘点补加零余量批次 | 批次 A 0（已登记），实物找到 3；按批次号补加 A，清点 3 | A 调整 **+3**；A **3** |
| 盘点行不带批次 | 提交一行不带 `lot_id` | `400 VALIDATION_FAILED`；不入账 |
| 盘点漏填批次 | 批次 A 5、B 10；只提交 A 4 | `400 COUNT_LINE_MISSING`，列出 B；不入账 |
| 盘点填 0 | 批次 A 5、B 10；提交 A 4、B 0 | A 调整 −1，B 调整 −10；账面 **4** |
| 盘点后新到批次 | 批次 A 5；打开盘点表后收货建批次 C 10；提交 A 5 | `400 COUNT_LINE_MISSING`，列出 C；不入账 |
| 余量以提交时为准 | 打开盘点表时 A 5；提交前另一台平板报损 A 2 已入账；提交 A 4 | `book_qty` **3**，`delta` **+1**；账面 **4**；响应返回这些值 |
| 盘点重复提交 | 盘点成功后又报损该物料；同一 `command_id` 重发盘点 | 原样返回首次的响应；`seq` 不增加 |
| 分次闭店盘点 | 面粉账面 100、黄油账面 50；21:00 `CLOSING` 只盘面粉得 90；21:30 `CLOSING` 只盘黄油得 48 | 面粉调整 −10、黄油调整 −2；各写一行 `inventory_counts` |
| 零差异盘点 | 批次 A 5，没有账外缺口；清点 A 5 | 照常写 `STOCK_COUNT_SUBMITTED`、`STOCK_ADJUSTED`；调整含 A 行（`book_qty` **5**、`delta` **0**）和账外缺口行（`book_qty` **0**、`delta` **0**）；不写库存流水；写一行 `inventory_counts`（`book_qty` 5、`counted_qty` 5） |
| 只剩账外缺口的物料 | 批次 A 0（已耗尽），账外缺口 −3；补加 A，清点 0 | A 保持 **0**；账外缺口调整 **+3** 归零；账面 **0** |
| 清点数为负 | 批次 A、B；提交 A −1、B 5 | `400 VALIDATION_FAILED`；整条不入账（合计为正也拒绝） |
| 盘点超过 72 小时 | 盘点的 `occurred_at` 比 `recorded_at` 早 72 小时零 1 ms | `400 CAPTURE_TOO_OLD`；不入账 |
| 冲销时批次已消耗 | 收货批次 L 10，已用掉 3，之后没有盘点；冲销这次收货 | 不带确认时 `409 CONFIRMATION_REQUIRED`（`STOCK_SHORTFALL`）；确认后 L 扣到 **0**；账外缺口 **−3** |
| 数量更正 | 收货录成 1000（实际 100），批次 L1；生产从 L1 扣 300；之后没有盘点；更正为 100 | 不带确认时 `409 CONFIRMATION_REQUIRED`（`STOCK_SHORTFALL`）；确认后 L1 **−200**；账外缺口 **0** |
| 更正多次后冲销 | 报损 10 → 更正为 8 → 更正为 6 → 冲销 | 冲销退回 **6**；报损合计 **0** |
| 重复冲销 | 同一事件冲销两次 | 第二次 `409 ALREADY_REVERSED` |
| 冲销后更正 | 冲销后再更正同一事件 | `409 ALREADY_REVERSED` |
| 冲销含账外缺口的报损 | 只有批次 A 5；报损 8 g、不指定批次（确认）→ A 0，账外缺口 −3；之后没有盘点；冲销这条报损 | A 加回 **5**，账外缺口加回 **3** 归零，账面 **5**；alloc `A 5 REVERSAL` + `3 REVERSAL`（不带 `lot_id`） |
| 退回会让账外缺口变正 | 只有批次 A 5；10:00 报损 8 g（确认）→ A 0，账外缺口 −3；一台平板 09:00 完成、提交时结果未知的盘点 10:30 才送达入账（A 清点 5 → A 5，账外缺口清零）；冲销 10:00 的报损 | `409 COUNT_REQUIRED`；整条命令不入账。重新盘点该物料（观察时点晚于 10:00）后再冲销，冲销被新盘点吸收 |
| FIFO 报损减量按逆序退回 | 批次 A 5、B 10（A 在前），之后没有能吸收更正的盘点；报损 8 g、不指定批次 → `A 5 FIFO` + `B 3 FIFO`（A 0、B 7）；更正为 6 | 先退最后分配的 B：alloc `B 2 CORRECTION`；A **0**、B **9** |
| 将报损中的一行更正为零 | 面粉批次 A 20 g、黄油批次 B 10 g，之后没有盘点；同一报损误录面粉 10 g、正确记录黄油 3 g → A 10、B 7；实际没有面粉报损，将面粉行更正为 0，`input` 为 `{qty: 0, unit_code: "g", base_qty_per_unit: 1}` | 更正成功，`delta` **−10**；alloc `A 10 CORRECTION`；A **20**、B **7**；报损有效数量面粉 **0**、黄油 **3**；原事件不变 |
| 补录时间边界 | 未触发锁账限制；补录 `occurred_at` 等于 `recorded_at`；另一次晚 1 ms | 前者照常受理；后者 `400 INVALID_OCCURRED_AT` |
| 补录旧日期收货的批次号 | 面粉在 09-10、09-11 已有批次，09-09 没有批次；09-12 经补录入口补一张 `occurred_at` 为 09-09 的收货，未被锁账限制拒绝 | 批次号为 `…-20260909-001`；FIFO 排在 09-10、09-11 的批次之前 |
| 盘点不接受补录 | 盘点请求体带 `occurred_at` | `400 VALIDATION_FAILED`；不入账 |
| 闭店重盘 | 成品账面 100；先盘为 80，再盘为 90；销售 0 | 调整净和 **−10**；未解释损耗 **10** |
| 销售示例 | 生产 100、报损 8、闭店实数 5（调整 −87）、销售 84 | 未解释损耗 **3** |
| 重新导入少了商品 | 10-08 第一次导入可颂 10、吐司 5；用新的 `command_id` 再次导入，只有可颂 12 | `SALES_DAY` version **2**；10-08 的 `daily_sales` 只剩可颂 **12**，吐司当日的行删除 |
| 相同内容重新导入 | 10-08 已导入一次；用新的 `command_id` 第二次导入内容相同的文件；再用第二次导入的 `command_id` 原样重发 | 第二次导入使 version **+1**；重发返回第二次导入的首次响应，version 不变 |
| 退货为负 | 导入一行可颂 `qty` −2 | 照常受理；`daily_sales` 记 **−2**，不改变库存 |
| 未知编码与忽略编码同时出现 | 文件中有一个未知编码，另有一个在忽略清单中的编码 | `400 UNKNOWN_ITEM_CODE`，只列未知编码；整份不入账 |
| 窗口跨营业日 | 成品的上一窗口在 10-07 闭店盘点时闭合；10-08 生产 100、销售 50，没有闭店盘点；10-09 销售 30，闭店盘点得 15（调整 −85）；没有其他调整或吸收行 | 窗口覆盖 10-08、10-09；未解释损耗 **5**；报表标注跨越的日期 |
| 结果为负 | 生产 100、闭店实数 95（调整 −5）、销售 8 | 未解释损耗 **−3**，作为异常显示，不截为 0 |
