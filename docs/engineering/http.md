# HTTP 与写命令共同规则

业务写接口、handler、幂等或确认处理必读。客户端提交与恢复另见 [business/command-submission.md](../business/command-submission.md)；稳定测试入口见 [interfaces/app.md](../interfaces/app.md)。

## HTTP 约定

- 业务接口统一前缀 `/api/v1/`；`GET /health` 例外。
- 节点对局域网只以 HTTPS 服务，绑定有线网口的固定 IP，并由部署的主机入站防火墙限制为仅有线业务网口可访问；明文 HTTP 只绑定本机回环地址，供门店机工作站使用，绑定其他地址时拒绝启动，`Host` 的主机部分不是 `localhost` 或 `workstation_addr` 的 IP 字面量（IPv6 带方括号）的请求返回 `400 VALIDATION_FAILED`；release 构建未配置 `[tls]` 时拒绝启动。细则见 [docs/operations/tls.md](../operations/tls.md)。
- 所有响应都是同一信封（`boh-app/src/http.rs`），`warnings` 总是存在，没有警告时为空数组：

```json
{ "success": true,  "data": { },  "warnings": [ { "code": "EXPIRES_BEFORE_OLDER_STOCK", "message": "...", "details": { } } ], "error": null }
{ "success": false, "data": null, "warnings": [], "error": { "code": "IDEMPOTENCY_CONFLICT", "message": "...", "details": { } } }
```

