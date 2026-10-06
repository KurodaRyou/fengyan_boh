# BOH 领域模型

本文件定义业务决策、事件目录、投影与业务规则。工程规则见 [AGENTS.md](../AGENTS.md)，门店操作规范见 [sop.md](sop.md)。
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
| 11 | 操作人 | 员工在已注册的平板上用 PIN 登录；平板安装时注册一次；会话按班次有效 | 主数据、待确认问题 |
| 12 | 总部 | 本期不开发。主数据由总部维护统一的 JSON 主数据包，`code` 和 UUID 全部门店统一 | 主数据 |
| 13 | 不存在的业务 | 调拨、套餐、半批生产本期都不存在 | 未来功能 |

## 时间

| 字段 | 含义 | 来源 | 用途 |
|---|---|---|---|
| `seq` | 提交顺序 | SQLite 分配 | 重放、同步；FIFO 先盘盈，再按 `seq`，同一事件内按原行序。不按时间戳 |
| `recorded_at` | 门店接收时间 | 系统时钟原值，不做钳制 | 审计、展示 |
| `occurred_at` | 业务发生时间 | 相对校准，或补录时由员工填写 | 计算 `business_date`；吸收判定 |
| `business_date` | 营业日 | 由 `occurred_at` 计算；`SALES_IMPORTED` 例外，由命令显式给出 | 报表归属 |

**相对校准**（实时录入的默认方式）：客户端录入时记 `captured_at`，发送时带 `sent_at`，两者都是平板本地时钟。服务端计算

```
lag         = sent_at − captured_at
occurred_at = recorded_at − lag
```

两个时间来自同一块时钟，相减后绝对偏差抵消。
这只校准时钟偏差，不证明真实录入时点：两个时间都由客户端提交，系统不保证它们真实。系统保证留痕（见下方「展示与导出」），真实性靠 [SOP](sop.md) 与店长复核；盘点吸收不需要店长确认。

- `lag < 0`：按 0 处理，返回警告 `CAPTURE_TIME_ADJUSTED`。
- `lag > 72h`：`400 CAPTURE_TOO_OLD`，改走补录入口。
- 生产命令可带 `started_captured_at`，用同一个 `sent_at`、`recorded_at` 按上述规则换算出 `started_at` 写进 payload。
- 带开始时间时，先校验 `started_captured_at <= captured_at`，再计算两段 lag；开始时间也适用 72 小时上限。开始晚于完成时返回 `400 INVALID_PRODUCTION_TIME`，不得用 lag 的负值归零掩盖错误顺序。

**补录**（显式入口，Q6 确认后实现）：

- 收货、生产、报损、订货、温度记录的命令可以显式提交 `occurred_at`（生产同时提交 `started_at`），不走相对校准。
- `occurred_at` 晚于 `recorded_at`：`400 INVALID_OCCURRED_AT`。超出锁账范围：`409 BOOKS_LOCKED`（见 Q6）。
- 盘点不能补录。
- 界面交互：所选那天该物料有盘点时，提示「X 点盘过该物料，这次发生在盘点前还是盘点后？」，按选择把 `occurred_at` 设在那次盘点时间的对应一侧。

**通用规则**：

- 一个命令产生的所有事件共用同一组 `recorded_at`、`occurred_at`、`business_date`。
- 冲销和数量更正的 `occurred_at`、`business_date` 取原事件的值，由写入线程填写，客户端不能指定。
- 首次提交时算出的 `occurred_at` 随事件保存；重试时返回原响应，不重新计算。
- 生产带 `started_at` 时必须满足 `started_at <= occurred_at`（相等允许），相对校准和显式补录都适用。不满足时返回 `400 INVALID_PRODUCTION_TIME`，不写事件或 `processed_commands`；此校验与其他业务校验一样在幂等检查之后执行。
- **营业日**：配置项 `timezone = "Asia/Shanghai"`、`business_day_cutoff = "04:00"`。`occurred_at` 换算到门店当地时间，早于日切的算前一营业日。由 `jiff` 纯函数计算。修改日切配置不重算历史事件。
- **展示与导出**：记录的展示和导出同时给出操作人、设备、`recorded_at` 和 `occurred_at`。不设阈值、不打标签，由复核的人比对两者判断。被纠错的记录与它的更正、冲销按聚合 version 顺序一起展示。
- **时钟异常**：`recorded_at` 不保证递增，这是预期行为。`/health` 暴露 `clock_regression_ms = max(0, max(recorded_at) − now)`，超过 5 分钟时状态为 `degraded`，**不拒绝写入**。

