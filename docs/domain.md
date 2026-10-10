# BOH 领域模型

本文件定义业务决策、事件目录、投影与业务规则。工程规则见 [AGENTS.md](../AGENTS.md)，门店操作规范见 [sop.md](sop.md)，规范用词见 [glossary.md](glossary.md)。
实现落地后，字段与表结构以代码为准（`boh-domain` 结构体、迁移 SQL），本文件只保留语义与不变量。
「待确认问题」中的条目，确认前不要实现依赖它的部分。

## 已确认的业务决策

| # | 主题 | 结论 | 详见 |
|---|---|---|---|
| 1 | 销售 | 每晚导入 POS 导出的 CSV / Excel，只进报表，不扣库存 | 销售导入 |
| 2 | 批次 | 全部物料按批次追踪；批次号 `lot_id` 人可读、印在标签上；未指定批次的扣减按批次号 FIFO 分配 | 批次 |
| 3 | 单位 | 多单位，账本只存基本单位；离线期间换算系数变了，命令返回 409 由人工确认 | 单位 |
| 4 | 生产扣料 | 按配方自动扣，员工可改实际用量；生产界面记录开始时间 | 生产规则 |
| 5 | 盘点粒度 | 按物料下的各批次逐行清点数量，只调整已有批次，不新建批次；库存明细界面同样按批次逐行显示 | 盘点 |
| 6 | 盘点后的影响 | 实物发生在盘点之前、入账在盘点之后的扣减（及其纠错），被那次盘点吸收：不动库存，只更正报表。新建批次的收货、生产产出不吸收 | 盘点吸收 |
| 7 | 补录 | 允许通过补录入口填写过去的发生时间 | 时间 |
| 8 | 录错 | 只有数量错用数量更正（保持原批次，差额可使原批次变负）；其他错误整条冲销 | 纠错 |
| 9 | 账面不足 | 报损先提示，员工确认实际数量后入账：指定批次的差额留在该批次（余量可为负），未指定批次的差额记入账外缺口；下次盘点处理。纠错时批次已被消耗的处理见「纠错」 | 批次、纠错 |
| 10 | 营业日 | 时区 `Asia/Shanghai`，日切 `04:00` | 时间 |
| 11 | 操作人 | 员工在已注册的平板上用 PIN 登录；平板安装时注册一次；会话按班次有效 | 主数据、待确认问题 |
| 12 | 总部 | 本期不开发。主数据由总部维护统一的 JSON 主数据包，`code` 和 UUID 全部门店统一 | 主数据 |
| 13 | 不存在的业务 | 调拨、套餐、半批生产本期都不存在 | 未来功能 |

## 时间

| 字段 | 含义 | 来源 | 用途 |
|---|---|---|---|
| `seq` | 提交顺序 | SQLite 分配 | 重放、同步。不用于 FIFO（见「批次」） |
| `recorded_at` | 门店接收时间 | 系统时钟原值，不做钳制 | 审计、展示 |
| `occurred_at` | 业务发生时间 | 相对校准，或补录时由员工填写 | 计算 `business_date` 和批次日期；吸收判定 |
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
- 换算溢出（时间戳超出可表示范围）：`400 VALIDATION_FAILED`。
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
- 创建收货、生产、报损等单据时，界面显示将要记录的发生日期和时间，供操作人员核验（见 [SOP](sop.md)「记录」）。
- 生产带 `started_at` 时必须满足 `started_at <= occurred_at`（相等允许），相对校准和显式补录都适用。不满足时返回 `400 INVALID_PRODUCTION_TIME`，不写事件或 `processed_commands`；此校验与其他业务校验一样在幂等检查之后执行。
- **营业日**：配置项 `timezone = "Asia/Shanghai"`（IANA 时区名）、`business_day_cutoff = "04:00"`（`'HH:MM'`），都必填，缺失或非法时拒绝启动。`occurred_at` 换算到门店当地时间，早于日切的算前一营业日。由 `jiff` 纯函数计算。修改日切配置不重算历史事件。
- **展示与导出**：记录的展示和导出同时给出操作人、设备、`recorded_at` 和 `occurred_at`。不设阈值、不打标签，由复核的人比对两者判断。被纠错的记录与它的更正、冲销按聚合 version 顺序一起展示。
- **时钟异常**：`recorded_at` 不保证递增，这是预期行为。`/health` 暴露 `clock_regression_ms = max(0, max(store_events.recorded_at) − now)`，超过 5 分钟时状态为 `degraded`，**不拒绝写入**。
  - 它表示「账本中最晚的一条记录比当前时钟超前多少」，不是「最近一次回拨了多少」。主要发现当前时钟落后（如断网的门店机断电后主板时钟复位，拿不到 NTP）；只要再写入记录，「最新一条事件的 `recorded_at`」就会跟着落后，发现不了这种情况。
  - 时钟曾跳到未来、写入记录后又被拨回时，账本只追加，那条记录一直在：指标持续 `degraded`，直到真实时间追上它。两种情况从账本上分不出来，需要结合当前时间判断。

## 主数据

| 实体（聚合类型） | 含义 | `snapshot` |
|---|---|---|
| `ITEM` | 物料，**含单位换算** | `boh_domain::master_data::ItemSnapshot` |
| `RECIPE` | 配方，**含全部版本**；版本只增不改 | `boh_domain::master_data::RecipeSnapshot` |
| `SUPPLIER` | 供应商 | `boh_domain::master_data::SupplierSnapshot` |
| `EMPLOYEE` | 员工，**不含凭据** | `code`, `name`, `role`（`STAFF` / `MANAGER`）, `active` |
| `WASTE_REASON` | 报损原因；初始化时预置 `EXPIRED`、`DAMAGED`、`PRODUCTION_DEFECT`、`TASTING`、`OTHER` | `boh_domain::master_data::WasteReasonSnapshot` |
| `EQUIPMENT` | 设备 | `boh_domain::equipment::EquipmentSnapshot` |

- 已落地实体的样本在 `crates/boh-app/tests/golden/MASTER_DATA_CHANGED@1/`。

- 快照是该行变更后的完整内容，不是差量。行的主键是事件的 `aggregate_id`，`revision` 是 `aggregate_version`，都不重复写进快照。`active` 是布尔值。
- `name` 非空；`code` 非空，在同一实体内唯一。`contact_phone` 不做格式校验。`MANAGER` 是店长，`STAFF` 是普通员工。
- `ITEM` 的 `code` 只能由 `A`–`Z`、`0`–`9` 和 `_` 组成（本地编辑和总部包导入都一样）。
  理由：它是批次号的一段，要印在标签上并由人录入。
- `units` 不含基本单位：`unit_code` 等于 `base_unit` 时系数恒为 1。`units` 中的 `unit_code` 在同一物料内唯一、不等于 `base_unit`，按 `unit_code` 升序；系数规则见「单位」。
- `versions` 按 `version` 升序，从 1 连续编号；`output_qty_per_batch`、`qty_per_batch` 是正整数基本单位；同一版本的 `lines` 中 `item_id` 不重复，顺序为录入顺序。
- 快照字段与枚举随 `MASTER_DATA_CHANGED@1` 冻结，改动按 AGENTS.md「只追加」升 `schema_version`。

- 主键一律 UUIDv7。`code` 人可读、各门店统一（物料的 `code` 用于销售导入）。
- `code`、`ITEM` 的 `base_unit` 和 `RECIPE` 的 `output_item_id` 创建后不可修改；配方已有的版本不可修改，只能追加新版本。
  理由：账本数量按基本单位存；配方版本中的数量按产出物料和用料的基本单位解释，历史版本的产出物料也不能变。
