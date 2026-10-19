# 库存共同规则与查询

库存命令、投影或查询须读本文件；按 [business/operations.md](operations.md) 读取具体操作。代码职责见 [库存规则归属](../engineering/architecture.md#库存规则归属)，事务与重放见 [engineering/storage.md](../engineering/storage.md)。共同规则的验收入口见 [已锁定用例](operations.md#已锁定)。

## 数值

- **禁止浮点数参与业务计算**（clippy `float_arithmetic = deny`）。
- 金额：`i64`，单位“分”。
- 数量（重量 / 体积 / 个数）：基本单位（克、毫升、个）的定点小数，最多 3 位小数，存千分之一基本单位的 `i64`。
- 换算系数：最多 9 位小数，存十亿分之一基本单位的 `i64`。
- 数量和换算系数在事件 payload 与 HTTP 中写成小数字符串，格式、上限与舍入见 [数量](#数量)、[单位](#单位)。
- 只在本文件[单位](#单位)规定的位置舍入（单位换算）；定点乘法的中间值用 `i128`，舍入后再转回 `i64`。
- `[profile.release] overflow-checks = true`；业务计算一律用 `checked_*`。

## 数量

- 数量是基本单位的定点小数，最多 3 位小数，例如 `11680.5` g。账本、投影、HTTP 接口表示同一个值：
  - 投影表的数量列存千分之一基本单位的 `INTEGER`：`11680.5` g 存为 `11680500`。
  - 事件 payload、HTTP 请求与响应（含错误和警告的 `details`）中写成小数字符串，不用 JSON 数字。
    理由：JSON 数字在平板的 JS 中被解析成双精度浮点。
  - 换算系数的表示见[「单位」](#单位)。金额仍是整数分，用 JSON 整数。
- **小数字符串**：
  - 接受 `^-?(0|[1-9][0-9]*)(\.[0-9]{1,n})?$`，`n` 是该字段允许的小数位数（数量为 3，换算系数为 9）。不接受 JSON 数字、正号、指数、首尾空白、整数部分的前导零、`.5`、`5.`、负零。负号只出现在允许负数的字段中。
  - 请求中小数部分可以带末尾的 0（`"5.500"`）；超过允许位数时 `400 VALIDATION_FAILED`，不舍入。
  - 输出（payload、响应、规范化请求）只用规范形式：去掉小数部分末尾的 0，小数部分全为 0 时去掉小数点，零写作 `"0"`。所以 `"5.500"` 与 `"5.5"` 的规范化请求相同。payload 中只出现规范形式。
- **录入上限**：员工录入的数量（收货、报损、盘点清点数、数量更正的新数量、配方的每批产出和每批用量；生产、销售导入随各自切片按同一规则）在录入时所选的单位下，绝对值不超过 `9999.999`。
  - 只检查录入的数字，不看换算后的值：`9999.999` kg 合法，`10000` g 不合法。
  - 超出时 `400 VALIDATION_FAILED`，属于取值，在幂等检查之前。不引导员工换单位；需要更大单位的物料按 [数量](../operations/sop.md#数量)在主数据中定义。
- **库存状态上限**：L = `1000000000` 个基本单位（存储值 `10^12`）。
  - 改变库存的命令按行处理，每行处理之后（看到同一命令中前面各行的结果），该行涉及的物料净账面、批次余量和账外缺口的绝对值都不超过 L。
  - 超出时整条命令不入账：`409 STOCK_LIMIT_EXCEEDED`，`details` 为 `{"line", "item_id", "lot_id"?}`，`line` 是按处理顺序第一个越界的行；该行有已有批次的余量越界时带该批次的 `lot_id`，只有新建批次、物料净账面或账外缺口越界时不带。不得绕回、截断或钳制后继续。
  - 属于业务校验，在幂等检查之后。适用于收货、报损（含预检）；生产、盘点、纠错随各自切片适用。
  - 理由：写进去的状态一定能被查询、扣减和盘点归零；盘点的调整量最多 2L，仍在 `i64` 之内。

## 单位

- 每个物料只有一个基本单位（`g` / `ml` / `pcs`）。账本、投影中的数量都是基本单位（见[「数量」](#数量)）。
- **换算系数** `base_qty_per_unit`：1 个该单位等于多少基本单位。大于 0，不超过 `1000000000`，最多 9 位小数，例如 1 oz = `28.349523125` g、1 mg = `0.001` g。
  - 投影表存十亿分之一基本单位的 `INTEGER`；事件 payload 和 HTTP 中是小数字符串（见[「数量」](#数量)）。
  - 理由：系数保留的位数比数量多。系数的误差会随录入数量放大，只保留 3 位时，录 16 oz 与录 1 lb 得到的克数不同。
  - 主数据写入时校验（本地编辑和总部包导入都一样），系数不合格的变更整条拒绝，不进入事件流。
- 命令用 `input {qty, unit_code, base_qty_per_unit}` 提交，其中系数是客户端录入时看到的值。
  - 取值：`input.qty` 大于 0，最多 3 位小数，符合[「数量」](#数量)的录入上限（数量更正的新数量和盘点清点数可以为 0，见[「纠错」](operations.md#纠错)、[「盘点」](operations.md#盘点)）；`input.base_qty_per_unit` 符合上面的系数规则；`input.unit_code` 非空、首尾不能有空白字符。不满足时 `400 VALIDATION_FAILED`。
- **单位换算**（`convert_unit`）：`qty = input.qty × input.base_qty_per_unit`，每行单独换算，结果四舍五入到 0.001 基本单位，中点远离 0（`0.0005` → `0.001`，`−0.0005` → `−0.001`）。
  - 例：1.25 oz × `28.349523125` = 35.43690390625 g，记 `35.437` g。
  - `input.qty` 大于 0 而换算结果为 0 时 `400 VALIDATION_FAILED`（例如 0.4 mg 换算为 0.0004 g）。换算只用请求中的值，属于取值，在幂等检查之前。
- 写入线程用当前换算核对（业务校验）：
  - `unit_code` 既不是物料的 `base_unit` 也不在 `units` 中：`400 UNKNOWN_UNIT`，`details` 为 `{"line", "item_id", "unit_code"}`；
  - 系数和当前值不相等（按数值比较）：`409 UNIT_CONVERSION_CHANGED`，`details` 为 `{"line", "item_id", "unit_code", "base_qty_per_unit": 当前值}`（`unit_code` 等于 `base_unit` 时当前值为 `"1"`），不入账，由人工确认后重新提交。
  - 配方的产出没有行下标，`details` 不带 `line`（见[「主数据接口」](master-data.md#主数据接口)配方）。
- payload 同时写入换算后的 `qty` 和 `input` 快照。

## 批次

全部物料都按批次追踪。账面数 = 批次余量之和 + 账外缺口。

- **批次号** `lot_id`：批次的唯一标识，同时是印在标签上、员工用来识别实物的号。格式 `<类型>-<编码>-<YYYYMMDD>-<流水号>`，例如 `RAW-FLOUR-20260910-001`：
  - 类型、编码取建批次时该物料的 `category`（`RAW` / `SEMI` / `FINISHED`）、`code`（只含 `A`–`Z`、`0`–`9`、`_`）；
  - 日期是批次日期（见下），必须是真实日期；
  - 流水号三位、补零，`001`–`999`。
  - 客户端提交的批次号（报损指定批次、批次查询的路径参数）按上述格式校验，不符合时 `400 VALIDATION_FAILED`。
  - 由写入线程在建批次的命令中生成，写进 payload；之后修改物料分类、重试命令、重建投影、重打标签都不改变它。
  - 不含门店代码：它只在门店内唯一，跨门店由同步批次头的 `store_id` 区分。
  - 生产商批号 `manufacturer_lot_no` 是另一项可选属性，不参与编号。
- **批次日期**：建批次事件的 `occurred_at` 换算到门店时区（配置 `timezone`）的日历日期，不用营业日。
  - 收货取收货的 `occurred_at`，生产产出取生产的 `occurred_at`（完成时刻）。
  - 例：10-08 23:50 录入、10-09 00:10 节点收到，`occurred_at` 为 10-08 23:50，批次日期为 10-08。
  - 补录时 `occurred_at` 由员工填写，批次日期随之是那一天。
- **流水号**：同一物料、同一批次日期内，按入账顺序取已有最大流水号 + 1；同一命令中靠前的行先取。
  - 同一天内不还原实物先后：A 先到但后入账，B 先入账，则 B 为 `001`、A 为 `002`。
  - 批次只增不删，已用的流水号不会再分配。
  - 超过 `999` 时整条命令拒绝：`409 LOT_SERIAL_EXHAUSTED`，`details` 为 `{"line", "item_id", "lot_date"}`（`lot_date` 为 `'YYYY-MM-DD'`），不入账，不循环使用。
- **标签打印**：标签只在建批次的命令成功之后打印，内容取自成功响应。
  - 提交结果未知时，等后台任务完成后按响应中的批次号打印；工作站核对为已入账时，按核对结果中的批次号打印（见[「命令提交与结果未知」](command-submission.md#命令提交与结果未知)）。
  - 重打标签取库存明细或批次查询中的批次号。
- **到期时间** `expires_at`（UTC 毫秒，可空）只用于过期提醒，不参与分配。收货批次必有到期时间，由收货行的 `expires_on` 换算（见[「收货接口」](operations.md#收货接口)）；生产产出的到期时间随生产切片定。
- **FIFO 分配**：未指定批次的扣减，只在该行物料余量大于 0 的批次中，按批次日期升序、同一日期内按流水号升序逐个扣减。
  - 不按 `seq`、UUID、到期日或实物发生钟点。
  - 批次日期较早的批次（例如补录或迟到入账形成的）排在较新日期的批次之前，即使入账在后。
- **来源**：批次投影保存建批次的 `source_event_seq` 和 `source_line_no`（收货取原 `lines[i]` 的 `i`，生产 `output` 取 0），只用于追溯，不参与分配。
- **指定批次**：客户端可以指定 `lot_id`。指定批次的扣减只作用于该批次，不转去扣其他批次；超出该批次账面时，按报损的确认规则入账，该批次余量可以为负。
- **账面不足**：
  - 指定批次时，差额留在该批次上（余量为负）；
  - 未指定批次时，扣完该物料全部余量大于 0 的批次后，剩余部分记入账外缺口。
  - 报损在账面不足时先提示、经员工确认后入账（见[「报损接口」](operations.md#报损接口)）；还没有任何批次的物料不能报损。
  - 下一次包含该批次或该物料的盘点会校准它们（见[「盘点」](operations.md#盘点)）。
- **负余量**：批次余量可以为负，表示账面少于实物。FIFO 跳过余量不大于 0 的批次。
- 分配结果连同来源写进 payload：`alloc[{lot_id?, qty, source}]`。
  - `qty` 为正数，不带 `lot_id` 表示账外缺口。
  - `source` 为 `SPECIFIED` / `FIFO` / `SHORTFALL`，纠错另有 `CORRECTION` / `REVERSAL`（见[「纠错」](operations.md#纠错)）。
  - 重放时直接使用，不重新分配。
- **到期提醒**：新批次的 `expires_at` 早于同物料另一个按 FIFO 先被扣的批次时，返回警告 `EXPIRES_BEFORE_OLDER_STOCK`。
  - 「另一个批次」指按 FIFO 顺序（批次日期、流水号）排在新批次之前、`remaining_qty > 0`、`expires_at` 不为空的批次，含同一命令中靠前的行新建的批次。
  - 每个命中的行一条警告，`details` 为 `{"line", "item_id", "lot_id"}`：`line` 是该行的下标，`lot_id` 是新批次。
- **承诺边界**：批次是账面推定，不是实物证据。追溯报告分开显示「员工指定」「系统推定」「账外 / 吸收」三类。

## 投影表

库存投影的字段与约束以迁移为准（`inventory_lots`、`inventory_movements` 见迁移 007，其余见迁移 005；数量与换算系数的存储单位见[「数量」](#数量)[「单位」](#单位)）：

| 表 / 视图 | 内容 |
|---|---|
| `inventory_lots` | 每个批次一行，主键是批次号 `lot_id`，余量为 0 的批次保留，余量可为负；批次日期、流水号由 `lot_id` 拆出，用于 FIFO；`source_event_seq`、`source_line_no` 是建批次的事件和原 payload 下标（见[「批次」](#批次)），之后的调整不改变它们 |
| `inventory_unallocated` | 账外缺口，每个物料至多一行 |
| `inventory_movements` | 每条库存影响按批次展开，一行一条；`event_seq` 是产生它的事件，`movement_no` 是事件内的展开编号 |
| `inventory_counts` | 每次盘点的每个被盘物料一行，零差异也写；`observed_at` 是盘点事件的 `occurred_at` |
| 视图 `inventory_on_hand` | 每个物料的账面数，没有库存时为 0 |

- `inventory_movements` 的数量带符号，增加为正：`nominal_qty` 是按申报内容应有的变动量，`qty_delta` 是实际作用于账面的变动量。
  - 未被吸收的行两者相等。被吸收的行 `qty_delta = 0`、`lot_id` 为 `NULL`；`absorbed_by_event_id` 直接取自 payload，重放时不查其他事件。
  - 未被吸收且 `lot_id` 为 `NULL` 的行作用于账外缺口。
- **展开顺序**：`movement_no` 在同一事件内从 0 开始连续编号，按以下顺序展开；在线写入和重建使用同一个顺序。
  - 行的顺序：`PRODUCTION_BATCH_COMPLETED` 先 `output`，再按下标升序的 `consumed`；其他事件按下标升序的 `lines`。
  - 每行展开为：新建批次（`lot_id`）一条；被吸收（`absorbed_by_event_id`）一条；带 `alloc` 的按 `alloc` 数组顺序每项一条。
  - `STOCK_ADJUSTED`：带 `lot_id` 的行一条，`qty_delta = delta`；不带 `lot_id` 的行一条账外缺口归零（`lot_id` 为 `NULL`，`qty_delta = −book_qty`）。
  - `nominal_qty = 0` 的展开项不写流水，也不占编号。
  - 一条 payload 行可能分到多个批次，所以流水不按 payload 行号编号。
- `kind`：收货 `RECEIPT`，生产产出 `PRODUCE`，生产用料 `CONSUME`，报损 `WASTE`，盘点调整 `ADJUST`；纠错行沿用原行的 `kind`（见[「纠错」](operations.md#纠错)）。
- `alloc_source`：新建批次 `NEW_LOT`；盘点对已有批次和账外缺口的调整 `COUNT`；被吸收 `ABSORBED`；其余直接取 payload 中 `alloc` 的 `source`。
- **不变量**（锁定测试和[不变量自检](../operations/invariant-check.md)都检查）：
  - 批次余量 `remaining_qty` = 该批次所有流水的 `qty_delta` 之和；
  - 账外缺口 = 该物料 `lot_id IS NULL` 的流水 `qty_delta` 之和，且 `<= 0`；
  - 账面数（视图 `inventory_on_hand`）= 批次余量之和 + 账外缺口。
- **日报**：视图（随报表需要定义），按 `business_date, item_id, kind` 聚合流水。收货、产出、用料、报损按 `nominal_qty`（申报值，含被吸收的行），盘点调整按 `qty_delta`。不维护增量日结表。
- 销售导入切片新增 `sales_days(business_date PRIMARY KEY, aggregate_id, version, source_event_id)`、`daily_sales(business_date, item_id, qty, amount_cents, source_event_id)`，表结构随该切片定。
- 主数据表也是投影：`equipment`（迁移 002），`items`、`item_units`、`recipes`、`recipe_versions`、`recipe_lines`、`suppliers`、`waste_reasons`（迁移 005）；`employees` 随认证切片加入。
- 所有投影都必须能通过 `boh-server rebuild-projections` 从 `store_events` **按 `seq`** 完整重建，结果与在线写入逐行一致。

## 库存明细查询

| 方法与路径 | 查询参数 | 权限 |
|---|---|---|
| `GET /api/v1/inventory` | `item_id`（可选） | 已认证员工 |

- 查询参数与 `data` 见 `boh_domain::inventory` 的 `InventoryQuery`、`Inventory`、`InventoryItem`，全部内容读自同一个读事务。
  - `business_date` 是门店节点按当前时间计算的营业日（盘点草稿用它核对，见[「盘点」](operations.md#盘点)第 1 条）。
  - `items` 每个物料一项，含停用的，按 `code` 的字节序升序；带 `item_id` 时只含该物料。
    `on_hand_qty` 是账面数（视图 `inventory_on_hand`），`unallocated_qty` 是账外缺口（`<= 0`，没有时为 0）。
  - `lots` 是该物料余量不为 0 的批次（含负余量），按 FIFO 顺序（批次日期、流水号）排列。每项是一个批次行（`LotDetails`，见[「批次查询」](#批次查询)），不含 `item_id`。
- 参数取值非法、未知或重复的参数：`400 VALIDATION_FAILED`。`item_id` 合法但不存在时 `items` 为空数组。

## 批次查询

| 方法与路径 | 权限 |
|---|---|
| `GET /api/v1/lots/{lot_id}` | 已认证员工 |

- 按标签上的批次号查一个批次，余量为 0 的也能查到（盘点补加历史批次用，见[「盘点」](operations.md#盘点)）。
- `data` 是 `{"lot": 批次行}`，批次行见 `boh_domain::inventory::Lot`（库存明细中的批次行是其中不含 `item_id` 的 `LotDetails`）。
  `source_occurred_at` 是建批次事件的 `occurred_at`；`expires_at`、`manufacturer_lot_no` 没有时省略该键。
- 路径中的 `lot_id` 不符合批次号格式（见[「批次」](#批次)）：`400 VALIDATION_FAILED`。格式合法但不存在：`404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": "LOT", "id"}`。

## 已知限制

- **批次追溯是账面推定**。员工实际拿的批次和系统推定的不一致时，批次余量会偏离实物，直到下一次盘点。
- **同一天内的批次顺序按入账先后**，不还原实物到达的先后：先到但后入账的批次流水号更大，FIFO 排在后面。
- **大数量的录入精度**：录入上限按所选单位计，大数量只能精确到大单位的 0.001。例如 11680.5 g 超过 g 的录入上限，换成 kg 是 11.6805，超过 3 位小数，只能录 `11.681` kg。账面和批次余量本身不受录入上限限制。