## 主数据

| 实体（聚合类型） | 含义 | `snapshot` 字段 |
|---|---|---|
| `ITEM` | 物料，**含单位换算** | `code`, `name`, `base_unit`（`g` / `ml` / `pcs`）, `category`（`RAW` / `SEMI` / `FINISHED`）, `default_shelf_life_ms?`, `units[{unit_code, base_qty_per_unit}]`, `active` |
| `RECIPE` | 配方，**含全部版本**；版本只增不改 | `code`, `name`, `output_item_id`, `versions[{version, output_qty_per_batch, lines[{item_id, qty_per_batch}]}]`, `active` |
| `SUPPLIER` | 供应商 | `code`, `name`, `contact_phone?`, `active` |
| `EMPLOYEE` | 员工，**不含凭据** | `code`, `name`, `role`（`STAFF` / `MANAGER`）, `active` |
| `WASTE_REASON` | 报损原因；初始化时预置 `EXPIRED`、`DAMAGED`、`PRODUCTION_DEFECT`、`TASTING`、`OTHER` | `code`, `name`, `active` |
| `EQUIPMENT` | 设备 | `code`, `name`, `equipment_type`（`FRIDGE` / `FREEZER` / `BLAST_FREEZER` / `OVEN` / `PROOFER` / `MIXER` / `OTHER`）, `active` |

- 快照是该行变更后的完整内容，不是差量。行的主键是事件的 `aggregate_id`，`revision` 是 `aggregate_version`，都不重复写进快照。`active` 是布尔值。
- `name` 非空；`code` 非空，在同一实体内唯一。`contact_phone` 是原样保存的文本，不做格式校验。`MANAGER` 是店长，`STAFF` 是普通员工。
- `units` 不含基本单位：`unit_code` 等于 `base_unit` 时系数恒为 1。`units` 中的 `unit_code` 在同一物料内唯一、不等于 `base_unit`，按 `unit_code` 升序；系数规则见「单位」。
- `versions` 按 `version` 升序，从 1 连续编号；`output_qty_per_batch`、`qty_per_batch` 是正整数基本单位；同一版本的 `lines` 中 `item_id` 不重复，顺序为录入顺序。
- 快照字段与枚举随 `MASTER_DATA_CHANGED@1` 冻结，改动按 AGENTS.md「只追加」升 `schema_version`。

- 主键一律 UUIDv7。`code` 人可读、各门店统一（物料的 `code` 用于销售导入）。
- `code` 和 `ITEM` 的 `base_unit` 创建后不可修改。
  理由：账本数量按基本单位存。