- 每行带 `revision`，每次变更 +1。主数据只停用（`active = 0`），不删除。
- HTTP 修改命令携带客户端看到的 `base_revision`，与当前 `revision` 不等时返回 `409 REVISION_CONFLICT`（`details` 为 `{"current_revision": n}`），不写事件或 `processed_commands`。新建命令不带 `base_revision`；主数据包导入按快照逐行比对，不适用此规则。
- 命令引用已停用的主数据照常受理，停用只在界面上隐藏。
- 主数据写接口只允许 `MANAGER`。
- HTTP `EMPLOYEE` 命令新建 `MANAGER`、将 `STAFF` 改为 `MANAGER`，或将停用员工启用且新 `role = MANAGER` 时，首次执行须校验已验证会话所属店长的当前 PIN；条件在同一写事务内按变更前投影和新快照判定，不因 `source = HQ_PACKAGE` 豁免。
- 上述 PIN 仅为命令的敏感字段，不进入 `EMPLOYEE` 快照或事件 payload，适用 AGENTS.md「幂等」的敏感命令规则；错误计入设备 PIN 失败次数，设备锁定时拒绝校验，拒绝时不改员工、不写事件或 `processed_commands`。
- `EMPLOYEE` 变更后必须至少保留一名已设定 PIN 且启用的 `MANAGER`，否则返回 `409 LAST_ACTIVE_MANAGER`，不写事件或 `processed_commands`。
- 每次变更写一条 `MASTER_DATA_CHANGED`：聚合类型是实体名，聚合 ID 是行的主键，`aggregate_version` 是变更后的 `revision`；payload 是 `{entity, source, snapshot}`，`source`（`LOCAL` / `HQ_PACKAGE`）。主数据表是投影，可以从事件流重建。
- 内容没有变化的变更不写事件（导入主数据包时逐行比对快照）。
- **认证状态不进事件流**：`employee_credentials`（PIN 的 Argon2id 哈希）、设备注册、设备解锁码哈希、设备锁定状态与失败计数、会话、PIN 重置授权都是普通状态表，不同步，不参与重建。
- **系统操作人**：初始化和主数据包导入在没有登录员工时，`actor_id` 使用保留 ID `00000000-0000-7000-8000-000000000000`。
- **系统设备**：初始化和没有登录平板的主数据包导入，`device_id` 使用保留 ID `00000000-0000-7000-8000-000000000001`，表示产生事件的本门店节点。该身份不需要设备注册、设备令牌、解锁码或员工会话，不写入设备注册表；事件的 `device_id` 仍为非空 UUIDv7。
- 两个保留 ID 不得分配给真实员工或注册平板。系统身份只由节点内部的初始化和主数据包导入入口填写，表示事件来源，不授予 HTTP 访问权限。
- **初始化**：`boh-server init` 在一个写事务中写入 `store_meta`、预置主数据和第一个店长账号（在终端输入两次 PIN），任一步失败则全部回滚，可以重新执行。第一个店长随认证切片加入（见 AGENTS.md「路线图」）。
  - 初始化是一个命令：`command_type` 为 `store.init`，`command_id` 由服务端按 AGENTS.md「ID 与时间」生成；`processed_commands` 中的规范化请求为 `{"store_id": <配置的 store_id>}`，响应为 `{}`。
  - `store_meta.created_at`、`processed_commands.recorded_at` 和每条事件的 `recorded_at` 都是同一个 `now()`；事件的 `occurred_at = recorded_at`，`business_date` 由它计算。
  - 产生的每条 `MASTER_DATA_CHANGED` 都使用上述系统 `actor_id` 和系统 `device_id`，`source = LOCAL`，`aggregate_version = 1`。
  - 预置报损原因按下表顺序写入，全部启用：

    | `code` | `name` |
    |---|---|
    | `EXPIRED` | 过期 |
    | `DAMAGED` | 损坏 |
    | `PRODUCTION_DEFECT` | 生产不良 |
    | `TASTING` | 试吃 |
    | `OTHER` | 其他 |

  - 没有登录平板的总部包导入同样使用这两个系统 ID，`source = HQ_PACKAGE`。启动时 `store_meta` 为空（未初始化）或与配置里的 `store_id` 不一致，拒绝启动。`store_meta` 已存在时 `init` 拒绝执行，不改动任何数据，以非 0 退出码结束。

### 主数据接口

| 实体 | 路径 | `command_type` 前缀 | 行的 ID 字段 | 单行键 / 列表键 |
|---|---|---|---|---|
| `EQUIPMENT` | `/api/v1/equipment` | `equipment` | `equipment_id` | `equipment` / `equipment` |
| `ITEM` | `/api/v1/items` | `item` | `item_id` | `item` / `items` |
| `RECIPE` | `/api/v1/recipes` | `recipe` | `recipe_id` | `recipe` / `recipes` |
| `SUPPLIER` | `/api/v1/suppliers` | `supplier` | `supplier_id` | `supplier` / `suppliers` |
| `WASTE_REASON` | `/api/v1/waste-reasons` | `waste_reason` | `waste_reason_id` | `waste_reason` / `waste_reasons` |

- 每个实体有三个接口：`POST <路径>` 新建（`<前缀>.create`）、`PUT <路径>/{<ID 字段>}` 修改（`<前缀>.update`）、`GET <路径>` 查询。配方另有追加版本的接口。写接口只允许 `MANAGER`，查询允许已认证员工。
- 新建请求体：`command_id` 加快照的全部字段（配方例外，见下文）。修改请求体：`command_id`、`base_revision` 加快照中可修改的字段；不可修改的字段不在请求体中，取当前值。各实体的字段见下文。
- 写命令成功的 `data` 是 `{<单行键>: 行}`，行为该命令执行后的实体：`<ID 字段>`、快照的全部字段、`revision`，可选字段缺省时省略该键。查询的 `data` 是 `{<列表键>: [行…]}`，含停用的，按 `code` 的字节序升序。
- 取值：`code`、`name` 非空，首尾不能有空白字符；`base_revision` 是正整数；其余字段见下文。不满足时 `400 VALIDATION_FAILED`。
- 不带 `captured_at` / `sent_at`：`occurred_at = recorded_at`，`business_date` 由它计算。
- **新建**：服务端生成 UUIDv7 作为 ID，写一条 `aggregate_version = 1` 的 `MASTER_DATA_CHANGED`。`code` 已被同一实体的其他行使用（含停用的）：`409 CODE_ALREADY_EXISTS`，`details` 为 `{"code", <ID 字段>}`（已占用该 `code` 的行）。
- **修改**：行不存在：`404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": <实体>, "id"}`。`base_revision` 规则见上。
  - 新快照与当前快照相同时不写事件，命令照常成功并写入 `processed_commands`，响应为当前行。
- **引用**：引用其他主数据的字段必须指向已有的行（含停用的），否则 `404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": <被引用的实体>, "id"}`。
- 同时命中多个业务错误时，返回哪一个不作规定。

**设备**：请求体与行见 `boh_domain::equipment` 的 `CreateEquipment`、`UpdateEquipment`、`Equipment`。

**物料**：请求体与行见 `boh_domain::master_data` 的 `CreateItem`、`UpdateItem`、`Item`。

- `default_shelf_life_ms` 可以省略，出现时是正整数，不接受 `null`。修改时省略表示没有默认保质期。
- `units` 是数组，可以为空，不接受 `null`。每项 `{unit_code, base_qty_per_unit}`：`unit_code` 非空、首尾不能有空白字符、不等于 `base_unit`；`base_qty_per_unit` 是正整数。
- `units` 必须按 `unit_code` 的字节序严格升序（因此不重复），服务端不重新排序。
- 修改换算系数、增删单位照常受理，不影响已入账的事件；离线录入的命令按「单位」核对。

**配方**：

| 方法与路径 | `command_type` | 请求体（`boh_domain::master_data`） |
|---|---|---|
| `POST /api/v1/recipes` | `recipe.create` | `CreateRecipe` |
| `PUT /api/v1/recipes/{recipe_id}` | `recipe.update` | `UpdateRecipe` |
| `POST /api/v1/recipes/{recipe_id}/versions` | `recipe.add_version` | `AddRecipeVersion` |

- 行见 `boh_domain::master_data::Recipe`。

- 新建时 `output_qty_per_batch` 和 `lines` 构成版本 1。
- **追加版本**：新版本号是当前最大版本 + 1，写一条 `MASTER_DATA_CHANGED`，`revision` + 1。权限、`base_revision`、配方不存在和成功的 `data` 同「修改」。
  - `output_qty_per_batch` 和 `lines`（含行顺序）都与最新版本相同时，不新增版本、不写事件，`revision` 不变；命令照常成功并写入 `processed_commands`，响应为当前配方。只与最新版本比较：从 A 改为 B 再改回 A，仍新增版本。
- `output_qty_per_batch`、`qty_per_batch` 是正整数。`lines` 非空，每项 `{item_id, qty_per_batch}`，同一版本内 `item_id` 不重复，按提交顺序保存。
- `output_item_id` 和每个 `lines[].item_id` 引用 `ITEM`（规则见「引用」）。不限制物料的 `category`，不检查产出物料是否出现在自己的 `lines` 中。

**供应商**：请求体与行见 `boh_domain::master_data` 的 `CreateSupplier`、`UpdateSupplier`、`Supplier`。

- `contact_phone` 可以省略，出现时非空、首尾不能有空白字符，不接受 `null`。修改时省略表示删除联系电话。

**报损原因**：请求体与行见 `boh_domain::master_data` 的 `CreateWasteReason`、`UpdateWasteReason`、`WasteReason`。预置的报损原因同样可以修改和停用。

### 温度记录接口

| 方法与路径 | `command_type` | 请求体 / 查询参数（`boh_domain::temperature`） | 权限 |
|---|---|---|---|
| `POST /api/v1/temperature-readings` | `temperature.log` | `LogTemperature` | 已认证员工 |
| `GET /api/v1/temperature-readings` | — | `TemperatureQuery` | 已认证员工 |

- 写命令成功的 `data` 是 `{"temperature_reading": 行}`，查询的 `data` 是 `{"temperature_readings": [行…]}`；行为 `boh_domain::temperature::TemperatureReading`。
- **取值**：不满足时 `400 VALIDATION_FAILED`。
  - `celsius_x10` 是 0.1 °C 的整数，`-500`～`5000`（-50.0～500.0 °C），两端可取。它只拦截单位或数量级录错，不是食安阈值，不产生警告。
  - `note` 可以省略；出现时非空、首尾不能有空白字符、最多 200 个字符（按 Unicode 字符计），不接受 `null`。首尾以外的空白和换行原样保存。
  - 请求体不接受 `occurred_at`（补录入口见「时间」）。
- **新建**：服务端生成 UUIDv7 作为读数 ID（聚合 ID），写一条 `aggregate_version = 1` 的 `TEMPERATURE_LOGGED`。
  - `occurred_at`、`business_date` 按「时间」的相对校准计算。相对校准和营业日计算属于业务校验，在幂等检查之后执行。
    - 理由：`recorded_at` 在写事务内才确定，`sent_at` 不在规范化请求中。重试即使带着会超限或溢出的新 `sent_at`，也返回首次响应，不重新校准。
  - `CAPTURE_TOO_OLD`、时间溢出的 `VALIDATION_FAILED` 以及警告 `CAPTURE_TIME_ADJUSTED` 的 `details` 都是 `{}`。
  - 设备不存在：`404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": "EQUIPMENT", "id"}`。停用的设备照常受理；不限制 `equipment_type`。
  - 同时命中多个业务错误时，返回哪一个不作规定。
