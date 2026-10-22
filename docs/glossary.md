# 术语表

本项目文档的规范用词。每个词只给一句话定义，规则以「详见」指向的章节为准。
英文名是规范名，正文可以用中文用词；同一个概念不要换别的说法。
涉及写命令的术语还须读取[共同提交契约](business/command-submission.md#命令提交与结果未知)，包括其中的业务例外；下表的具体章节链接只用于定位定义。

## 开发流程

| 规范名 | 中文用词 | 定义 | 详见 |
|---|---|---|---|
| Vertical slice | 纵向切片，简称切片 | 一次交付的完整功能增量，贯穿迁移、事件、校验、投影、HTTP 接口和锁定测试。它是开发的拆分方式，不是代码架构：本项目不采用 Vertical Slice Architecture，crate 仍按层横向划分 | [开发顺序](engineering/development.md#开发顺序每个新功能) |
| Walking skeleton | — | 第一个 vertical slice（设备主数据）。业务最简单，用来让一条真实命令完整走通 write path、幂等、重放和 golden payload；之后的切片照它的模式写 | [路线图](../AGENTS.md#路线图) |
| Locked test | 锁定测试 | 由 Claude 编写、人确认预期值的测试，实现 agent 不得修改或使其失效 | [测试分工](engineering/development.md#测试分工) |
| Locked path | 锁定路径 | `.github/CODEOWNERS` 列出的路径，只能出现在 `spec:` / `docs:` 提交中 | [协作分工](engineering/development.md#协作分工) |
| Review baseline | 比对基准 | 切片分支开头连续的 `spec:` / `docs:` 提交中的最后一个；review 时锁定路径相对它的 diff 必须为空 | [测试分工](engineering/development.md#测试分工) |
| Golden payload | golden 样本 | 某个 `event_type@schema_version` 的一种 payload 结构分支的完整序列化 JSON，用来保证已发布的 payload 结构不变 | [测试分工](engineering/development.md#测试分工) |

## 架构与存储

| 规范名 | 中文用词 | 定义 | 详见 |
|---|---|---|---|
| Write path | 写入路径 | 所有写命令共用的链路，与具体业务无关：`Writer::call` → `BEGIN IMMEDIATE` → 幂等检查 → 节点校时信号 → 业务校验 → `Ledger::append`（写事件并更新投影）→ 写 `processed_commands` → `COMMIT` → 返回信封 | [架构](engineering/architecture.md#架构) |
| Single writer | 唯一写连接 | 整个节点只有 `sqlite-writer` 线程持有的一个写连接，所有写操作排队经过它 | [SQLite](engineering/storage.md#sqlite) |
| Ledger | 账本 | `store_events` 表：只追加的事件流，是全部业务状态的唯一来源 | [只追加](engineering/storage.md#只追加append-only) |
| `seq` | 提交顺序 | 账本中事件的连续序号，由 SQLite 分配；事件间的一切顺序只看它 | [ID 与时间](business/time.md#id-与时间) |
| Projection | 投影 | 由事件推导出的当前状态表（库存余量、主数据表等），在写事件的同一事务内更新，可以从账本完整重建 | [只追加](engineering/storage.md#只追加append-only) |
| Replay / rebuild | 重放 / 重建 | 清空投影后按 `seq` 把全部事件重新 `projections::apply` 一遍，结果必须与原投影逐行一致 | [只追加](engineering/storage.md#只追加append-only) |
| Aggregate | 聚合 | 一组相关事件共用的标识（`aggregate_type` + 聚合 ID），`aggregate_version` 给出组内顺序。只是事件上的字段，不是代码中的类层次 | [「事件目录」](domain.md#事件目录) |
| Upcast | upcast | 读取旧 `schema_version` 的 payload 时，把它转换成当前结构的函数；与单位换算无关 | [只追加](engineering/storage.md#只追加append-only) |
| Idempotency | 幂等 | 同一 `command_id` 重复提交时返回原结果、不重复执行；内容不同时返回 `409 IDEMPOTENCY_CONFLICT` | [幂等](engineering/http.md#幂等) |
| Canonical request | 规范化请求 | 命令反序列化成强类型结构体后再序列化、剥离 `command_id` 和 `clock_synced_at` 得到的文本，用于幂等比对；含 PIN 或授权秘密的命令另有例外，见 [敏感命令的幂等](business/auth.md#敏感命令的幂等) | [幂等](engineering/http.md#幂等) |
| Envelope | 信封 | 所有 HTTP 响应共用的外层结构：`success`、`data`、`warnings`、`error` | [HTTP 约定](engineering/http.md#http-约定) |
| Invariant self-check | 不变量自检 | 启动时和闭店时在读连接上检查投影是否满足不变量，`/health` 返回最近一次的结果 | [不变量自检](operations/invariant-check.md) |
| Chain root | 连锁根 | 局域网 HTTPS 信任链的起点，全连锁一个，私钥离线保存，根证书内置 App | [证书层级](operations/tls.md#证书层级) |
| Store issuing CA | 门店签发 CA | 每店一张、由连锁根签发、带名称约束的中间 CA，私钥在门店节点上，用来签发叶证书 | [证书层级](operations/tls.md#证书层级) |
| Leaf certificate | 叶证书 | 门店节点在 HTTPS 握手中出示的服务器证书，由节点用门店签发 CA 自动签发和续期 | [叶证书的签发与续期](operations/tls.md#叶证书的签发与续期) |
| Name constraints | 名称约束 | 门店签发 CA 证书中限制它能签哪些 DNS 名和 IP 的扩展 | [证书层级](operations/tls.md#证书层级) |
| Issue list | 问题清单 | `/health` 的 `issues`：节点当前存在的运维问题，每项有码、严重度 `ERROR` / `WARNING` 和开始时间；严重度与异常分级无关 | [健康状态](operations/runtime.md#健康状态get-health) |
| Deployment record | 部署记录 | 每店一份、不进仓库的记录：设备固定 IP、路由器网段与地址池、门店签发 CA 到期日等 | [部署记录模板](../deploy/deployment-record.template.md) |
| Sync cursor | 同步游标 | `sync_state.acked_seq`：总部已确认收到的最大 `seq`，上行同步从它之后推送 | [上行同步](../AGENTS.md#上行同步只定契约本期不实现) |

## 业务领域

| 规范名 | 中文用词 | 定义 | 详见 |
|---|---|---|---|
| Business date | 营业日 | 门店当地日期，早于日切时刻（必填配置 `business_day_cutoff`，如 `04:00`）的发生时间算前一营业日 | [「通用规则」](business/time.md#通用规则) |
| Clock sync signal | 校时信号 | 门店节点或前端最近一次校时成功的时间；`上次校时 ≤ 现在 ≤ 上次校时 + 7 天` 时有效，有效期内认为时间准确 | [「校时信号」](business/time.md#校时信号) |
| Backfill | 补录 | 经显式入口提交员工填写的过去的 `occurred_at`，不要求前端校时信号，不受 72 小时限制 | [「补录」](business/time.md#补录) |
| Fixed-point quantity | 定点数量 | 基本单位的数量，最多 3 位小数；存千分之一基本单位的整数，接口和 payload 中写成小数字符串 | [「数量」](business/inventory.md#数量) |
| Unit conversion | 单位换算 | 录入数量乘换算系数得到基本单位数量（`convert_unit`），四舍五入到 0.001 基本单位 | [「单位」](business/inventory.md#单位) |
| Entry limit | 录入上限 | 每次录入在所选单位下的数字绝对值不超过 9999.999 | [「数量」](business/inventory.md#数量) |
| Stock state limit | 库存状态上限 | 任何写入之后，物料净账面、批次余量、账外缺口的绝对值不超过 10^9 个基本单位 | [「数量」](business/inventory.md#数量) |
| Lot | 批次 | 一次收货行或生产产出形成的一份库存，以批次号标识 | [「批次」](business/inventory.md#批次) |
| Lot number | 批次号 | 批次的唯一标识 `lot_id`，格式 `类型-编码-YYYYMMDD-流水号`，印在标签上，人和程序都用它识别同一批实物；生成后不变 | [「批次」](business/inventory.md#批次) |
| Manufacturer lot number | 生产商批号 | 包装上由生产商印的批号（`manufacturer_lot_no`），可选，不作批次主键；不是供应商的批号 | [「批次」](business/inventory.md#批次) |
| Allocation | 分配 | 把一笔扣减落到具体批次（或账外缺口）的结果，写进 payload 的 `alloc`，重放时不重新计算 | [「批次」](business/inventory.md#批次) |
| Off-book shortfall | 账外缺口 | 未落到任何批次的账面不足：未指定批次的扣减超过全部正余量批次的部分，以及整条冲销扣回时批次不足的部分；下一次盘点该物料时清零 | [「批次」](business/inventory.md#批次)、[「纠错」](business/operations.md#纠错) |
| Precheck | 预检 | 正式提交报损前的只读试算：返回每行的账面、按当前账面预计的分配和需要确认的项，不写任何内容 | [「报损接口」](business/operations.md#报损接口) |
| Anomaly level | 异常分级 | 每个异常码固定的处置级别：拒绝、需要确认或警告，一码一级 | [「异常分级」](engineering/http.md#异常分级) |
| Confirmation | 确认 | 员工确认某行的异常（如账面不足、疑似重复）确实如此，允许入账；码写在该行的 `acknowledged` 中；不绑定预检时的账面或分配 | [「异常分级」](engineering/http.md#异常分级) |
| Possible duplicate | 疑似重复 | 补录内容与已入账记录重合，需要确认才入账 | [补录](business/time.md#补录) |
| Not sent | 未发出 | 连接没有建立，节点肯定没收到的提交 | [「提交状态」](business/command-submission.md#1-提交状态app) |
| Unknown result | 结果未知 | 连接建立之后出错或收到 `500` 的提交，节点可能已入账 | [「提交状态」](business/command-submission.md#1-提交状态app) |
| Background task | 后台任务 | App 中用原 `command_id` 重试结果未知命令的任务，状态为进行中、完成、需处理、已取消 | [「后台任务」](business/command-submission.md#2-后台任务app) |
| Cancelled task | 已取消 | 录入人或店长在平板上取消、或员工确认登出时取消的后台任务；App 不再发送它，取消记入节点日志，不进账本 | [「后台任务」](business/command-submission.md#2-后台任务app) |
| Task number | 任务编号 | 后台任务的 `command_id`，以二维码显示，在工作站核对时扫码使用 | [「后台任务」](business/command-submission.md#2-后台任务app) |
| Workstation | 工作站 | 门店机上由节点提供的网页前端，用于按任务编号核对和补录 | [「工作站核对」](business/command-submission.md#6-工作站核对)、[部署要求](business/command-submission.md#11-部署要求) |
| Count absorption | 盘点吸收 | 实物发生在某次盘点观察时点之前、却在盘点之后才入账的扣减（及对盘点时已存在批次的纠错），已被盘点数计入，因此不再改动库存，只更正报表 | [「盘点吸收」](business/operations.md#盘点吸收) |
| Reversal | 冲销 | 追加一条 `EVENT_REVERSED`，整条抵消原事件 | [「纠错」](business/operations.md#纠错) |
| Quantity correction | 数量更正 | 追加一条 `QUANTITY_CORRECTED`，只改数量并保持原批次 | [「纠错」](business/operations.md#纠错) |