- 每行带 `revision`，每次变更 +1。主数据只停用（`active = 0`），不删除。
- 命令引用已停用的主数据照常受理，停用只在界面上隐藏。
- 主数据写接口只允许 `MANAGER`。
- 每次变更写一条 `MASTER_DATA_CHANGED`：聚合类型是实体名，聚合 ID 是行的主键，`aggregate_version` 是变更后的 `revision`；payload 是 `{entity, source, snapshot}`，`source`（`LOCAL` / `HQ_PACKAGE`）。主数据表是投影，可以从事件流重建。
- 内容没有变化的变更不写事件（导入主数据包时逐行比对快照）。
- **认证状态不进事件流**：`employee_credentials`（PIN 的 Argon2id 哈希）、设备注册、设备解锁码哈希、设备锁定状态与失败计数、会话都是普通状态表，不同步，不参与重建。
- **系统操作人**：初始化和主数据包导入在没有登录员工时，`actor_id` 使用保留 ID `00000000-0000-7000-8000-000000000000`。
- **系统设备**：初始化和没有登录平板的主数据包导入，`device_id` 使用保留 ID `00000000-0000-7000-8000-000000000001`，表示产生事件的本门店节点。该身份不需要设备注册、设备令牌、解锁码或员工会话，不写入设备注册表；事件的 `device_id` 仍为非空 UUIDv7。
- 两个保留 ID 不得分配给真实员工或注册平板。系统身份只由节点内部的初始化和主数据包导入入口填写，表示事件来源，不授予 HTTP 访问权限。
- **初始化**：`boh-server init` 写入 `store_meta`，再创建第一个店长账号（在终端输入两次 PIN）和预置主数据；产生的每条 `MASTER_DATA_CHANGED` 都使用上述系统 `actor_id` 和系统 `device_id`，`source = LOCAL`。没有登录平板的总部包导入同样使用这两个系统 ID，`source = HQ_PACKAGE`。启动时配置里的 `store_id` 和 `store_meta` 不一致，拒绝启动。

### 员工认证

- **设备注册**（平板安装时做一次）：在节点上运行 `boh-server enroll-code` 生成一次性注册码。新平板提交注册码和设备名，服务端签发设备令牌，作为 `device_id` 的来源，同时生成这台设备的解锁码。
  - 注册不使用 PIN：否则局域网内任何人都能对店长 PIN 试错。
  - 注册码由 CSPRNG 生成，不少于 40 bit，一次有效，10 分钟过期；错误的注册码尝试整个节点每分钟最多 10 次。
  - 在节点上运行 `boh-server revoke-device <设备>` 吊销设备。
- **解锁码**：每台设备一个，注册成功时由 CSPRNG 生成，不少于 80 bit（例如 16 位 Base32），只在注册界面显示一次，由店长自行保存（例如存在手机里）。节点只保存 Argon2id 哈希。
  - 丢失或泄露时在节点上运行 `boh-server reset-unlock-code <设备>` 重新生成，旧码立即失效。
- **员工登录**：只接受带有效设备令牌的请求。PIN 由服务端用 Argon2id 校验，签发会话令牌（服务端状态表）。PIN 一律 6 位数字。会话按班次有效（结束判定见 Q7）。
- **PIN 设定与重置**：新员工没有 PIN，不能登录。
  - 店长在已注册平板上登录后，选中一名启用状态的员工发起「设置 / 重置 PIN」，再把平板交给该员工本人输入两次新 PIN。店长不输入、也看不到员工的 PIN。
  - 已登录员工可以修改自己的 PIN：输入旧 PIN 和两次新 PIN。旧 PIN 校验失败计入该设备的 PIN 失败次数（见「在线限速」）。
  - 节点命令 `boh-server reset-pin <员工 code>` 在终端输入两次新 PIN，用于没有可登录店长的情况。
  - 设定、重置、修改成功后，该员工在所有设备上的已有会话立即失效。每次操作记日志，含发起人、目标员工和 `device_id`；不产生事件。
- **业务事件身份**：HTTP 业务事件的 `actor_id` 来自已验证的员工会话，`device_id` 来自该会话所属的注册平板设备令牌。`Actor` 提取器不接受客户端指定的系统身份，缺少或无效的设备令牌 / 员工会话必须拒绝请求，不回退到系统身份。
- **员工状态**：登录和每个已认证请求都读取 `employees` 投影，员工必须是启用状态，否则与无效会话同样拒绝。权限按投影中的当前 `role` 判断，不使用登录时的角色。停用员工不改动认证状态表，已有会话在下一次请求时失效。
- **在线限速**（PIN 能安全使用的前提）：
  - 按设备计数：同一设备 1 小时内 PIN 校验失败 5 次（不论输入的是哪个员工），该设备的登录功能锁定，界面显示设备名并提示由店长解锁。其他员工在该设备上登录成功不清零计数。
    - 按时间窗口计数，不按「连续失败」：否则夹一次自己的成功登录就能无限试错。
  - 锁定不自动过期，只能输入该设备的解锁码解锁，或在节点上运行 `boh-server unlock-device <设备>`。解锁后计数清零。锁定不影响已有会话。
  - 不按员工锁定：否则任何人都能针对性地锁住某个员工（尤其是店长）。
  - 解锁码校验每台设备每 10 秒最多一次，失败不锁定，记 `warn` 日志。
    - 解锁码失败不锁定：解锁码无法猜中，再加锁定只会让人能把平板推到只能在节点上解锁的状态。
  - 整个节点每小时 PIN 失败超过 50 次时，所有 PIN 校验额外延迟 5 秒，并在 `/health` 中报告 `auth_failures_last_hour`。
  - 每次 PIN 失败记 `warn` 日志，含员工和 `device_id`。