- **查询**：`business_date`（`'YYYY-MM-DD'`，真实日期）必填，`equipment_id` 可选；不分页。按 `occurred_at` 升序，相同时按 `seq` 升序；含停用设备的读数。
  - 参数缺失、取值非法、未知或重复的参数：`400 VALIDATION_FAILED`。`equipment_id` 合法但不存在时返回空数组。
- 投影 `temperature_readings` 每条读数一行，字段以迁移 003 为准。冲销在投影和查询中的体现随纠错切片设计。

### 收货接口

| 方法与路径 | `command_type` | 权限 |
|---|---|---|
| `POST /api/v1/receipts` | `receipt.create` | 已认证员工 |

- 请求体与行见 `boh_domain::receiving` 的 `CreateReceipt`、`Receipt`；`input` 见「单位」。
- 写命令成功的 `data` 是 `{"receipt": 行}`；行的 `lines` 与 payload 的 `lines` 相同。
- **取值**：不满足时 `400 VALIDATION_FAILED`，`details` 为 `{}`。
  - `lines` 非空。同一物料可以有多行，各行分别建批次。
  - `input.qty`、`input.base_qty_per_unit` 是正整数；`input.unit_code` 非空、首尾不能有空白字符。`input.qty × input.base_qty_per_unit` 不超出 `i64`。
  - `line_cost_cents` 是该行总金额（分），`>= 0`。
  - `manufacturer_lot_no` 可以省略；出现时非空、首尾不能有空白字符、最多 64 个字符（按 Unicode 字符计），不接受 `null`。
  - `produced_on`（生产日期）、`expires_on`（到期日）必填，是包装标签上的门店当地日期，格式 `'YYYY-MM-DD'`（真实日期），`produced_on <= expires_on`，`expires_on` 不晚于 `9998-12-31`（`expires_at` 由次日当地 0 点换算，须在时间戳范围内）。标签只印保质期时长时，由客户端按日历推算 `expires_on`。
  - 请求体不接受 `occurred_at`（补录入口见「时间」）。
- **新建**：服务端生成 UUIDv7 作为收货 ID（聚合 ID），每行按「批次」生成一个批次号 `lot_id`，写一条 `aggregate_version = 1` 的 `GOODS_RECEIVED@2`。
  - `occurred_at`、`business_date` 按「时间」的相对校准计算，规则与温度记录相同（`CAPTURE_TOO_OLD`、时间溢出的 `VALIDATION_FAILED`、`CAPTURE_TIME_ADJUSTED` 的 `details` 都是 `{}`）。
  - payload 每行：`qty = input.qty × input.base_qty_per_unit`；`expires_at` 是 `expires_on` 在门店时区（配置 `timezone`）的当日最后一毫秒，即次日当地 0 点的 UTC 毫秒减 1。
    - 理由：`projections::apply` 不读配置，换算结果必须写进 payload。
  - 批次日期是 `occurred_at` 的门店当地日历日期，与下面 `INVALID_LOT_DATES` 的 D 相同。
- **业务校验**（幂等检查之后）：同时命中多个业务错误时，返回哪一个不作规定；都不写事件或 `processed_commands`。
  - 供应商、物料不存在：`404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": "SUPPLIER" / "ITEM", "id"}`。停用的照常受理。
  - `unit_code` 既不是物料的 `base_unit` 也不在 `units` 中：`400 UNKNOWN_UNIT`，`details` 为 `{"line", "item_id", "unit_code"}`。
  - 系数与当前值不同：`409 UNIT_CONVERSION_CHANGED`，`details` 为 `{"line", "item_id", "unit_code", "base_qty_per_unit": 当前值}`。`unit_code` 等于 `base_unit` 时当前值为 1。
  - 日期与收货时间矛盾：`400 INVALID_LOT_DATES`，`details` 为 `{"line", "item_id", "reason"}`。以 `occurred_at` 在门店时区的日历日期 D 判定（不是营业日）：`produced_on > D` 时 `reason = "PRODUCED_IN_FUTURE"`；`expires_on < D` 时 `reason = "EXPIRED_ON_RECEIPT"`。等于 D 都允许。
    - 用日历日期而不是营业日：日切前（如凌晨 2 点）收到当天生产的货是正常的。
  - 流水号用尽：`409 LOT_SERIAL_EXHAUSTED`（见「批次」）。
  - `line` 是行在 `lines` 中的下标。
- **警告**：`CAPTURE_TIME_ADJUSTED` 在前，之后按 `line` 升序的 `EXPIRES_BEFORE_OLDER_STOCK`（见「批次」）。
- **吸收**：收货行新建批次，不被盘点吸收（见「盘点吸收」）。
- **投影**：每行在 `inventory_lots` 建一个 `origin = 'RECEIPT'` 的批次（`lot_id` 及由它拆出的批次日期和流水号，`source_line_no` 是行下标，`remaining_qty = qty`），在 `inventory_movements` 写一条 `kind = 'RECEIPT'`、`alloc_source = 'NEW_LOT'` 的流水，`physical_at = occurred_at`。`line_cost_cents`、`produced_on` 只在事件中。
- **`GOODS_RECEIVED@1` 不支持**：`@1` 的 `lot_id` 是 UUID，payload 中没有生成批次号所需的类型和编码，不能 upcast，也不推测。
  - 账本中有 `@1` 收货时，迁移 007 报错并拒绝执行（节点不能启动）；重建投影遇到 `@1` 收货也报错。
  - 门店节点从空库 `init` 开始，不会有 `@1` 收货；只影响开发库，开发库需要重新初始化。

### 报损接口

| 方法与路径 | `command_type` | 权限 |
|---|---|---|
| `POST /api/v1/waste-records/precheck` | —（只读，不是写命令） | 已认证员工 |
| `POST /api/v1/waste-records` | `waste.log` | 已认证员工 |

报损先预检；账面不足的行由员工确认实际报损数量后，再正式提交。系统不自动确认，也不自动重提。

**正式提交**：

- 请求体：`command_id`、`lines`、`captured_at`、`sent_at`。`lines` 每项 `{item_id, lot_id?, input, reason_code, confirm_shortage?}`：`lot_id` 是员工指定的批次，`input` 见「单位」，`reason_code` 是 `WASTE_REASON` 的 `code`，`confirm_shortage` 见下方「不足确认」。
- 写命令成功的 `data` 是 `{"waste_record": 行}`；行为 `waste_record_id`、`lines`（与 payload 的 `lines` 相同）、`business_date`、`occurred_at`、`recorded_at`、`actor_id`、`device_id`。
- **取值**：不满足时 `400 VALIDATION_FAILED`，`details` 为 `{}`。
  - `lines` 非空。同一物料、同一批次都可以出现在多行（例如不同原因）。
  - `input` 的规则同收货：`input.qty`、`input.base_qty_per_unit` 是正整数；`input.unit_code` 非空、首尾不能有空白字符；`input.qty × input.base_qty_per_unit` 不超出 `i64`。
  - `reason_code` 非空、首尾不能有空白字符。
  - `lot_id` 可以省略；出现时符合批次号格式（同「批次查询」），不接受 `null`。
  - `confirm_shortage` 可以省略；出现时是布尔值，不接受 `null`。省略与 `false` 等价，两者的规范化请求相同；`true` 保留在规范化请求中，参与幂等比对。
  - 请求体不接受 `occurred_at`（补录入口见「时间」）。
- **新建**：服务端生成 UUIDv7 作为报损 ID（聚合 ID），写一条 `aggregate_version = 1` 的 `WASTE_LOGGED`。
  - `occurred_at`、`business_date` 按「时间」的相对校准计算，规则与温度记录相同（`CAPTURE_TOO_OLD`、时间溢出的 `VALIDATION_FAILED`、`CAPTURE_TIME_ADJUSTED` 的 `details` 都是 `{}`）。
  - payload 每行：`qty = input.qty × input.base_qty_per_unit`；`lot_id` 取请求中指定的批次，未指定时省略；`item_book_qty`、`lot_book_qty` 和 `alloc` 按下方「逐行处理」得出。
  - payload 行的键顺序：`item_id`、`lot_id`、`qty`、`input`、`reason_code`、`item_book_qty`、`lot_book_qty`、`alloc`（被吸收的行在 `alloc` 的位置写 `absorbed_by_event_id`）；`input` 的键顺序同收货；`alloc` 每项的键顺序 `lot_id`、`qty`、`source`。
  - `confirm_shortage` 不写进 payload。
    理由：账面值记录当时是否不足，确认标记记录员工的选择，两者含义不同；后者保存在规范化请求中。
- **业务校验**（幂等检查之后）：同时命中多个业务错误时，返回哪一个不作规定；都不写事件或 `processed_commands`。不足确认在其余业务校验都通过之后判定。
  - 物料不存在：`404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": "ITEM", "id"}`。指定的批次不存在：`details` 为 `{"entity": "LOT", "id"}`。
  - 报损原因不存在：`404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": "WASTE_REASON", "code"}`。
  - 停用的物料、报损原因照常受理；指定余量为 0 或为负的批次照常受理（按下方规则需要确认）。
  - 指定的批次属于其他物料：`400 LOT_ITEM_MISMATCH`，`details` 为 `{"line", "item_id", "lot_id"}`。
  - 物料还没有任何批次（从未收货入库）：`409 ITEM_HAS_NO_LOTS`，`details` 为 `{"line", "item_id"}`。带确认标记也拒绝：确认只处理数量不足，不授予报损资格。
    - 依据是 `inventory_lots` 中有没有该物料的批次，不是 `inventory_on_hand` 的行（视图对每个物料都有一行）。批次由收货建立（生产产出随生产切片加入），余量为 0 后也保留，所以曾经入库、现已耗尽的物料照常按下方规则处理。
  - 数量越界：逐行处理中的账面、扣减后的批次余量或账外缺口超出 `i64`：`400 VALIDATION_FAILED`，`details` 为 `{}`。整条命令不入账，前面的正常行也不入账；不得绕回、截断或钳制后继续。
  - `UNKNOWN_UNIT`、`UNIT_CONVERSION_CHANGED`：同收货。
  - `line` 是行在 `lines` 中的下标。
