# 主数据与初始化

归属、快照语义与客户端接口在本文件。写入须同时读 [HTTP 与写命令共同规则](../engineering/http.md)、[时间](time.md#id-与时间)；有数量的主数据另读 [数量](inventory.md#数量) 和 [单位](inventory.md#单位)。员工与秘密信息另见 [business/auth.md](auth.md)。

## 主数据

| 实体（聚合类型） | 含义 | `snapshot` |
|---|---|---|
| `ITEM` | 物料，**含单位换算** | `boh_domain::master_data::ItemSnapshot` |
| `RECIPE` | 配方，**含全部版本**；版本只增不改 | `boh_domain::master_data::RecipeSnapshot` |
| `SUPPLIER` | 供应商 | `boh_domain::master_data::SupplierSnapshot` |
| `EMPLOYEE` | 员工，**不含凭据** | `code`, `name`, `role`（`STAFF` / `MANAGER`）, `active` |
| `WASTE_REASON` | 报损原因；初始化时预置 `EXPIRED`、`DAMAGED`、`PRODUCTION_DEFECT`、`TASTING`、`OTHER` | `boh_domain::master_data::WasteReasonSnapshot` |
| `EQUIPMENT` | 设备 | `boh_domain::equipment::EquipmentSnapshot` |

- **归属**：上表全部实体都属于连锁层面，UUID 和 `code` 全连锁统一，不按门店划分。
  - 第一版只有一家门店、没有总部：门店节点暂代主数据来源，由店长经主数据写接口创建和维护全部主数据，包括物料和供应商（`source = LOCAL`）。总部上线之前不开第二家门店。
  - 接入总部后：主数据全部由总部所有，只经主数据包进入门店（`source = HQ_PACKAGE`）；门店节点关闭主数据写接口，门店只能提交修改申请。总部以这家门店现有的主数据作为初始数据，UUID 和 `code` 不变。
  - 接入总部后，新门店的 `init` 不再创建预置报损原因或第一个店长的员工主数据，两者均由首次主数据包提供（`source = HQ_PACKAGE`），UUID 和 `code` 由总部统一分配。首位店长的本地 PIN 设定与初始化的衔接随下行同步设计，见 [尚未设计](../domain.md#尚未设计动手前先讨论)。
  - 接入总部后，门店不自建物料，不自找供应商；总部可以按区域、按门店下发不同的物料。员工调动由总部执行。
  - 理由：物料的 `code` 写进批次号、印在标签上，也用于销售导入，创建后不能修改；多门店各自创建主数据可能造成编码冲突。

- 已落地实体的样本在 `crates/boh-app/tests/golden/MASTER_DATA_CHANGED@1/`。

- 快照是该行变更后的完整内容，不是差量。行的主键是事件的 `aggregate_id`，`revision` 是 `aggregate_version`，都不重复写进快照。`active` 是布尔值。
- `name` 非空；`code` 非空，在同一实体内唯一。`contact_phone` 不做格式校验。`MANAGER` 是店长，`STAFF` 是普通员工。
- `ITEM` 的 `code` 只能由 `A`–`Z`、`0`–`9` 和 `_` 组成（本地编辑和总部包导入都一样）。
  理由：它是批次号的一段，要印在标签上并由人录入。
- `units` 不含基本单位：`unit_code` 等于 `base_unit` 时系数恒为 1。`units` 中的 `unit_code` 在同一物料内唯一、不等于 `base_unit`，按 `unit_code` 升序；系数规则见[「单位」](inventory.md#单位)。
- `versions` 按 `version` 升序，从 1 连续编号；每批产出和每行每批用量见[「主数据接口」](#主数据接口)配方；同一版本的 `lines` 中 `item_id` 不重复，顺序为录入顺序。
- 快照字段与枚举随 `MASTER_DATA_CHANGED@1` 冻结；结构变更按 [开发阶段与上线](../../AGENTS.md#开发阶段与上线)处理：上线前直接修改、不升 `schema_version`；上线后按[「只追加」](../engineering/storage.md#只追加append-only)升版本。

- 主键一律 UUIDv7。`code` 人可读、各门店统一（物料的 `code` 用于销售导入）。
- `code`、`ITEM` 的 `base_unit` 和 `RECIPE` 的 `output_item_id` 创建后不可修改；配方已有的版本不可修改，只能追加新版本。
  理由：账本数量按基本单位存；配方版本中的数量按产出物料和用料的基本单位解释，历史版本的产出物料也不能变。
- 每行带 `revision`，每次变更 +1。主数据只停用（`active = 0`），不删除。
- HTTP 修改命令携带客户端看到的 `base_revision`，与当前 `revision` 不等时返回 `409 REVISION_CONFLICT`（`details` 为 `{"current_revision": n}`），不写事件或 `processed_commands`。新建命令不带 `base_revision`；主数据包导入按快照逐行比对，不适用此规则。
- 命令引用已停用的主数据照常受理，停用只在界面上隐藏。
- 主数据写接口只允许 `MANAGER`；接入总部后关闭（见「归属」）。
- `EMPLOYEE` 的权限提升、最后一名店长和认证状态表见 [员工主数据](auth.md#员工主数据)。
- 每次变更写一条 `MASTER_DATA_CHANGED`：聚合类型是实体名，聚合 ID 是行的主键，`aggregate_version` 是变更后的 `revision`；payload 是 `{entity, source, snapshot}`，`source`（`LOCAL` / `HQ_PACKAGE`）。主数据表是投影，可以从事件流重建。
- 内容没有变化的变更不写事件（导入主数据包时逐行比对快照）。
- **系统操作人**：初始化和主数据包导入在没有登录员工时，`actor_id` 使用保留 ID `00000000-0000-7000-8000-000000000000`。
- **系统设备**：初始化和没有登录平板的主数据包导入，`device_id` 使用保留 ID `00000000-0000-7000-8000-000000000001`，表示产生事件的本门店节点。该身份不需要设备注册、设备令牌、解锁码或员工会话，不写入设备注册表；事件的 `device_id` 仍为非空 UUIDv7。
- 两个保留 ID 不得分配给真实员工或注册平板。系统身份只由节点内部的初始化和主数据包导入入口填写，表示事件来源，不授予 HTTP 访问权限。
- **初始化（第一版，接入总部前）**：`boh-server init` 在一个写事务中写入 `store_meta`、预置主数据和第一个店长账号（在终端输入两次 PIN），任一步失败则全部回滚，可以重新执行。第一个店长随认证切片加入；当前写入 `store_meta` 和预置报损原因。
  - 初始化是一个命令：`command_type` 为 `store.init`，`command_id` 由服务端按 [ID 与时间](time.md#id-与时间)生成；`processed_commands` 中的规范化请求为 `{"store_id": <配置的 store_id>}`，响应为 `{}`。
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

## 主数据接口

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
- 取值：`code`、`name` 非空，首尾不能有空白字符；`base_revision` 是正整数；其余字段见下文。
- **新建**：`code` 已被同一实体的其他行使用（含停用的）：`409 CODE_ALREADY_EXISTS`，`details` 为 `{"code", <ID 字段>}`（已占用该 `code` 的行）。
- **修改**：行不存在：`404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": <实体>, "id"}`。`base_revision` 规则见[「主数据」](#主数据)。
  - 新快照与当前快照相同时不写事件，命令照常成功并写入 `processed_commands`，响应为当前行。
- **引用**：引用其他主数据的字段必须指向已有的行（含停用的），规则见[「写命令通用规则」](../engineering/http.md#写命令通用规则)。

**设备**：请求体与行见 `boh_domain::equipment` 的 `CreateEquipment`、`UpdateEquipment`、`Equipment`。

**物料**：请求体与行见 `boh_domain::master_data` 的 `CreateItem`、`UpdateItem`、`Item`。

- `default_shelf_life_ms` 可以省略，出现时是正整数，不接受 `null`。修改时省略表示没有默认保质期。
- `units` 是数组，可以为空，不接受 `null`。每项 `{unit_code, base_qty_per_unit}`：`unit_code` 非空、首尾不能有空白字符、不等于 `base_unit`；`base_qty_per_unit` 见[「单位」](inventory.md#单位)换算系数。
- `units` 必须按 `unit_code` 的字节序严格升序（因此不重复），服务端不重新排序。
- 修改请求不带 `base_unit`，所以「`unit_code` 不等于 `base_unit`」在修改时是例外：在幂等检查之后、写事务内按当前 `base_unit` 核对，按业务校验处理。不满足时仍返回 `400 VALIDATION_FAILED`（`details` 为 `{}`），不写事件或 `processed_commands`；与行不存在、`REVISION_CONFLICT` 同时命中时返回哪一个不作规定。新建请求带 `base_unit`，这一项照常属于取值。
- 修改换算系数、增删单位照常受理，不影响已入账的事件；录入到提交之间换算系数变了的命令按[「单位」](inventory.md#单位)核对。

**配方**：

| 方法与路径 | `command_type` | 请求体（`boh_domain::master_data`） |
|---|---|---|
| `POST /api/v1/recipes` | `recipe.create` | `CreateRecipe` |
| `PUT /api/v1/recipes/{recipe_id}` | `recipe.update` | `UpdateRecipe` |
| `POST /api/v1/recipes/{recipe_id}/versions` | `recipe.add_version` | `AddRecipeVersion` |

- 行见 `boh_domain::master_data::Recipe`。

- 新建时每批产出和 `lines` 构成版本 1。
- **数量**：每批产出和每行的每批用量都用 `input` 提交（见[「单位」](inventory.md#单位)），按录入上限检查录入的数字，例如每批面粉录 `25` kg，不录 `25000` g。
  - 快照中同时保存换算后的基本单位数量（都大于 0）和 `input`，字段见 `boh_domain::master_data::RecipeSnapshot`。生产按基本单位数量计算。
  - 写入线程按当前换算核对产出物料和各行物料的单位（`UNKNOWN_UNIT`、`UNIT_CONVERSION_CHANGED`，见[「单位」](inventory.md#单位)）：行的 `details` 带 `line`（`lines` 下标），产出的不带。已保存的版本不受之后修改换算系数的影响。
- **追加版本**：新版本号是当前最大版本 + 1，写一条 `MASTER_DATA_CHANGED`，`revision` + 1。权限、`base_revision`、配方不存在和成功的 `data` 同「修改」。
  - 每批产出和 `lines`（含行顺序和 `input`）都与最新版本相同时，不新增版本、不写事件，`revision` 不变；命令照常成功并写入 `processed_commands`，响应为当前配方。只与最新版本比较：从 A 改为 B 再改回 A，仍新增版本；同一数量换一种单位录入（`2.5` kg 改为 `2500` g）也新增版本。
- `lines` 非空，每项是物料和 `input`，同一版本内 `item_id` 不重复，按提交顺序保存。
- `output_item_id` 和每个 `lines[].item_id` 引用 `ITEM`（规则见「引用」）。不限制物料的 `category`，不检查产出物料是否出现在自己的 `lines` 中。

**供应商**：请求体与行见 `boh_domain::master_data` 的 `CreateSupplier`、`UpdateSupplier`、`Supplier`。

- `contact_phone` 可以省略，出现时非空、首尾不能有空白字符，不接受 `null`。修改时省略表示删除联系电话。

**报损原因**：请求体与行见 `boh_domain::master_data` 的 `CreateWasteReason`、`UpdateWasteReason`、`WasteReason`。预置的报损原因同样可以修改和停用。
