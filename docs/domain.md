# BOH 领域模型

本文件保留业务总览、事件目录、实现边界与待确认问题；完整业务规则按下表进入专题。工程规则见 [AGENTS.md](../AGENTS.md)，门店操作规范见 [operations/sop.md](operations/sop.md)，规范用词见 [glossary.md](glossary.md)，员工认证见 [business/auth.md](business/auth.md)。
实现落地后，字段与表结构以代码为准（`boh-domain` 结构体、迁移 SQL），业务文档只保留语义与不变量。
[待确认问题](#待确认问题)中的条目，确认前不要实现依赖它的部分。

## 阅读导航

本索引及其专题同时包含已实现功能和后续切片的已定规则；实现范围见各节的状态说明、[事件目录](#事件目录)与 [路线图](../AGENTS.md#路线图)。

| 要查的内容 | 章节 |
|---|---|
| 业务决策与主数据归属 | [已确认的业务决策](#已确认的业务决策)、[主数据](business/master-data.md#主数据) |
| 时间可信度、发生时间、营业日与补录 | [校时信号](business/time.md#校时信号)、[门店节点校时](business/time.md#门店节点校时)、[前端校时](business/time.md#前端校时)、[发生时间校验](business/time.md#发生时间的校验)、[补录](business/time.md#补录)、[通用规则](business/time.md#通用规则) |
| 数量表示、单位换算与批次分配 | [数量](business/inventory.md#数量)、[单位](business/inventory.md#单位)、[批次](business/inventory.md#批次) |
| 生产、盘点、吸收、纠错与销售 | [生产规则](business/operations.md#生产规则)、[盘点](business/operations.md#盘点)、[盘点吸收](business/operations.md#盘点吸收)、[纠错](business/operations.md#纠错)、[销售导入](business/operations.md#销售导入) |
| 事件结构、投影语义与不变量 | [事件目录](#事件目录)、[投影表](business/inventory.md#投影表) |
| 拒绝、确认、警告与客户端重试 | [异常分级](engineering/http.md#异常分级)、[命令提交与结果未知](business/command-submission.md#命令提交与结果未知)、[前端错误处理](business/command-submission.md#前端错误处理) |
| 写命令与查询接口 | [写命令通用规则](engineering/http.md#写命令通用规则)、[节点时钟](business/time.md#节点时钟接口)、[主数据](business/master-data.md#主数据接口)、[温度记录](business/operations.md#温度记录接口)、[收货](business/operations.md#收货接口)、[报损](business/operations.md#报损接口)、[库存明细](business/inventory.md#库存明细查询)、[批次查询](business/inventory.md#批次查询) |
| 验收依据与功能边界 | [验收用例](business/operations.md#验收用例)、[已知限制](#已知限制)、[未来功能](#未来功能本期不实现)、[待确认问题](#待确认问题) |

## 已确认的业务决策

| # | 主题 | 结论 | 详见 |
|---|---|---|---|
| 1 | 销售 | 每晚导入 POS 导出的 CSV / Excel，只进报表，不扣库存 | [销售导入](business/operations.md#销售导入) |
| 2 | 批次 | 全部物料按批次追踪；批次号 `lot_id` 人可读、印在标签上；未指定批次的扣减按批次号中的批次日期、流水号 FIFO 分配 | [批次](business/inventory.md#批次) |
| 3 | 单位 | 多单位，账本只存基本单位；录入到提交之间换算系数变了，命令返回 409 由人工确认 | [单位](business/inventory.md#单位) |
| 4 | 生产扣料 | 按配方自动扣，员工可改实际用量；生产界面记录开始时间 | [生产规则](business/operations.md#生产规则) |
| 5 | 盘点粒度 | 按物料下的各批次逐行清点数量，只调整已有批次，不新建批次；库存明细界面同样按批次逐行显示 | [盘点](business/operations.md#盘点) |
| 6 | 盘点后的影响 | 实物发生在盘点之前、入账在盘点之后的扣减，以及对盘点时已存在批次的纠错，被那次盘点吸收：不动库存，只更正报表。新建批次的收货行、生产产出本身不吸收 | [盘点吸收](business/operations.md#盘点吸收) |
| 7 | 补录 | 允许通过补录入口填写过去的发生时间 | [补录](business/time.md#补录) |
| 8 | 录错 | 只有数量错用数量更正（保持原批次，差额可使原批次变负）；其他错误整条冲销 | [纠错](business/operations.md#纠错) |
| 9 | 账面不足 | 报损先提示，员工确认实际数量后入账：指定批次的差额留在该批次（余量可为负），未指定批次的差额记入账外缺口；下次盘点处理。纠错扣回不足同样需要确认，见[「纠错」](business/operations.md#纠错) | [批次](business/inventory.md#批次)、[纠错](business/operations.md#纠错)、[异常分级](engineering/http.md#异常分级) |
| 10 | 营业日 | 时区 `Asia/Shanghai`，日切 `04:00` | [通用规则](business/time.md#通用规则) |
| 11 | 操作人 | 员工在已注册的平板上用 PIN 登录；平板安装时注册一次；会话按班次有效 | [business/auth.md](business/auth.md)、[待确认问题](#待确认问题) |
| 12 | 总部与主数据归属 | 总部本期不开发，第一版只有一家门店。全部主数据属于连锁，`code` 和 UUID 全连锁统一；第一版由店长在门店节点维护，接入总部后由总部经主数据包下发，门店只能提交修改申请 | [主数据](business/master-data.md#主数据) |
| 13 | 不存在的业务 | 调拨、套餐、半批生产本期都不存在 | [未来功能](#未来功能本期不实现) |
| 14 | 数量 | 最多 3 位小数；每次录入不超过 9999.999，库存状态不超过 10^9 个基本单位；换算系数最多 9 位小数，换算结果四舍五入到 0.001 基本单位 | [数量](business/inventory.md#数量)、[单位](business/inventory.md#单位) |
| 15 | 提交与结果未知 | 平板连不上门店节点时不排队：未发出的转纸面或换平板重录；结果未知的转入后台任务，用原 `command_id` 重试；需要人处理的到工作站按任务编号核对，先在平板取消，再补录 | [命令提交与结果未知](business/command-submission.md#命令提交与结果未知) |

## 事件目录

数量是基本单位的定点小数，payload 中为小数字符串（见[「数量」](business/inventory.md#数量)）；金额是整数分。`schema_version` 从 1 开始。
`actor_id`、`device_id`、`occurred_at`、`business_date` 存在 `store_events` 的列中，不重复写进 payload。
`input` 见[「单位」](business/inventory.md#单位)，`alloc` 和批次号 `lot_id` 见[「批次」](business/inventory.md#批次)。新建批次的行带新批次号，不会被吸收；扣减库存的行（盘点行除外）要么带 `alloc`，要么带 `absorbed_by_event_id`（被吸收，见[「盘点吸收」](business/operations.md#盘点吸收)），二者取一。
带 `?` 的字段可选：缺省时省略该键，payload 中不出现 `null`。字段之间的 `a | b` 表示二者恰有一个出现；枚举值写作（`A` / `B`）。

| event_type | aggregate_type | payload 要点 | 投影影响 | 实现状态 |
|---|---|---|---|---|
| `GOODS_RECEIVED` | `RECEIPT` | `@2`：`boh_domain::receiving::GoodsReceived`；样本 `crates/boh-app/tests/golden/GOODS_RECEIVED@2/`；`lot_id` 是批次号。`@1`（`lot_id` 为 UUID）不支持，见[「收货接口」](business/operations.md#收货接口) | 每行新建一个批次 | 已实现 |
| `PRODUCTION_BATCH_COMPLETED` | `PRODUCTION_BATCH` | `recipe_id`, `recipe_version`, `batch_count`, `started_at?`, `output{item_id, planned_qty, qty, lot_id, expires_at?}`, `consumed[{item_id, planned_qty, qty, alloc \| absorbed_by_event_id}]` | 原料按分配扣减（或被吸收）；成品新建批次 | 尚未实现，随生产切片实现 |
| `WASTE_LOGGED` | `WASTE_RECORD` | `boh_domain::waste::WasteLogged`；样本 `crates/boh-app/tests/golden/WASTE_LOGGED@1/`。`lot_id` 是员工指定的批次；`item_book_qty` 是提交时该行之前的物料净账面，指定批次的行另带该批次账面 `lot_book_qty`（与 `lot_id` 同时出现）。见[「报损接口」](business/operations.md#报损接口) | 按分配扣减（或被吸收） | 部分实现：写入路径的吸收判定随盘点切片实现 |
| `STOCK_COUNT_SUBMITTED` | `STOCK_COUNT` | `purpose`（`CLOSING` / `AUDIT`）, `lines[{item_id, lot_id, counted_qty, input}]` | 无 | 尚未实现，随盘点切片实现 |
| `STOCK_ADJUSTED` | `STOCK_COUNT` | `lines[{item_id, lot_id?, book_qty, counted_qty, delta}]`：带 `lot_id` 的是批次行，不带的是每个物料一行的账外缺口清零 | 批次和账外缺口按 delta 变化；写 `inventory_counts` | 尚未实现，随盘点切片实现 |
| `PURCHASE_ORDER_SUBMITTED` | `PURCHASE_ORDER` | `supplier_id`, `lines[{item_id, qty, input}]`, `deliver_on`（门店当地日期 `'YYYY-MM-DD'`） | 无 | 尚未实现，未排期 |
| `TEMPERATURE_LOGGED` | `TEMPERATURE_READING` | `boh_domain::temperature::TemperatureLogged`；样本 `crates/boh-app/tests/golden/TEMPERATURE_LOGGED@1/` | 写 `temperature_readings` | 已实现 |
| `QUANTITY_CORRECTED` | 与原事件相同 | `corrected_event_id`, `reason`, `lines[{line_ref, item_id, physical_at, old_qty, new_qty, delta, input?, line_cost_cents?, alloc \| absorbed_by_event_id}]` | 按差额调整（或被吸收），见[「纠错」](business/operations.md#纠错) | 尚未实现，随纠错切片实现 |
| `EVENT_REVERSED` | 与原事件相同 | `reversed_event_id`, `reason`, `lines[{line_ref, item_id, physical_at, qty, alloc \| absorbed_by_event_id}]` | 精确取反（或被吸收），见[「纠错」](business/operations.md#纠错) | 尚未实现，随纠错切片实现 |
| `SALES_IMPORTED` | `SALES_DAY` | `source`（`CSV` / `XLSX`）, `file_name`, `lines[{item_id, qty, amount_cents}]`, `ignored_rows`（整数） | 覆盖 `daily_sales` 中该营业日；**不写库存流水** | 尚未实现，随销售导入切片实现 |
| `MASTER_DATA_CHANGED` | 实体名 | `entity`, `source`, `snapshot` | 覆盖对应的主数据表 | 部分实现：`EQUIPMENT`、`ITEM`、`RECIPE`、`SUPPLIER`、`WASTE_REASON` 的 `source = LOCAL` 写入已实现；`EMPLOYEE` 随认证切片实现，`HQ_PACKAGE` 随下行同步实现 |

- `reason_code` 的值对应 `WASTE_REASON` 的 `code`。
- 温度记录每条读数一个新的 `TEMPERATURE_READING` 聚合。
- 一次盘点的 `STOCK_COUNT_SUBMITTED`、`STOCK_ADJUSTED` 属于同一个 `STOCK_COUNT` 聚合，version 分别为 1、2。
- **可纠错事件的聚合版本**：原事件是 version 1，每次数量更正 +1，冲销是该聚合的最后一个版本。

## 已知限制

按任务读取相应边界：[时间与状态文件故障](business/time.md#已知限制)、[批次与数量](business/inventory.md#已知限制)、[盘点期间的实物变动](business/operations.md#已知限制)、[断连与本地任务丢失](business/command-submission.md#已知限制)。

## 未来功能（本期不实现）

- **门店调拨**：`TRANSFER_SENT` / `TRANSFER_RECEIVED`，含在途归属规则。
- **总部同步**：上行契约见 [上行同步](../AGENTS.md#上行同步只定契约本期不实现)；下行为主数据包导入（`source = HQ_PACKAGE`）。总部从事件自行计算报表，必须区分被吸收的行和正常生效的行。总部关账后可下发锁账日（见 Q6）。
- **采购对接**：采购单与收货的关联、在途库存。
- **批次标签打印**：标签版式、打印机接入和扫码录入延后；批次号与打印契约见[「批次」](business/inventory.md#批次)。

## 待确认问题

- **Q6 锁账日**：默认方案是店长在管理界面设置锁账日，`business_date` 不晚于它的补录和纠错返回 `409 BOOKS_LOCKED`；另外默认最多补录 7 天以内。**阻塞补录入口**。
- **Q7 会话的「班次」如何结束**：默认方案是员工主动登出，或登录满 12 小时。**阻塞认证切片**。到期前 30 分钟提醒；到期后后台任务保留，重试得 `401` 转需处理，由店长在工作站处理；登出时有未完成后台任务的提示见[「登出与会话」](business/command-submission.md#7-登出与会话)。
- **Q11 店长复核的频率与形式**：复核范围见 [复核](operations/sop.md#复核)；频率，以及用纸质签字还是在软件中留复核记录，待定。软件中留记录需新增事件类型。

## 专题入口

以下原章节入口仅作导航；规则正文在链接目标。

### 时间

见 [时间](business/time.md#时间)。

### 分工

见 [分工](business/time.md#分工)。

### 校时信号

见 [校时信号](business/time.md#校时信号)。

### 门店节点校时

见 [门店节点校时](business/time.md#门店节点校时)。

### 前端校时

见 [前端校时](business/time.md#前端校时)。

### 发生时间的校验

见 [发生时间的校验](business/time.md#发生时间的校验)。

### 补录

见 [补录](business/time.md#补录)。

### 真实性

见 [真实性](business/time.md#真实性)。

### 通用规则

见 [通用规则](business/time.md#通用规则)。

### 主数据

见 [主数据](business/master-data.md#主数据)。

### 数量

见 [数量](business/inventory.md#数量)。

### 单位

见 [单位](business/inventory.md#单位)。

### 批次

见 [批次](business/inventory.md#批次)。

### 生产规则

见 [生产规则](business/operations.md#生产规则)。

### 盘点

见 [盘点](business/operations.md#盘点)。

### 盘点吸收

见 [盘点吸收](business/operations.md#盘点吸收)。

### 纠错

见 [纠错](business/operations.md#纠错)。

### 销售导入

见 [销售导入](business/operations.md#销售导入)。

### 投影表

见 [投影表](business/inventory.md#投影表)。

### 异常分级

见 [异常分级](engineering/http.md#异常分级)。

### 写命令通用规则

见 [写命令通用规则](engineering/http.md#写命令通用规则)。

### 命令提交与结果未知

见 [命令提交与结果未知](business/command-submission.md#命令提交与结果未知)。

### 1. 提交状态（App）

见 [1. 提交状态（App）](business/command-submission.md#1-提交状态app)。

### 2. 后台任务（App）

见 [2. 后台任务（App）](business/command-submission.md#2-后台任务app)。

### 3. 参数

见 [3. 参数](business/command-submission.md#3-参数)。

### 4. 后台重试收到的结果

见 [4. 后台重试收到的结果](business/command-submission.md#4-后台重试收到的结果)。

### 5. 例外

见 [5. 例外](business/command-submission.md#5-例外)。

### 6. 工作站核对

见 [6. 工作站核对](business/command-submission.md#6-工作站核对)。

### 7. 登出与会话

见 [7. 登出与会话](business/command-submission.md#7-登出与会话)。

### 8. 节点侧：按编号查询与取消日志

见 [8. 节点侧：按编号查询与取消日志](business/command-submission.md#8-节点侧按编号查询与取消日志)。

### 9. 系统中断登记本

见 [9. 系统中断登记本](business/command-submission.md#9-系统中断登记本)。

### 10. 检查点

见 [10. 检查点](business/command-submission.md#10-检查点)。

### 11. 部署要求

见 [11. 部署要求](business/command-submission.md#11-部署要求)。

### 节点时钟接口

见 [节点时钟接口](business/time.md#节点时钟接口)。

### 前端错误处理

见 [前端错误处理](business/command-submission.md#前端错误处理)。

### 主数据接口

见 [主数据接口](business/master-data.md#主数据接口)。

### 温度记录接口

见 [温度记录接口](business/operations.md#温度记录接口)。

### 收货接口

见 [收货接口](business/operations.md#收货接口)。

### 报损接口

见 [报损接口](business/operations.md#报损接口)。

### 库存明细查询

见 [库存明细查询](business/inventory.md#库存明细查询)。

### 批次查询

见 [批次查询](business/inventory.md#批次查询)。

### 验收用例

见 [验收用例](business/operations.md#验收用例)。

### 已锁定

见 [已锁定](business/operations.md#已锁定)。

### 待实现

见 [待实现](business/operations.md#待实现)。

## 接口

[共同规则](engineering/http.md#写命令通用规则) · [主数据](business/master-data.md#主数据接口) · [业务操作](business/operations.md) · [库存查询](business/inventory.md#库存明细查询) · [时钟](business/time.md#节点时钟接口)。

## 尚未设计（动手前先讨论）

- 前端原生 App：技术栈（Android / iOS 各自开发或跨平台）、安装分发与版本更新、设备管理（MDM）的选型与设备注册流程、后台任务与盘点草稿的本地存储，断网、重试、恢复与取消的客户端日志；时间规则已定，见 [前端校时](business/time.md#前端校时)；提交状态、后台任务、参数、登出提示已定，见 [命令提交与结果未知](business/command-submission.md#命令提交与结果未知)。
- 工作站网页前端：技术栈、构建方式、静态文件编进二进制的方式、登录与入口；新增依赖须先登记技术栈表并经人批准。传输已定：经本机回环地址的明文 HTTP（见 [监听](operations/tls.md#监听)）。
- 下行同步：主数据包的格式、拉取与导入流程（原则已定：总部维护统一的 JSON 主数据包，逐行比对快照，`source = HQ_PACKAGE`；全部主数据归总部所有，见 [主数据](business/master-data.md#主数据)）。**总部本期不开发，延后。**
  - 设计时一并定：门店修改申请的流程；按门店下发不同物料时，下发给某店的包内引用必须完整（配方用到的物料都在包内）；物料撤出某店的下发范围时该店只停用、不删除；新门店初始化、首次主数据包导入与首位店长本地 PIN 设定的衔接（包括如何满足 `LAST_ACTIVE_MANAGER` 约束）；员工调动后在新门店的登录（认证状态不同步，见 [docs/business/auth.md](business/auth.md)）。
- 上行同步的实现：总部接收接口、批量大小、mTLS 证书下发与轮换、失败的重试分类（哪些错误可重试）。**延后**；契约见[「上行同步」](../AGENTS.md#上行同步只定契约本期不实现)。信任体系沿用[证书层级](operations/tls.md#证书层级)中的连锁根，客户端证书用单独的签发 CA 和私钥。
- 异地备份复制的实现。
- 门店节点恢复与业务连续性：恢复入口与操作步骤（方向：数据库备份 + 纸面业务凭据 + 人工补录 + 盘点核对），覆盖收货、生产、报损、盘点、幂等重试，以及恢复后的上行同步（恢复到旧备份会让 `seq` 回退，与总部已接收的事件冲突）。恢复后不得重用已贴在货物上的批次号。原则已定：恢复时清空会话、PIN 重置授权、设备注册、设备解锁码、锁定状态与失败计数，平板重新注册；员工 PIN 哈希保留（认证状态表见[员工主数据](business/auth.md#员工主数据)）。认证幂等记录不得绕过恢复后的设备注册校验。**延后**；恢复演练测试照常要求。

---