- **逐行处理**：按 `lines` 顺序，每行看到的账面是同一命令中前面各行处理之后的值。
  1. **吸收**：按「盘点吸收」判定（随盘点切片实现；在此之前每行都不被吸收）。被吸收的行写 `absorbed_by_event_id`，不需要确认，带了确认标记也忽略，不改变后续各行看到的账面。
  2. **账面**：`item_book_qty` 是该行之前的物料净账面（该物料全部批次余量之和，含负数，加账外缺口，即视图 `inventory_on_hand` 的值）。指定批次的行另记 `lot_book_qty`，即该行之前该批次的余量。被吸收的行同样记录。
  3. **是否需要确认**（未被吸收的行）：

     | 行 | 需要确认的条件 |
     |---|---|
     | 指定批次 | `qty > lot_book_qty`（批次余量为 0 或为负也算）或 `qty > item_book_qty` |
     | 不指定批次 | `qty > item_book_qty` |

     报损量等于账面时不需要确认。
  4. **分配**（未被吸收的行）：
     - 指定批次：整行扣在该批次上，`alloc` 恰好一项 `{lot_id, qty, "SPECIFIED"}`，不转去扣其他批次，该批次余量可以为负。
     - 不指定批次：按「批次」的 FIFO 顺序，在该物料余量大于 0 的批次中逐个扣到 0 或扣完为止，每个批次一项，记 `FIFO`；仍有剩余时，剩余部分一项，不带 `lot_id`，记 `SHORTFALL`（记入账外缺口）。
       - 净账面不小于报损量时，正余量批次之和一定足够，所以只有经过确认的行才会产生 `SHORTFALL`。
     - `alloc` 每项 `qty` 为正，合计等于该行 `qty`。
- **不足确认**：
  - `confirm_shortage: true` 表示员工确认这一行的数量确实发生，允许账面不足时入账。它不绑定预检时的账面或分配：带确认的行按提交时的账面入账，不足程度比预检时更大也有效。
  - 需要确认但没带确认的行，照确认后的结果继续试算后续各行。全部行处理完后，存在这样的行时整条命令不入账，返回 `409 WASTE_CONFIRMATION_REQUIRED`，`details` 为 `{"lines": [{line, item_id, lot_id?, qty, item_book_qty, lot_book_qty?}]}`，按 `line` 升序列出全部这样的行，一次确认完。
  - 账面足够的行带了确认标记照常处理。
- **重提**：
  - 只有明确收到拒绝（如 `WASTE_CONFIRMATION_REQUIRED`）之后，才可以用同一个 `command_id` 修改内容重提：被拒绝的命令不落库。确认后重提时 `captured_at` 沿用首次填写的值，只更新 `sent_at`；超过 72 小时返回 `CAPTURE_TOO_OLD`，改走补录。
  - 结果未知（超时、断线）时，先用原 ID、原内容重试取回结果，不能直接加确认标记。首次已成功时，原内容重试返回首次响应，即使已超过 72 小时（幂等检查先于时间校准）。
  - 成功之后用同一个 ID 去掉或改变确认标记：`409 IDEMPOTENCY_CONFLICT`，`fields` 为 `["lines"]`。
  - 修改某行的物料、批次或数量后，客户端清除该行的确认，重新预检。
- **警告**：本切片只有 `CAPTURE_TIME_ADJUSTED`。盘点切片实现吸收判定后，被吸收的行另返回 `ABSORBED_BY_COUNT`（见「盘点吸收」）。
  - 报损不返回 `STOCK_SHORTFALL`：不足已在入账前提示并确认，事件中有提交时的账面。
- **投影**：
  - 未被吸收的行：`alloc` 按顺序每项写一条 `inventory_movements`，`kind = 'WASTE'`，`alloc_source` 取该项的 `source`，`lot_id` 取该项的 `lot_id`（`SHORTFALL` 为 `NULL`），`nominal_qty = qty_delta = −qty`，`physical_at = occurred_at`。
    带 `lot_id` 的项把该批次的 `remaining_qty` 减去 `qty`；`SHORTFALL` 项把该物料的账外缺口减去 `qty`，`inventory_unallocated` 没有该物料的行时新建。
  - 被吸收的行：写一条 `kind = 'WASTE'`、`alloc_source = 'ABSORBED'`、`lot_id` 为 `NULL`、`nominal_qty = −qty`、`qty_delta = 0` 的流水，`absorbed_by_event_id` 取自 payload，不改动批次和账外缺口。
  - `reason_code`、`item_book_qty`、`lot_book_qty` 只在事件中。

**预检**：

- 请求体只有 `lines`，每项 `{item_id, lot_id?, input, reason_code}`，取值规则同正式提交；不接受 `command_id`、`captured_at`、`sent_at` 和 `confirm_shortage`。
- 在一个读事务中执行，不写事件、`processed_commands` 或任何表；没有幂等处理。
- 校验与正式提交相同（取值、引用、`LOT_ITEM_MISMATCH`、`ITEM_HAS_NO_LOTS`、单位、数量越界），错误码与 `details` 也相同。不做时间校准，也不做吸收判定（没有发生时间）。
- 成功的 `data` 是 `{"lines": [{line, item_id, lot_id?, qty, item_book_qty, lot_book_qty?, needs_confirmation, alloc}]}`，每行一项：按正式提交的逐行处理计算，假设全部行都不被吸收、需要确认的行都已确认。`warnings` 为空数组。
- 结果是按当前账面的估算：正式提交在写事务内重新计算，两次请求之间账面可能变化。界面把 `alloc` 显示为「按当前账面预计扣减」。

### 库存明细查询

| 方法与路径 | 查询参数 | 权限 |
|---|---|---|
| `GET /api/v1/inventory` | `item_id`（可选） | 已认证员工 |

- 查询参数与 `data` 见 `boh_domain::inventory` 的 `InventoryQuery`、`Inventory`、`InventoryItem`，全部内容读自同一个读事务。
  - `business_date` 是门店节点按当前时间计算的营业日（盘点草稿用它核对，见「盘点」第 1 条）。
  - `items` 每个物料一项，含停用的，按 `code` 的字节序升序；带 `item_id` 时只含该物料。
    `on_hand_qty` 是账面数（视图 `inventory_on_hand`），`unallocated_qty` 是账外缺口（`<= 0`，没有时为 0）。
  - `lots` 是该物料余量不为 0 的批次（含负余量），按 FIFO 顺序（批次日期、流水号）排列。每项是一个批次行（`LotDetails`，见「批次查询」），不含 `item_id`。
- 参数取值非法、未知或重复的参数：`400 VALIDATION_FAILED`。`item_id` 合法但不存在时 `items` 为空数组。

### 批次查询

| 方法与路径 | 权限 |
|---|---|
| `GET /api/v1/lots/{lot_id}` | 已认证员工 |

- 按标签上的批次号查一个批次，余量为 0 的也能查到（盘点补加历史批次用，见「盘点」）。
- `data` 是 `{"lot": 批次行}`，批次行见 `boh_domain::inventory::Lot`（库存明细中的批次行是其中不含 `item_id` 的 `LotDetails`）。
  `source_occurred_at` 是建批次事件的 `occurred_at`；`expires_at`、`manufacturer_lot_no` 没有时省略该键。
- 路径中的 `lot_id` 不符合批次号格式（类型为 `RAW` / `SEMI` / `FINISHED`，编码只含 `A`–`Z`、`0`–`9`、`_`，日期为真实日期，流水号 `001`–`999`）：`400 VALIDATION_FAILED`。格式合法但不存在：`404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": "LOT", "id"}`。

### 员工认证

- **设备注册**（平板安装时做一次）：在节点上运行 `boh-server enroll-code` 生成一次性注册码。新平板提交注册码和设备名，服务端签发设备令牌，作为 `device_id` 的来源，同时生成这台设备的解锁码。
  - 注册不使用 PIN：否则局域网内任何人都能对店长 PIN 试错。
  - 注册码由 CSPRNG 生成，不少于 40 bit，一次有效，10 分钟过期；错误的注册码尝试整个节点每分钟最多 10 次。
  - 在节点上运行 `boh-server revoke-device <设备>` 吊销设备。
- **解锁码**：每台设备一个，注册成功时由 CSPRNG 生成，不少于 80 bit（例如 16 位 Base32），只在注册界面显示一次，由店长自行保存（例如存在手机里）。节点只保存 Argon2id 哈希。
  - 丢失或泄露时在节点上运行 `boh-server reset-unlock-code <设备>` 重新生成，旧码立即失效。