- 警告不改变 `success`。警告码见 [异常分级](#异常分级)：`ABSORBED_BY_COUNT`、`EXPIRES_BEFORE_OLDER_STOCK`。
- 错误对象包含 `code`、`message`、`details`；`details` 总是存在，没有明细时为 `{}`。
- HTTP 状态码要和语义一致：校验失败 400、不存在 404、幂等冲突与业务冲突 409、内部错误 500、节点时钟未校时 503。`code` 是稳定的大写蛇形字符串，客户端按 `code` 判断，不按 `message`。
- 数量和换算系数一律用小数字符串，不用 JSON 数字；金额用 JSON 整数（分）。见 [数量](../business/inventory.md#数量)。
- 通用错误码：结构或取值错误 `400 VALIDATION_FAILED`；引用的实体不存在 `404 REFERENCE_NOT_FOUND`。
- 需要员工确认：`409 CONFIRMATION_REQUIRED`，`details` 为 `{"confirmations": [...]}`，结构见 [异常分级](#异常分级)。
- 其他通用错误码：未匹配的路由 `404 ROUTE_NOT_FOUND`；方法不允许 `405 METHOD_NOT_ALLOWED`；内部错误 `500 INTERNAL_ERROR`；缺少或无效的身份 `401 UNAUTHENTICATED`；角色不够 `403 FORBIDDEN`。
- 身份与权限通过后，请求体不是合法 JSON、缺少 `Content-Type: application/json`、字段缺失、未知字段、类型或取值不对，以及路径参数无法解析，一律 `400 VALIDATION_FAILED`。
  - axum 提取器默认返回纯文本的 415 / 422，必须转换成信封。
- 配置 TLS 的局域网请求先完成 TLS 握手与证书验证，再进入下述 HTTP 处理；握手失败由客户端处理 TLS 错误，不产生 HTTP 错误信封，错误与日志分层见 [请求顺序与错误](../operations/tls.md#请求顺序与错误)。
- 写命令按以下顺序处理，先命中的结果生效：身份与权限 → 请求结构与取值（反序列化成强类型命令）→ 幂等检查 → 节点校时信号 → 业务校验。被前两步拒绝的请求不做幂等比对。
- 写命令成功一律 `200`，重试返回的原响应也是 `200`。
- 需要人工处理后重提的错误码，由前端 App 底层按 [前端错误处理](../business/command-submission.md#前端错误处理)的对照表统一显示提示页和处理动作，处理后以同一个 `command_id` 重提；各业务页面不各自处理。
- `409 IDEMPOTENCY_CONFLICT` 的 `details` 是 `{"fields": [...]}`：`command_type` 不同时只列 `"command_type"`；否则列出规范化请求中取值不同或只在一方出现的顶层字段名，按字典序排列。规范化请求包含路径参数（如设备 ID）。
- 内部错误只记日志，不把 SQL / 内部细节返回给客户端。

## 幂等

- 每个写命令必须带客户端生成的 `command_id`（UUIDv7）。超时或结果未知时，客户端**必须用原 ID、原内容重试**。
- 规范化请求：把命令反序列化成强类型结构体再序列化，剥离 `command_id` 和 `clock_synced_at`，按 [敏感命令的幂等](../business/auth.md#敏感命令的幂等)处理敏感字段后存进 `processed_commands.request`。
  普通业务命令只剥离这两个字段（`occurred_at` 是业务内容）。比对 `command_type` + 规范化请求（文本，不用哈希）；所有含 PIN 或 PIN 重置授权秘密的命令（含员工主数据权限提升）适用 [敏感命令的幂等](../business/auth.md#敏感命令的幂等)的例外。
- **幂等检查先于任何业务校验**。
  - 同一 `command_id` + 同一 `command_type` + 同一规范化请求 → 直接返回保存的响应（含 warnings），不重复执行。
  - 同一 `command_id`，但 `command_type` 或规范化请求不同 → `409 IDEMPOTENCY_CONFLICT`，响应中给出差异字段。
- 只有成功的命令写入 `processed_commands`。被业务校验拒绝的命令不落库，修正后可以用同一个 ID 重提。
- 命令和事件结构体禁止 `HashMap` / `HashSet`，只用 `Vec` / `BTreeMap`。

## 写命令通用规则

适用于以下写命令：主数据、温度记录、收货、报损。报损预检是 `POST`，但不是写命令：没有 `command_id`，没有幂等处理（见[「报损接口」](../business/operations.md#报损接口)预检）。
处理顺序见 [HTTP 约定](#http-约定)：身份与权限 → 请求结构与取值 → 幂等检查 → 节点校时信号 → 业务校验。各接口只写自己的取值、业务校验和例外。

- **取值**：不满足时 `400 VALIDATION_FAILED`，`details` 为 `{}`。
- **新建**：服务端生成 UUIDv7 作为聚合 ID，写一条 `aggregate_version = 1` 的事件。
- **发生时间**：
  - 温度记录、收货、报损带 `occurred_at`、`clock_synced_at`，按[「时间」](../business/time.md#时间)的[「发生时间的校验」](../business/time.md#发生时间的校验)校验后原样记录，`business_date` 由 `occurred_at` 计算。
    - 提交的 `clock_synced_at` 原样记录在每条事件的 `store_events.clock_synced_at` 中（见[「时间」](../business/time.md#时间)）；它不进入规范化请求，重试返回首次响应，所以记录的是首次成功提交时的值。
    - 发生时间的校验和营业日计算属于业务校验，在幂等检查之后执行。
      理由：`recorded_at` 在写事务内才确定。重试即使带着已过期的 `clock_synced_at`，也返回首次响应，不重新校验。
  - 主数据命令不带 `occurred_at` / `clock_synced_at`：`occurred_at = recorded_at`，`business_date` 由它计算。
- **业务校验**（幂等检查之后）：被拒绝时不写事件或 `processed_commands`。同时命中多个业务错误时，返回哪一个不作规定；需要确认的项在其余业务校验都通过之后判定（见「异常分级」），接口另外规定了判定先后的也除外。
- **引用**：引用的实体不存在时 `404 REFERENCE_NOT_FOUND`，`details` 为 `{"entity": <被引用的实体>, "id"}`，接口另有规定的除外。停用的照常受理。
- **行下标**：带 `lines` 的命令，错误和警告 `details` 中的 `line` 是行在 `lines` 中的下标。

## 异常分级

分级固定写在本节，不做运行时配置。级别：

| 级别 | 含义 |
|---|---|
| 拒绝 | 整条命令不入账；确认不能放行，按错误修改内容后重提 |
| 需要确认 | 整条命令先不入账，返回 `409 CONFIRMATION_REQUIRED`；员工逐项确认后以同一个 `command_id` 重提，才入账 |
| 警告 | 命令照常入账，随成功响应的 `warnings` 返回 |

**原则**：

- 一码一级：每个码只有一个级别。命令需要不同的处置时，起一个新码。
  - 理由：前端按 `code` 选处理器；同一码在不同命令中级别不同，前端就要再按命令分支。
- 需要员工确认的原因共用顶层码 `409 CONFIRMATION_REQUIRED`，具体原因码放在 `details.confirmations` 中。以后出现别的类别时，各起自己的顶层码。
- 需要确认的异常，其触发条件必须能从 payload 判定；判定不了的，必须把判定依据写进 payload。确认本身不写进 payload，执行人由 `store_events.actor_id` 记录。
  - 例：报损事件记录了 `qty`、`item_book_qty`、`lot_book_qty`，满足不足条件又没被吸收的行必然经过了确认。疑似重复判定不了，所以写 `possible_duplicate_of`（见[「时间」](../business/time.md#时间)补录）。

**分级表**：

| 码 | 级别 | 触发条件 | 适用命令 | 实现状态 |
|---|---|---|---|---|
| `STOCK_SHORTFALL` | 需要确认 | 报损：未被吸收的行数量超过账面（见[「报损接口」](../business/operations.md#报损接口)逐行处理）；纠错：扣回时批次余量不够（见[「纠错」](../business/operations.md#纠错)） | 报损、纠错 | 报损已实现；纠错随纠错切片 |
| `POSSIBLE_DUPLICATE` | 需要确认 | 补录行与已入账事件疑似重复（见[「时间」](../business/time.md#时间)补录） | 补录的收货、报损、温度记录；生产随生产切片 | 随补录入口切片 |
| `ITEM_HAS_NO_LOTS` | 拒绝 | 报损的物料还没有任何批次 | 报损 | 已实现 |
| `COUNT_REQUIRED` | 拒绝 | 纠错的退回会使账外缺口大于 0 | 纠错 | 随纠错切片 |
| `STOCK_LIMIT_EXCEEDED` | 拒绝 | 超出库存状态上限（见[「数量」](../business/inventory.md#数量)） | 收货、报损；生产、盘点、纠错随各自切片 | 收货、报损已实现 |
| `LOT_SERIAL_EXHAUSTED` | 拒绝 | 批次流水号超过 `999`（见[「批次」](../business/inventory.md#批次)） | 收货；生产随生产切片 | 收货已实现 |
| `INVALID_LOT_DATES` | 拒绝 | 收货的生产日期、到期日与收货时间矛盾 | 收货 | 已实现 |
| `UNIT_CONVERSION_CHANGED` | 拒绝 | 录入时的换算系数与当前值不相等（见[「单位」](../business/inventory.md#单位)） | 带 `input` 的命令 | 已实现 |
| `EXPIRES_BEFORE_OLDER_STOCK` | 警告 | 新批次比先被扣的批次先到期（见[「批次」](../business/inventory.md#批次)） | 收货；生产随生产切片 | 收货已实现 |
| `ABSORBED_BY_COUNT` | 警告 | 行被盘点吸收（见[「盘点吸收」](../business/operations.md#盘点吸收)） | 报损、生产、纠错 | 随盘点切片 |
| 其余错误码（取值、引用、校时、发生时间、幂等、版本冲突等） | 拒绝 | 见各自章节 | — | — |

- 生产的用料不足、无批次用料、产出到期，盘点的差异提醒：级别随生产、盘点切片定。

**确认**：

- 确认写在每一行内：`acknowledged: [码…]`，码是该行要确认的原因。没有行的命令把 `acknowledged` 写在命令顶层，409 明细项不带 `line`（随补录入口切片，温度记录）。
- 取值（在幂等检查之前，不满足时 `400 VALIDATION_FAILED`）：
  - `acknowledged` 可以省略，不接受 `null`；码必须属于该命令的可确认集合（如实时报损只有 `STOCK_SHORTFALL`），同一行内不重复。
  - 规范化请求中按码的字节序排序，空数组与省略等价（省略该键）。
- 确认的是「该行的实际情况确实如此」（如报损数量确实发生），不绑定预检或上一次 409 时的账面：带确认的行按提交时的状态入账，异常程度比提示时更大也有效。
  - 不需要确认的行带了确认照常受理，确认保留在规范化请求中。被盘点吸收的行忽略确认。
- 判定时机：在其余业务校验都通过之后判定，其他业务错误先返回。需要确认而没带确认的行，照确认后的结果继续试算后续各行；全部行处理完后，一次列出所有未确认的项。不部分入账。
- 409 明细：`details` 为 `{"confirmations": [{line, code, details}]}`，按 `line` 升序、同一行内按 `code` 的字节序升序。各原因码的 `details` 见所在章节；数量用规范形式的小数字符串（见[「数量」](../business/inventory.md#数量)）。
- 不要求每个可确认的命令都有预检：409 本身就是提示。有预检的命令，预检的 `data.confirmations` 与 409 明细结构相同。
- 前端的处理见[「前端错误处理」](../business/command-submission.md#前端错误处理)：修改某行的物料、批次或数量后清除该行的确认，不自动确认。

健康报告的字段、问题清单与实现边界见 [GET /health](../operations/runtime.md#健康状态get-health)。