- 客户端不缓存 PIN 哈希。
- **传输加密**：设备令牌、PIN、会话令牌只经 HTTPS 传输。局域网 HTTPS（证书方案见 Q9）是认证切片的前置条件；节点未配置 TLS 时不注册认证接口，release 构建启用认证但未配置 TLS 时**拒绝启动**。
- 认证落地之前，业务切片只依赖 `Actor` 提取器，开发桩同时提供员工 ID 和 `role`。开发桩只在 debug 构建中可用；release 构建配置了开发桩时**拒绝启动**。

## 单位

- 每个物料只有一个基本单位。账本、投影中的数量都是基本单位的 `i64`。换算系数必须是**正整数**基本单位（`> 0`）。
  - 主数据写入时校验（本地编辑和总部包导入都一样），系数不合格的变更整条拒绝，不进入事件流。
- 命令用 `input {qty, unit_code, base_qty_per_unit}` 提交，其中系数是客户端录入时看到的值。写入线程用当前换算核对：
  - `unit_code` 未配置：`400 UNKNOWN_UNIT`；
  - 系数和当前值不一致：`409 UNIT_CONVERSION_CHANGED`，不入账，由人工确认后重新提交。
- payload 同时写入换算后的 `qty` 和 `input` 快照。
- 盘点的 `counted_qty` 只用基本单位，不带 `input`，这是 `input` 规则的例外。

## 批次

全部物料都按批次追踪。账面数 = 批次余量之和 + 账外缺口。

- **批次 ID**：每条收货行、每次生产产出、每个盘盈由写入线程生成一个 `lot_id`（UUIDv7），写进 payload。供应商批号 `supplier_lot_no` 是可选属性，不作主键。
- **到期时间** `expires_at`（UTC 毫秒，可空）只用于过期提醒，不参与分配。只有日期的保质期由客户端换算成门店当地当日结束时刻再提交；未填写时可用 `default_shelf_life_ms` 推算，推算结果写进 payload。
- **FIFO 分配**：未指定 `lot_id` 的扣减，按（盘盈优先、`source_seq`、`source_line_no`）升序分配：`origin = 'COUNT_GAIN'` 的批次优先，同一优先级内先按来源事件的 `seq` 升序，再按原 payload 行序升序。同一收货事件允许同一物料有多行，各行分别建批次，按原 `lines` 数组顺序分配。补录的收货按入账顺序排队。
- **来源行序**：批次投影保存 `source_line_no`，从 0 开始；收货取原 `lines[i]` 的 `i`，盘盈取原 `new_lots[i]` 的 `i`，生产 `output` 固定取 0。被吸收的行不建批次，其他行保留原下标，不重新编号。行序直接从已保存的 payload 恢复，不新增 payload 字段；更正、冲销或盘点调整已有批次时不改变它的 `source_seq` 和 `source_line_no`。
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
| `STOCK_COUNT_SUBMITTED` | `STOCK_COUNT` | `purpose`（`CLOSING` / `AUDIT`）, `lines[{item_id, lot_id?, counted_qty}]` | 无 |
| `STOCK_ADJUSTED` | `STOCK_COUNT` | `lines[{item_id, lot_id?, book_qty, counted_qty, delta}]`, `new_lots[{lot_id, item_id, qty}]` | 批次和账外缺口按 delta 变化；新建盘盈批次；写 `inventory_counts` |
| `PURCHASE_ORDER_SUBMITTED` | `PURCHASE_ORDER` | `supplier_id`, `lines[{item_id, qty, input}]`, `deliver_on`（门店当地日期 `'YYYY-MM-DD'`） | 无 |
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