- **员工登录**：只接受带有效设备令牌的请求。PIN 由服务端用 Argon2id 校验，签发会话令牌（服务端状态表）。PIN 一律 6 位数字。会话按班次有效（结束判定见 Q7）。
- **PIN 设定与重置**：新员工没有 PIN，不能登录。
  - 店长在已注册平板上登录后，选中另一名启用状态的员工，每次首次发起「设置 / 重置 PIN」都输入并校验自己的当前 PIN；失败计入设备 PIN 失败次数，设备锁定时不校验。服务端在同一事务中吊销该设备上的全部员工会话、使该设备已有的未用 PIN 重置授权失效并创建新授权，再把平板交给该员工本人输入两次新 PIN。店长不输入、也看不到员工的新 PIN。
  - PIN 重置授权的秘密由平板在发起命令前用 CSPRNG 生成，不少于 128 bit，随命令经 HTTPS 提交；节点只保存 Argon2id 验证值，响应只含非秘密的范围、状态和期限。授权 10 分钟过期，绑定发起店长、目标员工、`device_id` 和「设置 / 重置 PIN」操作，只能成功执行一次，不是员工会话，不授予其他 HTTP 权限。
  - 店长不能通过重置授权设定或重置自己的 PIN，返回 `409 SELF_RESET_NOT_ALLOWED`；自己的 PIN 照常经旧 PIN 校验后修改，无法校验旧 PIN 时用 CLI 恢复。
  - 员工凭有效设备令牌和 PIN 重置授权秘密提交新 PIN，无需员工会话；服务端核对秘密、设备与授权一致、设备未被吊销、目标员工启用且不是发起人、发起人仍为启用的 `MANAGER`，在同一事务中完成 PIN 变更、消费授权及会话和相关授权的失效。
  - 已登录员工可以修改自己的 PIN：输入旧 PIN 和两次新 PIN。旧 PIN 校验失败计入该设备的 PIN 失败次数；设备锁定时拒绝旧 PIN 校验（见「在线限速」）。
  - 节点命令 `boh-server reset-pin <员工 code>` 在终端输入两次新 PIN，用于没有可登录店长的情况。
  - 设定、重置、修改成功后，该员工在所有设备上的已有会话及以其为发起人或目标的未用 PIN 重置授权立即失效；不产生事件。
  - 每次操作记日志，含来源（`HTTP` / `CLI`）、目标员工、发起人和 `device_id`；HTTP 的发起人和 `device_id` 取已验证会话或 PIN 重置授权保存的身份，CLI 的发起人和 `device_id` 为 `null`，不使用保留系统身份。
- **业务事件身份**：HTTP 业务事件的 `actor_id` 来自已验证的员工会话，`device_id` 来自该会话所属的注册平板设备令牌。`Actor` 提取器不接受客户端指定的系统身份，缺少或无效的设备令牌 / 员工会话必须拒绝请求，不回退到系统身份。
- **员工状态**：登录和每个已认证请求都读取 `employees` 投影，员工必须是启用状态，否则与无效会话同样拒绝。权限按投影中的当前 `role` 判断，不使用登录时的角色。停用员工不改动认证状态表，已有会话在下一次请求时失效。
- **在线限速**（PIN 能安全使用的前提）：
  - 按设备计数：同一设备 1 小时内 PIN 校验失败 5 次（不论输入的是哪个员工），该设备的登录 PIN 校验、修改 PIN 的旧码校验、店长重置授权签发和员工主数据权限提升的 PIN 校验及敏感字段幂等比对均锁定，界面显示设备名并提示由店长解锁。登录或 PIN 操作成功不清零计数。
    - 按时间窗口计数，不按「连续失败」：否则夹一次自己的成功登录就能无限试错。
  - 锁定不自动过期，只能输入该设备的解锁码解锁，或在节点上运行 `boh-server unlock-device <设备>`。解锁后计数清零。锁定不影响已有会话的其他业务操作。
  - 不按员工锁定：否则任何人都能针对性地锁住某个员工（尤其是店长）。
  - 解锁码校验每台设备每 10 秒最多一次，失败不锁定，记 `warn` 日志。
    - 解锁码失败不锁定：解锁码无法猜中，再加锁定只会让人能把平板推到只能在节点上解锁的状态。
  - 整个节点每小时 PIN 失败超过 50 次时，所有 PIN 校验额外延迟 5 秒，并在 `/health` 中报告 `auth_failures_last_hour`。
  - 每次 PIN 失败记 `warn` 日志，含员工和 `device_id`。
- 客户端不缓存 PIN 哈希。
- 含 PIN 或 PIN 重置授权秘密的命令只在当前页面内存暂存，不进 `IndexedDB`；结果未知时用原 `command_id`、原内容和原秘密重试，丢失内存后不得用同一 ID 替换秘密。
- 丢失内存后，登录或自行改 PIN 以新命令登录核实当前 PIN；重新创建重置授权须店长重新登录、校验当前 PIN 并使用新 `command_id`。
- **传输加密**：设备令牌、PIN、会话令牌、PIN 重置授权只经 HTTPS 传输。局域网 HTTPS（证书方案见 Q9）是认证切片的前置条件；节点未配置 TLS 时不注册认证接口，release 构建启用认证但未配置 TLS 时**拒绝启动**。
- 认证落地之前，业务切片只依赖 `Actor` 提取器，开发桩同时提供员工 ID 和 `role`。开发桩只在 debug 构建中可用；release 构建配置了开发桩时**拒绝启动**。
  - 配置项 `dev_actor_stub = true` 时启用，默认 `false`。未启用时没有任何身份来源，业务接口一律 `401 UNAUTHENTICATED`。
  - 开发桩按请求读取 `X-Dev-Employee-Id`（UUIDv7，作为 `actor_id`）、`X-Dev-Device-Id`（UUIDv7，作为 `device_id`）和 `X-Dev-Role`（`STAFF` / `MANAGER`）。缺少任一个、取值非法或使用保留系统 ID：`401 UNAUTHENTICATED`。
  - 开发桩不查 `employees` 投影，也不校验设备注册。

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

- **批次号** `lot_id`：批次的唯一标识，同时是印在标签上、员工用来识别实物的号。格式 `<类型>-<编码>-<YYYYMMDD>-<流水号>`，例如 `RAW-FLOUR-20260910-001`：
  - 类型、编码取建批次时该物料的 `category`、`code`；
  - 日期是批次日期（见下）；
  - 流水号三位、补零，`001`–`999`。
  - 由写入线程在建批次的命令中生成，写进 payload；之后修改物料分类、重试命令、重建投影、重打标签都不改变它。
  - 不含门店代码：它只在门店内唯一，跨门店由同步批次头的 `store_id` 区分。
  - 生产商批号 `manufacturer_lot_no` 是另一项可选属性，不参与编号。
- **批次日期**：建批次事件的 `occurred_at` 换算到门店时区（配置 `timezone`）的日历日期，不用营业日。
  - 收货取收货的 `occurred_at`，生产产出取生产的 `occurred_at`（完成时刻）。
  - 例：服务器时间 10-09 00:10、lag 20 分钟，`occurred_at` 为 10-08 23:50，批次日期为 10-08。
  - 补录时 `occurred_at` 由员工填写，批次日期随之是那一天。
- **流水号**：同一物料、同一批次日期内，按入账顺序取已有最大流水号 + 1；同一命令中靠前的行先取。
  - 同一天内不还原实物先后：A 先到但后入账，B 先入账，则 B 为 `001`、A 为 `002`。
  - 批次只增不删，已用的流水号不会再分配。
  - 超过 `999` 时整条命令拒绝：`409 LOT_SERIAL_EXHAUSTED`，`details` 为 `{"line", "item_id", "lot_date"}`（`lot_date` 为 `'YYYY-MM-DD'`），不入账，不循环使用。
- **标签打印**：标签只在建批次的命令成功之后打印，内容取自成功响应。
  - 结果未知时用原 `command_id`、原内容重试，拿到首次响应中的同一批次号后再打印。
  - 重打标签取库存明细或批次查询中的批次号。
- **到期时间** `expires_at`（UTC 毫秒，可空）只用于过期提醒，不参与分配。收货批次必有到期时间，由收货行的 `expires_on` 换算（见「收货接口」）；生产产出的到期时间随生产切片定。
- **FIFO 分配**：未指定批次的扣减，只在该行物料余量大于 0 的批次中，按批次日期升序、同一日期内按流水号升序逐个扣减。
  - 不按 `seq`、UUID、到期日或实物发生钟点。
  - 补录的旧日期批次排在较新日期的批次之前。
- **来源**：批次投影保存建批次的 `source_event_seq` 和 `source_line_no`（收货取原 `lines[i]` 的 `i`，生产 `output` 取 0），只用于追溯，不参与分配。
- **指定批次**：客户端可以指定 `lot_id`。指定批次的扣减只作用于该批次，不转去扣其他批次；超出该批次账面时，按报损的确认规则入账，该批次余量可以为负。
- **账面不足**：
  - 指定批次时，差额留在该批次上（余量为负）；
  - 未指定批次时，扣完该物料全部余量大于 0 的批次后，剩余部分记入账外缺口。
  - 报损在账面不足时先提示、经员工确认后入账（见「报损接口」）；还没有任何批次的物料不能报损。
  - 下一次包含该批次或该物料的盘点会校准它们（见「盘点」）。
- **负余量**：批次余量可以为负，表示账面少于实物。FIFO 跳过余量不大于 0 的批次。
- 分配结果连同来源写进 payload：`alloc[{lot_id?, qty, source}]`。
  - `qty` 为正数，不带 `lot_id` 表示账外缺口。
  - `source` 为 `SPECIFIED` / `FIFO` / `SHORTFALL`，纠错另有 `CORRECTION` / `REVERSAL`（见「纠错」）。
  - 重放时直接使用，不重新分配。
