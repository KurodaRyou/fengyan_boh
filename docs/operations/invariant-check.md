# 不变量自检

本文件是不变量自检的规则。硬规则摘要见 [AGENTS.md「不变量自检」](../../AGENTS.md#不变量自检)，`/health` 字段见 [健康状态](runtime.md#健康状态get-health)，被检查的不变量见 [投影表](../business/inventory.md#投影表)。

自检在运行中的节点上检查投影是否满足已定义的不变量。通过只说明本次执行的检查项成立，不代表投影与事件流完全一致；重放一致性由锁定测试保证。

## 检查项

按下表顺序执行，每项有固定的检查码。每项检查一组对象，对象的 `actual` 与 `expected` 不相等时，该对象计一条违规。

| 顺序 | 检查码 | 对象（`subject`） | 对象范围 | `actual` | `expected` |
|---|---|---|---|---|---|
| 1 | `INVENTORY_LOT_REMAINING` | `{ "item_id", "lot_id" }` | `inventory_lots` 的全部批次 | 该批次的 `remaining_qty` | 该 `lot_id` 全部流水的 `qty_delta` 之和，没有流水时为 0 |
| 2 | `INVENTORY_UNALLOCATED` | `{ "item_id" }` | `inventory_unallocated` 的全部物料，加上有 `lot_id IS NULL` 流水的物料 | 该物料的账外缺口；没有行时为 0 | 该物料 `lot_id IS NULL` 的流水的 `qty_delta` 之和，没有流水时为 0 |
| 3 | `INVENTORY_ON_HAND` | `{ "item_id" }` | 视图 `inventory_on_hand` 的全部行 | 视图中的 `qty` | 该物料批次 `remaining_qty` 之和加账外缺口（没有行时为 0） |

- `subject` 的字段是 JSON 字符串，取值见上表。第 1 项的 `item_id` 取该批次在 `inventory_lots` 中的 `item_id`。
- `actual` 与 `expected` 是基本单位的数量，按 [数量](../business/inventory.md#数量)写成小数字符串（存储值 `7000` 写作 `"7"`）。
- 对象范围不按物料是否停用过滤。
- 汇总只用 `qty_delta`，不用 `nominal_qty`：被吸收的行 `qty_delta = 0`，不改变账面。
- 批次余量为负是合法状态。物料有 `lot_id IS NULL` 的流水、合计为 0、没有账外缺口行，也是合法状态。
- 账外缺口 `<= 0` 由表约束保证；流水之和大于 0 时必然与账外缺口不等，按第 2 项报告，不另设检查码。
- 第 3 项目前由视图定义保证，照常执行。
- 任一汇总超出 `i64`：本次检查失败（`failed`），不按违规报告。

扩展：
- 检查项随切片增加，目标是覆盖全部投影与事件流的一致性。新增检查项追加到表尾，同时锁定它的 `subject`、`actual`、`expected` 的结构、类型和含义；已有检查码的定义不改变。
- 当前每次检查执行表中全部检查项。将来不同触发执行不同的检查集合时，必须同时规定 `status` 的聚合规则：一次检查通过只能清除它重新验证过的检查项的异常。

## 执行

- 一次检查的全部检查项和 `max_seq`（`coalesce(max(seq), 0)`）在同一个 `Readers::call` 中读取，看到同一个快照。
- 结果：全部检查项执行完且没有违规为 `passed`；执行完且有违规为 `violated`；查询失败、汇总超出 `i64` 或任务本身失败为 `failed`。
- 检查不修改数据库，不自动修复，不重试。结果不阻止启动、写入、备份和之后的检查，不让进程退出或重启。

## 报告

`/health` 的 `data.invariant_check` 总是存在：

```json
{
  "last_attempt": { "finished_at": 1791248400000, "result": "violated" },
  "last_report": {
    "finished_at": 1791248400000,
    "max_seq": 42,
    "checks": [
      { "check": "INVENTORY_LOT_REMAINING", "violations": 1 },
      { "check": "INVENTORY_UNALLOCATED", "violations": 0 },
      { "check": "INVENTORY_ON_HAND", "violations": 0 }
    ],
    "violation_count": 1,
    "samples": [
      {
        "check": "INVENTORY_LOT_REMAINING",
        "subject": { "item_id": "…", "lot_id": "RAW-FLOUR-20261006-001" },
        "actual": "7",
        "expected": "0"
      }
    ],
    "samples_truncated": false
  }
}
```

- `last_attempt`：最近一次检查尝试，`result` 为 `passed`、`violated` 或 `failed`。
- `last_report`：最近一次执行完的检查（`passed` 或 `violated`）的报告。
  - `checks`：本次执行的全部检查项，按执行顺序，`violations` 是该项的违规数。
  - `violation_count`：各项违规数之和。违规按「检查码 + 对象」计数，不同检查项发现同一物料异常时分别计数。
  - `samples`：最多 20 条违规，按检查顺序，同一检查项内按 `subject` 的字段依次比较（字段顺序同上表，按字节序）。
  - `samples_truncated`：`violation_count` 大于 `samples` 的条数时为 `true`。
- `finished_at` 是检查结束时时钟模块的 `now()` 原值。`passed` / `violated` 时两个 `finished_at` 相同。
- 失败（`failed`）只更新 `last_attempt`，保留之前的 `last_report`：不发布半份报告，不把未知的违规数写成 0。
- 结果按执行完成的先后更新，不比较时间戳：时钟回拨后完成的检查照样覆盖之前的结果。
- 检查执行期间，`/health` 返回上一次的结果。本进程还没有检查结束时，两个字段都为 `null`。
- 只保存在进程内存中，不写数据库或事件，重启后清空，由启动检查重新得出。
- 读取 `/health` 不触发检查。

`last_attempt.result` 为 `violated` 或 `failed` 时 `status` 为 `degraded`；之后一次 `passed` 清除自检造成的降级，其他降级原因照常生效。两个字段为 `null` 时不因自检降级：`ok` 只表示本进程暂未发现异常。

## 日志

- `violated`：记 `error`，带各检查码的违规数和 `samples` 中的明细。
- `failed`：记 `error`，带完整 source 链、操作上下文和调用位置；每个内部错误只记一次（[Rust](../engineering/rust.md#rust)错误处理）。需要区分失败原因时，在错误产生处用项目错误类型区分，不匹配 SQLite 错误文本。
- `/health` 只返回上面的结构，不返回 SQL、文件路径或底层错误。

## 触发

- **启动检查**：每次启动一次，在数据库打开、迁移和门店身份校验完成之后提交。HTTP 服务不等待它完成。
- **闭店检查**：闭店触发（[触发](backup.md#触发)的 `closing_backup_time`，时区、夏令时、时钟跳变与回拨的规则相同）执行闭店流程：
  1. 闭店备份；
  2. 把备份结果交给 `/health`；
  3. 自检（备份失败也执行）；
  4. 把自检结果交给 `/health`。
  - 闭店流程在备份任务入口中作为一个任务执行，自检结束后入口才开始下一个排队的备份。每小时备份不触发自检。
- 没有其他定时触发。
  - 每次检查都要扫描全部库存流水，并在执行期间持有读快照、推迟 checkpoint。完整性和 `seq` 连续性已由每小时备份校验，库存投影的异常接受接近一天的发现延迟；需要更短的发现时限时再调整频率。
- 所有检查（启动、闭店、测试入口）经同一个检查入口，按提交顺序逐个执行，不并发。
  - 并发时，旧快照的结果可能晚于新快照的结果发布，把它覆盖。

## 关闭

收到停止信号后不再接受新的触发；执行完正在进行和已提交的检查、已接受的闭店流程和已排队的备份，再关写线程（[优雅关闭顺序](runtime.md#优雅关闭)）。关闭返回后不留后台任务。

## 测试

- 锁定测试经 `boh_app::test_node` 进行（[带后台任务的测试节点](../interfaces/app.md#boh-app带后台任务的测试节点)）；`test_router` 不启动后台任务，`invariant_check` 的两个字段一直为 `null`。
- 制造异常：节点启动前经 `boh_storage::testing::open_writer` 修改投影表；节点运行中经 `TestNode::writer` 的唯一写连接修改，不另开写连接。不关闭外键，不替换冻结的视图定义。
- 覆盖：
  - 检查项：一致的账本（零库存、负批次余量、账外缺口的组合）、批次余量与流水不一致、没有流水的批次、停用物料、账外缺口与流水不一致（两边都有值、缺行、多余行、流水合计为正）、账外缺口缺行且流水合计为 0 合法、流水汇总超出 `i64`（第 1、2 项）。
    - 批次余量和账外缺口受库存状态上限的表约束限制，第 3 项的汇总在测试中造不出溢出；规则照常适用。
  - 报告：样本排序与截断、样本满后仍完整计数、首次失败、失败保留旧报告且不发布半份报告、修复后恢复与时钟回拨后的结果覆盖、与其他降级原因的叠加、检查不修改数据库、读取 `/health` 不触发。
  - 触发与关闭：启动检查、闭店在非空库存上检查、闭店先备份后检查且阻挡之后排队的备份、备份失败仍检查、`finished_at` 取结束时刻、关闭等待正在执行的检查和已接受的闭店流程。