1. **盘点表与草稿**：客户端按库存明细生成盘点表，不调用专门的盘点接口，不产生事件。库存明细查询返回物料的全部批次、账外缺口及门店节点当前的 `business_date`。
   - 每个批次行显示收货或生产时间、到期日、供应商批号。盘点表不预填清点数量，有空格时客户端不允许提交。
   - 草稿按设备保存在平板的 `IndexedDB` 中，刷新、休眠后都能恢复。草稿保存清点数及物料 / 批次标识、营业日和重试所需的命令信息，不保存库存余量，不改变「客户端不维护库存镜像」的规则。
   - 提交成功后删除草稿；提交被拒时保留已填的数，刷新盘点表后只补新出现的批次行。
   - 首次提交时生成的 `command_id` 存进草稿。结果未知时草稿锁定为只读，用原 ID、原内容重试，直到拿到结果。
   - **【默认】草稿跨营业日不能提交**：草稿保存打开盘点表时门店节点返回的当前 `business_date`；首次提交或被拒后重新提交前，用库存明细查询返回的当前 `business_date` 核对，跨营业日时清空草稿、重新清点。客户端不自己计算时区和日切。结果未知的命令先按原 ID、原内容取回结果，再处理草稿的营业日。
2. **提交盘点**：命令携带 `purpose`（`CLOSING` / `AUDIT`）和逐批次的清点结果，`captured_at` 取员工点「完成盘点」的时刻，同一事务内写入 `STOCK_COUNT_SUBMITTED` 和 `STOCK_ADJUSTED`。不加审批，提交即调整，录错就重盘。
   - **【默认】允许按区域或物料组分几次提交**，界面默认这样组织；同一营业日分次提交的 `CLOSING` 盘点各自入账。本期不加存放位置字段。
3. **范围与校验**：只针对 `lines` 里出现的物料。对其中每个物料，范围是该物料的全部批次（含余量为 0 的）加上账外缺口，范围与余量一律以门店节点提交时的值为准。
   - 填入的数量一律视为实有数量，没填不等于 0。被盘物料中余量大于 0 的每个批次都必须出现在 `lines` 中，实物没有就填 0；缺少时返回 `400 COUNT_LINE_MISSING`，列出缺少的批次，整条命令不入账。打开盘点表之后新建的批次同样校验，客户端刷新盘点表后补填。
   - 余量为 0 的批次可以不列；不带 `lot_id` 的行可以不列，不列表示没有对不上批次的货。
   - `lines` 中的 `lot_id` 不属于该物料或不在范围内：`400 LOT_NOT_IN_COUNT_SCOPE`；同一批次出现两次：`400 DUPLICATE_COUNT_LINE`。
   - **【默认】同一物料出现两行不带 `lot_id`**：`400 DUPLICATE_COUNT_LINE`。
4. **逐批次调整**：
   - 对每个列出的批次：`delta = counted_qty − remaining_qty`，`remaining_qty` 取提交时的余量；未列出的零余量批次不调整。
   - 账外缺口清零。
   - 不带 `lot_id` 且 `counted_qty > 0`（找到了对不上批次的货）：新建一个 `COUNT_GAIN` 批次，写进 `new_lots`。每个物料最多一行不带 `lot_id`。
   - `book_qty`、`delta` 和新批次都写进 payload，重放时不重新计算。`STOCK_ADJUSTED.lines` 包含范围内余量非 0 或被清点的每个批次，另外每个物料恰有一行不带 `lot_id`：`book_qty` 是账外缺口，`counted_qty` 是无批次的清点数（没有时为 0）。投影把这一行拆成账外缺口的 `−book_qty`（归零）和 `new_lots` 中的新批次。