- **到期提醒**：新批次的 `expires_at` 早于同物料另一个按 FIFO 先被扣的批次时，返回警告 `EXPIRES_BEFORE_OLDER_STOCK`。
  - 「另一个批次」指批次号排在新批次之前、`remaining_qty > 0`、`expires_at` 不为空的批次，含同一命令中靠前的行新建的批次。
  - 每个命中的行一条警告，`details` 为 `{"line", "item_id", "lot_id"}`：`line` 是该行的下标，`lot_id` 是新批次。
- **承诺边界**：批次是账面推定，不是实物证据。追溯报告分开显示「员工指定」「系统推定」「账外 / 吸收」三类。

## 事件目录

所有数量是基本单位 `i64`，金额是 `i64` 分。`schema_version` 从 1 开始。
`actor_id`、`device_id`、`occurred_at`、`business_date` 存在 `store_events` 的列中，不重复写进 payload。
`input` 见「单位」，`alloc` 和批次号 `lot_id` 见「批次」。新建批次的行带新批次号，不会被吸收；扣减库存的行（盘点行除外）要么带 `alloc`，要么带 `absorbed_by_event_id`（被吸收，见「盘点吸收」），二者取一。
带 `?` 的字段可选：缺省时省略该键，payload 中不出现 `null`。字段之间的 `a | b` 表示二者恰有一个出现；枚举值写作（`A` / `B`）。

| event_type | aggregate_type | payload 要点 | 投影影响 |
|---|---|---|---|
| `GOODS_RECEIVED` | `RECEIPT` | `@2`：`boh_domain::receiving::GoodsReceived`；样本 `crates/boh-app/tests/golden/GOODS_RECEIVED@2/`；`lot_id` 是批次号。`@1`（`lot_id` 为 UUID）不支持，见「收货接口」 | 每行新建一个批次 |
| `PRODUCTION_BATCH_COMPLETED` | `PRODUCTION_BATCH` | `recipe_id`, `recipe_version`, `batch_count`, `started_at?`, `output{item_id, planned_qty, qty, lot_id, expires_at?}`, `consumed[{item_id, planned_qty, qty, alloc \| absorbed_by_event_id}]` | 原料按分配扣减（或被吸收）；成品新建批次 |
| `WASTE_LOGGED` | `WASTE_RECORD` | `lines[{item_id, lot_id?, qty, input, reason_code, item_book_qty, lot_book_qty?, alloc \| absorbed_by_event_id}]`：`lot_id` 是员工指定的批次；`item_book_qty` 是提交时该行之前的物料净账面，指定批次的行另带该批次账面 `lot_book_qty`（与 `lot_id` 同时出现）。见「报损接口」 | 按分配扣减（或被吸收） |
| `STOCK_COUNT_SUBMITTED` | `STOCK_COUNT` | `purpose`（`CLOSING` / `AUDIT`）, `lines[{item_id, lot_id, counted_qty}]` | 无 |
| `STOCK_ADJUSTED` | `STOCK_COUNT` | `lines[{item_id, lot_id?, book_qty, counted_qty, delta}]`：带 `lot_id` 的是批次行，不带的是每个物料一行的账外缺口清零 | 批次和账外缺口按 delta 变化；写 `inventory_counts` |
| `PURCHASE_ORDER_SUBMITTED` | `PURCHASE_ORDER` | `supplier_id`, `lines[{item_id, qty, input}]`, `deliver_on`（门店当地日期 `'YYYY-MM-DD'`） | 无 |
| `TEMPERATURE_LOGGED` | `TEMPERATURE_READING` | `boh_domain::temperature::TemperatureLogged`；样本 `crates/boh-app/tests/golden/TEMPERATURE_LOGGED@1/` | 写 `temperature_readings` |
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

1. **盘点表与草稿**：客户端按库存明细生成盘点表，不调用专门的盘点接口，不产生事件。库存明细查询返回物料余量不为 0 的批次、账外缺口及门店节点当前的 `business_date`（见「库存明细查询」）。
   - 每个批次行显示批次号、收货或生产时间、到期日、生产商批号。盘点表不预填清点数量，有空格时客户端不允许提交。
   - 余量为 0 的已有批次确认有实物时，按标签上的批次号经「批次查询」找到，补加到盘点表。
   - 草稿按设备保存在平板的 `IndexedDB` 中，刷新、休眠后都能恢复。草稿保存清点数及物料 / 批次标识、营业日和重试所需的命令信息，不保存库存余量，不改变「客户端不维护库存镜像」的规则。
   - 提交成功后删除草稿；提交被拒时保留已填的数，刷新盘点表后只补新出现的批次行。
   - 首次提交时生成的 `command_id` 存进草稿。结果未知时草稿锁定为只读，用原 ID、原内容重试，直到拿到结果。
   - **【默认】草稿跨营业日不能提交**：草稿保存打开盘点表时门店节点返回的当前 `business_date`；首次提交或被拒后重新提交前，用库存明细查询返回的当前 `business_date` 核对，跨营业日时清空草稿、重新清点。客户端不自己计算时区和日切。结果未知的命令先按原 ID、原内容取回结果，再处理草稿的营业日。
2. **提交盘点**：命令携带 `purpose`（`CLOSING` / `AUDIT`）和逐批次的清点结果，`captured_at` 取员工点「完成盘点」的时刻，同一事务内写入 `STOCK_COUNT_SUBMITTED` 和 `STOCK_ADJUSTED`。不加审批，提交即调整，录错就重盘。
   - **【默认】允许按区域或物料组分几次提交**，界面默认这样组织；同一营业日分次提交的 `CLOSING` 盘点各自入账。本期不加存放位置字段。
3. **范围与校验**：只针对 `lines` 里出现的物料。对其中每个物料，范围是该物料的全部批次（含余量为 0 的）加上账外缺口，范围与余量一律以门店节点提交时的值为准。
   - 填入的数量一律视为实有数量，没填不等于 0。被盘物料中余量不为 0（含负余量）的每个批次都必须出现在 `lines` 中，实物没有就填 0；缺少时返回 `400 COUNT_LINE_MISSING`，列出缺少的批次，整条命令不入账。打开盘点表之后新建的批次同样校验，客户端刷新盘点表后补填。
   - 余量为 0 的批次可以不列，也可以补加。每行都必须带 `lot_id`，缺少时 `400 VALIDATION_FAILED`：盘点只调整已有批次，不新建批次。
   - 尚未完成收货或生产登记的实物不能混入已有批次的清点数：暂停该物料的盘点，先完成登记，再重新清点该物料；来源不明的实物按 [SOP](sop.md)「盘点」处理。
   - `lines` 中的 `lot_id` 不属于该物料或不在范围内：`400 LOT_NOT_IN_COUNT_SCOPE`；同一批次出现两次：`400 DUPLICATE_COUNT_LINE`。
4. **逐批次调整**：
   - 对每个列出的批次：`delta = counted_qty − remaining_qty`，`remaining_qty` 取提交时的余量（可为负，例如余量 −3 盘得 2，`delta` 为 +5）；未列出的零余量批次不调整。
   - 账外缺口清零。
   - `book_qty`、`delta` 都写进 payload，重放时不重新计算。`STOCK_ADJUSTED.lines` 包含范围内余量不为 0 或被清点的每个批次，另外每个物料恰有一行不带 `lot_id`：`book_qty` 是账外缺口，`counted_qty` 为 0，`delta = −book_qty`（归零）。
5. **提交响应**：响应 `data` 返回服务端计算的 `lines[{item_id, lot_id?, book_qty, counted_qty, delta}]`，与第 4 条的调整 payload 一致。界面以响应为准，不显示客户端自己计算的差异。同一 `command_id`、同一规范化请求重发时，原样返回首次的响应（含 warnings），不按当前库存重新计算，`seq` 不增加。
6. **盘点投影**：被盘的每个物料写一行 `inventory_counts`（零差异也写）：`book_qty` 是提交时范围内批次余量与账外缺口之和，`counted_qty` 是清点合计。分次提交分别写入。
7. **操作规范**：盘点只数实物，清点到提交期间被盘物料不得变动，见 [SOP](sop.md)「盘点」。
   系统不检测盘点期间的变动，违反规范时差异会算错，由店长重盘。

## 盘点吸收

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

- 新建批次的行（收货行、生产产出）不吸收，照常建批次：盘点时这批货还没有批次号，按 [SOP](sop.md)「盘点」不会被清点进已有批次，而是先登记、再单独清点。
- 冲销或更正新建批次的行时，只有在建批次事件入账之后提交的盘点（`c.event_seq` 大于该批次的 `source_event_seq`）才能吸收它；更早入账的盘点看不到这个批次。

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
- **扣回时批次余量不够**（原批次已被后续扣减）：返回 `STOCK_SHORTFALL`，界面提示「建议现在盘点该物料」。
  - 数量更正：批次真实存在，差额直接从原批次扣，原批次余量可以为负。下一次盘点该批次时校准。
  - 整条冲销：收货或生产实际没有发生，批次扣到 0，不足部分记入账外缺口（差额不属于任何真实批次）。下一次包含该物料的盘点会清零，不会在盘点差异里重复出现。
- **退回会使账外缺口大于 0**（原分配含账外缺口，而该缺口已被一次入账在后、观察时点却早于原行的盘点清零，只在违反盘点操作规范时出现）：`409 COUNT_REQUIRED`。先盘点该物料，新盘点的观察时点晚于原行，纠错随后会被吸收。

**数量更正 `QUANTITY_CORRECTED`**：

- `line_ref` 定位原事件中的一行：`lines[i]`（收货、报损）、`consumed[i]`、`output`（生产），`i` 为原数组下标。`item_id` 必须与该行一致，否则 `400 LINE_MISMATCH`。
- `old_qty` 是该行当前有效数量，`delta = new_qty − old_qty`，`delta ≠ 0`，`new_qty >= 0`。原行带 `input` 时（收货、报损），新数量照常带 `input`；生产的行不带。
- 收货行可同时更正 `line_cost_cents`。生产只更正数量，不更正 `batch_count` 和配方（录错配方要冲销）。
- 未被吸收的行：

  | 行类型 | `delta > 0` | `delta < 0` |
  |---|---|---|
  | 新建批次的行（收货行、生产 `output`） | 原批次 + delta | 从原批次扣回，原批次余量可以为负 |
  | 扣减库存的行（生产 `consumed`、报损行） | 指定批次的报损行从原批次追加扣减（可为负）；其他按 FIFO 追加分配 | 按有效分配的**逆序**退回（最后分配的先退，账外缺口部分退回账外缺口） |

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

- Excel / CSV 在**平板浏览器中解析**，转成统一的 JSON 命令提交。Rust 端只处理 JSON，不为 Excel 引入依赖。命令走普通的 write path，幂等规则不变。
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

库存投影由迁移 005 定义，迁移 007 重建 `inventory_lots` 和 `inventory_movements`（批次号为主键、拆出批次日期与流水号、余量可为负），字段与约束以迁移为准：

| 表 / 视图 | 内容 |
|---|---|
| `inventory_lots` | 每个批次一行，主键是批次号 `lot_id`，余量为 0 的批次保留，余量可为负；批次日期、流水号由 `lot_id` 拆出，用于 FIFO；`source_event_seq`、`source_line_no` 是建批次的事件和原 payload 下标（见「批次」），之后的调整不改变它们 |
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
- `kind`：收货 `RECEIPT`，生产产出 `PRODUCE`，生产用料 `CONSUME`，报损 `WASTE`，盘点调整 `ADJUST`；纠错行沿用原行的 `kind`（见「纠错」）。
- `alloc_source`：新建批次 `NEW_LOT`；盘点对已有批次和账外缺口的调整 `COUNT`；被吸收 `ABSORBED`；其余直接取 payload 中 `alloc` 的 `source`。
- **不变量**（测试和定时自检都检查）：
  - 批次余量 `remaining_qty` = 该批次所有流水的 `qty_delta` 之和；
  - 账外缺口 = 该物料 `lot_id IS NULL` 的流水 `qty_delta` 之和，且 `<= 0`；
  - 账面数（视图 `inventory_on_hand`）= 批次余量之和 + 账外缺口。
- **日报**：视图（随报表需要定义），按 `business_date, item_id, kind` 聚合流水。收货、产出、用料、报损按 `nominal_qty`（申报值，含被吸收的行），盘点调整按 `qty_delta`。不维护增量日结表。
- 销售导入切片新增 `sales_days(business_date PRIMARY KEY, aggregate_id, version, source_event_id)`、`daily_sales(business_date, item_id, qty, amount_cents, source_event_id)`，表结构随该切片定。
- 主数据表也是投影：`equipment`（迁移 002），`items`、`item_units`、`recipes`、`recipe_versions`、`recipe_lines`、`suppliers`、`waste_reasons`（迁移 005）；`employees` 随认证切片加入。
- 所有投影都必须能通过 `boh-server rebuild-projections` 从 `store_events` **按 `seq`** 完整重建，结果与在线写入逐行一致。

## 已知限制