5. **提交响应**：响应 `data` 返回服务端计算的 `lines[{item_id, lot_id?, book_qty, counted_qty, delta}]` 和 `new_lots[{lot_id, item_id, qty}]`，与第 4 条的调整 payload 一致。界面以响应为准，不显示客户端自己计算的差异。同一 `command_id`、同一规范化请求重发时，原样返回首次的响应（含 warnings），不按当前库存重新计算，`seq` 不增加。
6. **盘点投影**：被盘的每个物料写一行 `inventory_counts`（零差异也写）：`book_qty` 是提交时范围内批次余量与账外缺口之和，`counted_qty` 是清点合计。分次提交分别写入。
7. **操作规范**：盘点只数实物，清点到提交期间被盘物料不得变动，见 [SOP](sop.md)「盘点」。
   系统不检测盘点期间的变动，违反规范时差异会算错，由店长重盘。

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
- **退回会使账外缺口大于 0**（原分配含账外缺口，而该缺口已被一次入账在后、观察时点却早于原行的盘点清零，只在违反盘点操作规范时出现）：`409 COUNT_REQUIRED`。先盘点该物料，新盘点的观察时点晚于原行，纠错随后会被吸收。

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

表结构随库存切片进入 003，下面的 SQL 是设计意图，**不冻结**：