- **批次追溯是账面推定**。员工实际拿的批次和系统推定的不一致时，批次余量会偏离实物，直到下一次盘点。
- **同一天内的批次顺序按入账先后**，不还原实物到达的先后：先到但后入账的批次流水号更大，FIFO 排在后面。
- **批次日期依赖门店节点时钟**：门店长期离线、门店节点和平板的日期都错、操作人员又没有核验界面显示的发生日期时，会产生日期错误的记录和批次号，批次号印出后不能修改。将来可以加入联网自动校时，事件同步到总部后供集中复核。
- **盘点期间的实物变动**：系统不检测，靠操作规范禁止，违反时由重盘纠正。
- **时钟**：系统时钟跳到未来再拨回时，期间事件的 `business_date` 是错的，不能自动修正；`/health` 会暴露这种情况，并持续 `degraded` 到真实时间追上那些记录（见「时间」时钟异常）。
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
| 指定批次不足 | 面粉批次 A 5、B 10；报损面粉 8 g，指定 A | 不带确认：`409 WASTE_CONFIRMATION_REQUIRED`，列出该行（`lot_book_qty` **5**、`item_book_qty` **15**），不入账；同一 `command_id` 加确认重提后分配 `A 8 SPECIFIED`；A **−3**，B **10** 不动 |
| 指定批次、净账面不足 | 批次 A 5，账外缺口 −10；报损 3 g，指定 A | 批次够但净账面 **−5** 不足：不带确认时 `409 WASTE_CONFIRMATION_REQUIRED`；确认后 `A 3 SPECIFIED`；A **2**，账面 **−8** |
| 有负批次时不指定批次 | 批次 A −3、B 10（净账面 7）；报损 8 g，不指定批次 | 需要确认；确认后 `B 8 FIFO`；A **−3**、B **2**，账外缺口 **0**，账面 **−1** |
| 不足记入账外缺口 | 只有批次 A 5；报损 8 g，不指定批次 | 需要确认；确认后 `A 5 FIFO` + `3 SHORTFALL`；A **0**，账外缺口 **−3**，账面 **−3**；不返回 `STOCK_SHORTFALL` |
| 同一命令的后续行 | 批次 A 5、B 10（A 在前）；一次报损两行：`lines[0]` 不指定批次 8 g，`lines[1]` 指定 A 1 g | `409` 只列 `line` **1**（`lot_book_qty` **0**、`item_book_qty` **7**）；确认后 `lines[0]` 为 `A 5 FIFO` + `B 3 FIFO`，`lines[1]` 为 `A 1 SPECIFIED`；A **−1**、B **7** |
| 从未收货的物料 | 面粉已有批次；糖已建档但从未收货；报损糖 1 g（带或不带确认），或一次报损面粉 1 g 和糖 1 g | `409 ITEM_HAS_NO_LOTS`，列出糖所在的行；整条不入账；预检同样拒绝 |
| 收货后耗尽 | 只有批次 A 5，报损 8 g 确认后 A **0**、账外缺口 −3；再报 2 g，不指定批次 | 照常需要确认；确认后整行 `2 SHORTFALL`，账外缺口 **−5** |
| 数量越界 | 批次 A 的余量已是 `i64::MIN + 2`；指定 A 报 3 g（确认）；或同一命令前面另有一条正常行 | `400 VALIDATION_FAILED`；整条不入账；指定 A 报 2 g（确认）照常入账，A 为 `i64::MIN`。账外缺口累加越界同样处理 |
| 预检只读 | 批次 A 5；预检报损 8 g，指定 A | 返回该行 `needs_confirmation` **true**、`lot_book_qty` **5**、预计 `A 8 SPECIFIED`；事件、投影和 `processed_commands` 不变 |
| 确认后不足加大 | 批次 A 5；预检指定 A 报 8 后员工确认；提交前另一台平板报损 A 2 已入账；带确认提交 | 照常入账，`lot_book_qty` **3**（提交时的值）；A **−5** |
| 提交时新出现的不足 | 批次 A 5；预检指定 A 报 4 不需要确认；提交前另一台平板报损 A 2 已入账；不带确认提交 | `409 WASTE_CONFIRMATION_REQUIRED`（`lot_book_qty` **3**）；不入账 |
| 确认标记与幂等 | 带确认的报损成功后，用原 `command_id`、原内容重发；再去掉确认标记重发 | 第一次原样返回首次响应，`seq` 不增加；第二次 `409 IDEMPOTENCY_CONFLICT`，`fields` 为 `["lines"]` |
| 同次收货多个批次 | 09-10 一次收货面粉两行：`lines[0]` 建 A（`RAW-FLOUR-20260910-001`）5 g，`lines[1]` 建 B（`…-002`）10 g；报损面粉 8 g，不指定批次 | 按批次号分配 `A 5 FIFO` + `B 3 FIFO`；余量 A **0**、B **7** |
| 同次收货重复提交 | 上例扣减后，用原 `command_id`、原内容重发收货 | 返回原批次号 A、B；`seq` 不增加；余量仍为 A **0**、B **7** |
| 批次号重建 | 保存同次收货后尚未扣减的原库结果，清空投影，按 `seq` 重建，再报损面粉 8 g，不指定批次 | 重建后投影逐行一致，批次号仍为 `…-001`、`…-002`；后续分配 `A 5 FIFO` + `B 3 FIFO`，余量 A **0**、B **7** |
| 批次日期与流水号 | 面粉：09-10 08:00 收货一行；09-10 14:00 收货两行；09-11 收货一行；09-12 补录一张 09-09 的收货 | 批次号依次为 `RAW-FLOUR-20260910-001`、`…-20260910-002`、`…-20260910-003`、`…-20260911-001`、`…-20260909-001`；FIFO 先扣 `…-20260909-001` |
| 午夜前后的批次日期 | 服务器时间 10-09 00:10 收货，lag 20 分钟；另一次 lag 5 分钟 | 前者 `occurred_at` 为 10-08 23:50，批次日期 **20261008**；后者为 10-09 00:05，批次日期 **20261009** |
| 流水号用尽 | 同一物料同一天已有 `…-999` 批次，再收一行 | `409 LOT_SERIAL_EXHAUSTED`；不入账 |
| 修改物料分类 | 面粉（`RAW`）已有批次 `RAW-FLOUR-20260910-001`；把分类改为 `SEMI` 后当天再收货 | 原批次号不变；新批次为 `SEMI-FLOUR-20260910-002`（流水号按物料与日期连续） |
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
| 换算变化 | 离线录入「2 袋，每袋 25000 g」；提交前换算改为 20000 | `409 UNIT_CONVERSION_CHANGED`，不入账 |
| 到期早于先扣的批次 | 面粉已有批次 A 余量 5、到期 10-20；一次收货 `lines[0]` 到期 10-15，`lines[1]` 到期 10-25，`lines[2]` 到期 10-22 | 两条 `EXPIRES_BEFORE_OLDER_STOCK`：`line` **0**（早于 A）、`line` **2**（早于同次的 `lines[1]`）；`lines[1]` 不警告；三行都入账 |
| 到货即过期 | 10-08 收货，`expires_on` 10-07 | `400 INVALID_LOT_DATES`，`reason` `EXPIRED_ON_RECEIPT`；不入账。`expires_on` 10-08 照常受理 |
| 凌晨收当天生产的货 | 10-08 02:00 收货（营业日 10-07），`produced_on` 10-08 | 照常受理；`produced_on` 10-09 时 `400 INVALID_LOT_DATES`，`reason` `PRODUCED_IN_FUTURE` |
| 销售示例 | 生产 100、报损 8、闭店实数 5（调整 −87）、销售 84 | 未解释损耗 **3** |
| 幂等 | 同一命令发送 2 次，`sent_at` 不同 | 第二次原样返回首次响应（含 warnings），`seq` 不增加 |
| 盘点后的正常业务 | 09:20 盘点；10:30 报损并提交 | 不被吸收，正常扣减 |
| 盘点后补录收货 | 09:00 到货一袋面粉未登记，09:20 盘点面粉时未清点它；09:40 补录收货，`occurred_at = 09:00` | 不被吸收，照常建批次；账面 = 盘点数 + 这袋面粉 |
| 冲销盘点后补录的收货 | 上例补录的收货录错，10:00 冲销 | 不被 09:20 的盘点吸收（盘点时该批次不存在）；从该批次扣回 |
| 冲销时批次已消耗 | 收货批次 L 10，已用掉 3，之后没有盘点；冲销这次收货 | L 扣到 **0**；账外缺口 **−3**；返回 `STOCK_SHORTFALL` |
| 数量更正 | 收货录成 1000（实际 100），批次 L1；生产从 L1 扣 300；之后没有盘点；更正为 100 | L1 **−200**；账外缺口 **0**；返回 `STOCK_SHORTFALL` |
| 更正多次后冲销 | 报损 10 → 更正为 8 → 更正为 6 → 冲销 | 冲销退回 **6**；报损合计 **0** |
| 重复冲销 | 同一事件冲销两次 | 第二次 `409 ALREADY_REVERSED` |
| 冲销后更正 | 冲销后再更正同一事件 | `409 ALREADY_REVERSED` |
| 员工停用 | 员工已登录；之后该员工被停用 | 已有会话的请求与无效会话同样被拒绝，不入账；该员工 PIN 登录被拒绝 |
| 店长降级 | 另有一名已设定 PIN 且启用的店长；当前店长已登录，之后 `role` 改为 `STAFF` | 已有会话立即只有 `STAFF` 权限 |
| 新员工无 PIN | 新建员工后未设定 PIN 即登录 | 登录被拒绝 |
| 重置 PIN 吊销会话 | 员工在两台设备上各有会话；店长为其重置 PIN | 两个会话的请求都被拒绝；新 PIN 可以登录，旧 PIN 不能 |
| 普通员工不能重置 | `STAFF` 会话为其他员工发起重置 PIN | 被拒绝，PIN 不变 |
| 自行修改 PIN 旧码错误 | 已登录员工修改 PIN 时旧 PIN 输错 | 被拒绝，PIN 不变；计入该设备的 PIN 失败次数 |
| 锁定设备修改 PIN | 已有会话在同一设备 1 小时内输错旧 PIN 5 次，再提交旧 PIN | 不再校验旧 PIN，PIN 不变；已有会话的其他业务操作仍可用 |
| 终端重置 PIN 审计 | 在节点运行 `reset-pin` 成功重置员工 PIN | 日志含 `CLI` 来源和目标员工，发起人与 `device_id` 为 `null`，不用保留系统身份 |
| 交接平板权限 | 店长发起员工 A 的 PIN 重置，平板另一标签页也有员工会话，再交给 A | 该设备所有旧会话失效；新授权只能在该设备设置 A 的 PIN，不能访问主数据写接口或重置 B 的 PIN |
| PIN 重置授权失效 | 用新命令提交已成功使用或已过期的授权，或从另一设备提交 | 拒绝，PIN 不变；同一授权不能再次执行 PIN 变更 |
| PIN 变更使旧授权失效 | 员工已有未用重置授权（作为发起人或目标），之后该员工的 PIN 被设定、重置或修改 | 这些授权立即失效，不能再执行 PIN 变更 |
| 最后一名可登录店长 | 仅有一名已设定 PIN 且启用的店长，另一个店长尚未设 PIN；停用或降级前者 | `409 LAST_ACTIVE_MANAGER`，员工、事件和 `processed_commands` 不变 |
| 店长权限提升验 PIN | A、B 的 PIN 不同；持有店长 A 会话，提升 STAFF B 或新建 MANAGER；输入 B 的 PIN 或锁定设备提交 | 拒绝，员工、事件和 `processed_commands` 不变；须 A 的当前 PIN 正确且设备未锁定才能首次执行，错误计入同设备失败次数 |
| 店长重新启用验 PIN | 启用停用的 MANAGER，或同时启用停用 STAFF 并改为 MANAGER；另编辑已启用 MANAGER 的姓名 | 前两者须校验会话店长的当前 PIN；仅编辑姓名不要求再次验 PIN，仍须有效 MANAGER 会话 |
| 权限提升原命令重试 | 员工提升成功后再被降级或停用，以原 ID、原内容和原 PIN 重试；再省略或改变 PIN | 原命令须核对保存的秘密证明，只返回原非秘密回执，不恢复权限或新增事件；省略或改变 PIN 为幂等冲突，受设备锁和限速约束 |
| 店长重置再次验 PIN | 持有有效店长会话，发起其他员工的 PIN 重置但店长 PIN 错误，或设备已锁定 | 不创建授权；错误 PIN 计入失败次数，锁定后不再校验；不能仅凭会话重置 PIN |
| 店长不能自重置 | 店长以自己为重置授权的目标 | `409 SELF_RESET_NOT_ALLOWED`；旧 PIN 保护的自行修改和 CLI 恢复仍可用 |
| 敏感认证幂等 | 首次成功后用原 ID 和原内容重试；再用同一 ID 改变旧 PIN、新 PIN 或授权秘密 | 原内容返回原非秘密回执，不重复执行或恢复权限；改变秘密返回 `409 IDEMPOTENCY_CONFLICT`，只列字段名，失败比对受设备锁和限速约束 |
| 敏感命令秘密不落库 | 完成初始化、终端重置、自行修改、授权创建和兑换、店长权限提升或重新启用，再检查数据库、WAL、备份、日志及留存请求和响应 | 没有明文 PIN、确认 PIN 或授权秘密；秘密字段只有带独立盐的 Argon2id 验证值，授权响应只有非秘密元数据 |
| 授权响应丢失 | 平板已保存原命令与授权秘密于内存，授权创建或兑换成功但响应丢失 | 原命令可取回原非秘密回执；不重复创建、兑换或恢复已失效授权，仍核对有效设备 |
| 自行改 PIN 后刷新 | 员工自行改 PIN 已成功但响应丢失，刷新后原命令内存丢失 | 以新命令用新 PIN 登录，不要求店长重置授权，不用旧 ID 提交替换内容 |

## 未来功能（本期不实现）

- **门店调拨**：`TRANSFER_SENT` / `TRANSFER_RECEIVED`，含在途归属规则。
- **总部同步**：上行契约见 AGENTS.md「上行同步」；下行为主数据包导入（`source = HQ_PACKAGE`）。总部从事件自行计算报表，必须区分被吸收的行和正常生效的行。总部关账后可下发锁账日（见 Q6）。
- **采购对接**：采购单与收货的关联、在途库存。
- **批次标签打印**：标签版式、打印机接入和扫码录入延后；批次号与打印契约见「批次」。

## 待确认问题

- **Q6 锁账日**：默认方案是店长在管理界面设置锁账日，`business_date` 不晚于它的补录和纠错返回 `409 BOOKS_LOCKED`；另外默认最多补录 7 天以内。**阻塞补录入口**。
- **Q7 会话的「班次」如何结束**：默认方案是员工主动登出，或登录满 12 小时。**阻塞认证切片**。会话结束时平板队列中未提交命令的处理一并确认，因为 `actor_id` 取提交时的会话。
- **Q9 局域网证书**：默认方案是总部私有 CA 为每个门店节点签发证书，平板初始化时安装一次根证书；备选是公网域名 + ACME DNS 验证（平板无需配置，但续期依赖联网）。**阻塞认证切片**。
- **Q11 店长复核的频率与形式**：复核范围见 [SOP](sop.md)「复核」；频率，以及用纸质签字还是在软件中留复核记录，待定。软件中留记录需新增事件类型。