```sql
CREATE TABLE inventory_lots (
    lot_id          TEXT PRIMARY KEY,
    item_id         TEXT NOT NULL,
    origin          TEXT NOT NULL CHECK (origin IN ('RECEIPT', 'PRODUCTION', 'COUNT_GAIN')),
    source_seq      INTEGER NOT NULL REFERENCES store_events(seq),  -- FIFO 的事件顺序
    source_line_no  INTEGER NOT NULL CHECK (source_line_no >= 0),    -- 原 payload 下标；output 为 0
    remaining_qty   INTEGER NOT NULL CHECK (remaining_qty >= 0),
    expires_at      INTEGER,
    supplier_lot_no TEXT,
    UNIQUE (source_seq, source_line_no)
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
- **盘点期间的实物变动**：系统不检测，靠操作规范禁止，违反时由重盘纠正。
- **时钟**：系统时钟跳到未来再拨回时，期间事件的 `business_date` 是错的，不能自动修正；`/health` 会暴露这种情况。
- **发生时间由员工声明**：`captured_at`、`sent_at`、生产的 `started_captured_at` 和补录的 `occurred_at` 都由客户端提交，已认证员工可以伪造；HTTPS、设备令牌和 PIN 不能证明真实录入时间。伪造会改变记录归属的营业日、盘点窗口和吸收判定。温度记录的时间就是记录内容，伪造即记录造假。系统只保证留痕（见「时间」展示与导出），防伪造靠 [SOP](sop.md) 与店长复核。实时录入不受 Q6 锁账日约束。
- **门店节点长时间宕机**时没有离线写入能力，改用纸质登记，恢复后走补录入口，见 [SOP](sop.md)。
- **客户端暂存**：普通命令只承诺页面不刷新期间，命令进入 `IndexedDB` 队列，界面显示「待提交 N 条」。盘点草稿是例外，可跨刷新、休眠恢复，规则见「盘点」第 1 条。客户端不维护库存镜像，不做批次分配。排队命令补交被拒时，界面列出命令和原因，由人工处理。

## 验收用例

写成锁定测试之前，预期数值由人工逐条确认（见 AGENTS.md「测试分工」）。初始状态：账面 = 实物。

| 用例 | 过程 | 预期 |
|---|---|---|
| 生产期间盘点 | 面粉 100；09:10 开始生产，取 20；09:20 盘点得 80（调整 −20）；09:30 完工，用料 20，`started_at = 09:10` | 用料被吸收；账面 **80**；返回 `ABSORBED_BY_COUNT` |
| 生产没带开始时间 | 同上，没有 `started_at` | 不被吸收，账面 **60** |
| 生产开始晚于完成 | 相对校准命令的 `started_captured_at = 09:30`、`captured_at = 09:10`、`sent_at = 09:40`；或补录的 `started_at > occurred_at` | `400 INVALID_PRODUCTION_TIME`；库存、事件和 `processed_commands` 不变 |
| 负 lag 不掩盖生产顺序 | `sent_at = 09:00`、`captured_at = 09:10`、`started_captured_at = 09:30` | 归零前拒绝：`400 INVALID_PRODUCTION_TIME`；不入账 |
| 生产开始等于完成 | `started_captured_at = captured_at`，其余内容合法 | 时间顺序合法，照常处理 |
| 迟到报损 | 100；09:00 报损 10 留在平板队列；09:20 盘点得 90（调整 −10）；09:40 报损送达 | 被吸收；账面 **90**；未解释损耗 **0** |
| 补录报损 | 100；15:00 盘点得 50（调整 −50）；16:00 补录报损 50，`occurred_at = 10:00` | 被吸收；账面 **50**；报损 **50**；未解释损耗 **0** |
| 盘点后冲销 | 100；09:00 误报损 10，账面 90；09:20 盘点得 100（调整 +10）；10:00 冲销报损 | 被吸收；账面 **100**；报损合计 **0**；未解释损耗 **0** |
| 闭店重盘 | 成品账面 100；先盘为 80，再盘为 90；销售 0 | 调整净和 **−10**；未解释损耗 **10** |
| 指定批次不足 | 批次 A 5、B 10；指定 A 扣 8 | 分配 `A 5 SPECIFIED` + `B 3 FIFO` |
| 同次收货多个批次 | 同一收货事件的同一物料两行：`lines[0]` 建 A 5，`lines[1]` 建 B 10；不指定批次扣 8；B 的 UUID 字典序小于 A | 按行序分配 `A 5 FIFO` + `B 3 FIFO`；余量 A **0**、B **7**，不按 UUID 排序 |
| 同次收货重复提交 | 上例扣减后，用原 `command_id`、原内容重发收货 | 返回原批次 A、B；`seq` 不增加；余量仍为 A **0**、B **7**，来源行序不变 |
| 来源行序重建 | 保存同次收货后尚未扣减的原库结果，清空投影，按 `seq` 重建，再不指定批次扣 8 | 重建后投影逐行一致，A、B 的 `source_line_no` 分别为 **0**、**1**；后续分配 `A 5 FIFO` + `B 3 FIFO`，余量 A **0**、B **7** |
| 按批次盘点 | 批次 A 5、B 10，账外缺口 −2；清点 A 4、B 10、无批次 3 | A 调整 −1；账外缺口 +2 归零；新建 `COUNT_GAIN` 批次 3；账面 **17** |
| 盘点漏填批次 | 批次 A 5、B 10；只提交 A 4 | `400 COUNT_LINE_MISSING`，列出 B；不入账 |
| 盘点填 0 | 批次 A 5、B 10；提交 A 4、B 0 | A 调整 −1，B 调整 −10；账面 **4** |
| 盘点后新到批次 | 批次 A 5；打开盘点表后收货建批次 C 10；提交 A 5 | `400 COUNT_LINE_MISSING`，列出 C；不入账 |
| 余量以提交时为准 | 打开盘点表时 A 5；提交前另一台平板报损 A 2 已入账；提交 A 4 | `book_qty` **3**，`delta` **+1**；账面 **4**；响应返回这些值 |
| 盘点重复提交 | 盘点成功后又报损该物料；同一 `command_id` 重发盘点 | 原样返回首次的响应；`seq` 不增加 |
| 分次闭店盘点 | 面粉账面 100、黄油账面 50；21:00 `CLOSING` 只盘面粉得 90；21:30 `CLOSING` 只盘黄油得 48 | 面粉调整 −10、黄油调整 −2；各写一行 `inventory_counts` |
| 无批次行重复 | 同一物料提交两行不带 `lot_id` | `400 DUPLICATE_COUNT_LINE` |
| 换算变化 | 离线录入「2 袋，每袋 25000 g」；提交前换算改为 20000 | `409 UNIT_CONVERSION_CHANGED`，不入账 |
| 销售示例 | 生产 100、报损 8、闭店实数 5（调整 −87）、销售 84 | 未解释损耗 **3** |
| 幂等 | 同一命令发送 2 次，`sent_at` 不同 | 第二次原样返回首次响应（含 warnings），`seq` 不增加 |
| 盘点后的正常业务 | 09:20 盘点；10:30 报损并提交 | 不被吸收，正常扣减 |
| 冲销时批次已消耗 | 收货批次 L 10，已用掉 3，之后没有盘点；冲销这次收货 | L 扣到 **0**；账外缺口 **−3**；返回 `STOCK_SHORTFALL` |
| 数量更正 | 收货录成 1000（实际 100），批次 L1；生产从 L1 扣 300；之后没有盘点；更正为 100 | L1 **0**；账外缺口 **−200**；返回 `STOCK_SHORTFALL` |
| 更正多次后冲销 | 报损 10 → 更正为 8 → 更正为 6 → 冲销 | 冲销退回 **6**；报损合计 **0** |
| 重复冲销 | 同一事件冲销两次 | 第二次 `409 ALREADY_REVERSED` |
| 冲销后更正 | 冲销后再更正同一事件 | `409 ALREADY_REVERSED` |
| 员工停用 | 员工已登录；之后该员工被停用 | 已有会话的请求与无效会话同样被拒绝，不入账；该员工 PIN 登录被拒绝 |
| 店长降级 | 店长已登录；之后 `role` 改为 `STAFF` | 已有会话立即只有 `STAFF` 权限 |
| 新员工无 PIN | 新建员工后未设定 PIN 即登录 | 登录被拒绝 |
| 重置 PIN 吊销会话 | 员工在两台设备上各有会话；店长为其重置 PIN | 两个会话的请求都被拒绝；新 PIN 可以登录，旧 PIN 不能 |
| 普通员工不能重置 | `STAFF` 会话为其他员工发起重置 PIN | 被拒绝，PIN 不变 |
| 自行修改 PIN 旧码错误 | 已登录员工修改 PIN 时旧 PIN 输错 | 被拒绝，PIN 不变；计入该设备的 PIN 失败次数 |

## 未来功能（本期不实现）

- **门店调拨**：`TRANSFER_SENT` / `TRANSFER_RECEIVED`，含在途归属规则。
- **总部同步**：上行契约见 AGENTS.md「上行同步」；下行为主数据包导入（`source = HQ_PACKAGE`）。总部从事件自行计算报表，必须区分被吸收的行和正常生效的行。总部关账后可下发锁账日（见 Q6）。
- **采购对接**：采购单与收货的关联、在途库存。
- **批次标签与短码**：收货时给每个批次贴标签，标签印人能读的批次短码；短码及标签生成功能延后。

## 待确认问题

- **Q6 锁账日**：默认方案是店长在管理界面设置锁账日，`business_date` 不晚于它的补录和纠错返回 `409 BOOKS_LOCKED`；另外默认最多补录 7 天以内。**阻塞补录入口**。
- **Q7 会话的「班次」如何结束**：默认方案是员工主动登出，或登录满 12 小时。**阻塞认证切片**。会话结束时平板队列中未提交命令的处理一并确认，因为 `actor_id` 取提交时的会话。
- **Q9 局域网证书**：默认方案是总部私有 CA 为每个门店节点签发证书，平板初始化时安装一次根证书；备选是公网域名 + ACME DNS 验证（平板无需配置，但续期依赖联网）。**阻塞认证切片**。
- **Q11 店长复核的频率与形式**：复核范围见 [SOP](sop.md)「复核」；频率，以及用纸质签字还是在软件中留复核记录，待定。软件中留记录需新增事件类型。
