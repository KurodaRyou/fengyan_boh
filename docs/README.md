# 文档索引

本文件供 agent 按任务定位规范，只维护阅读入口；规则正文保留在对应文档中。

## 开始任务

1. 确认当前工作目录、分支、HEAD 和未提交改动；实现状态以当前 checkout 为准。
2. 读取 [AGENTS.md](../AGENTS.md)，核对[路线图](../AGENTS.md#路线图)、[开发阶段与上线](../AGENTS.md#开发阶段与上线)与本轮任务范围。已定规则不代表功能已实现；依赖[待确认问题](domain.md#待确认问题)或[尚未设计事项](../AGENTS.md#尚未设计动手前先讨论)的部分遵守相应限制。
3. 按下表读取相关章节，并继续读取其中引用的共用规则；涉及多个主题时取并集。无需为单一任务通读全部 `docs/`。
4. 实现或 review 时，再定位对应源码与锁定测试。独立测试审查的阶段、依据和禁读范围以 [AGENTS.md「测试分工」](../AGENTS.md#测试分工)为准。

## 按任务读取

| 任务涉及 | 规则入口与共同依赖 |
|---|---|
| 业务写命令 | [写命令通用规则](domain.md#写命令通用规则)、[HTTP 约定](../AGENTS.md#http-约定)、[幂等](../AGENTS.md#幂等)、[节点校时](domain.md#门店节点校时)、[发生时间校验](domain.md#发生时间的校验)、[异常分级](domain.md#异常分级)，再读[具体命令接口](domain.md#阅读导航) |
| 主数据维护与初始化 | [主数据归属与初始化](domain.md#主数据)、[主数据接口](domain.md#主数据接口)、[员工主数据](auth.md#员工主数据)；写入还适用「业务写命令」行的共用规则 |
| 库存决策、投影与查询 | [库存规则归属](../AGENTS.md#库存规则归属)、[数量](domain.md#数量)、[单位](domain.md#单位)、[批次](domain.md#批次)、[投影表](domain.md#投影表)，再读对应业务、[库存明细](domain.md#库存明细查询)或[批次查询](domain.md#批次查询)；涉及吸收时读[盘点](domain.md#盘点)与[盘点吸收](domain.md#盘点吸收) |
| 时钟、营业日与补录 | [时间](domain.md#时间)；补录另读[命令提交与结果未知](domain.md#命令提交与结果未知)、[异常分级](domain.md#异常分级)与[待确认问题](domain.md#待确认问题) |
| App 提交、重试、取消与工作站 | [前端校时](domain.md#前端校时)、[命令提交与结果未知](domain.md#命令提交与结果未知)、[前端错误处理](domain.md#前端错误处理)、[认证](auth.md)、[HTTPS 的 App 要求](tls.md#对前端-app-的要求)、[SOP](sop.md) |
| 认证、权限与设备注册 | [auth.md](auth.md)、[局域网 HTTPS](tls.md)、[主数据](domain.md#主数据)；当前身份入口见[开发桩](auth.md#开发桩) |
| 备份、自检与健康状态 | [备份](backup.md)、[不变量自检](invariant-check.md)、[SQLite](../AGENTS.md#sqlite)、[HTTP 约定中的 `/health`](../AGENTS.md#http-约定)；后台任务关闭顺序见 [Rust](../AGENTS.md#rust) |
| 配置、监听、启动与关闭 | [局域网 HTTPS](tls.md)、[节点校时](domain.md#门店节点校时)、[Rust 关闭顺序](../AGENTS.md#rust)、[进程与配置测试接口](interfaces.md)、[部署要求](../deploy/README.md) |
| 锁定测试与验收 | [测试分工](../AGENTS.md#测试分工)、[稳定测试接口](interfaces.md)、相关业务规范的验收用例；预期值确认按[开发顺序](../AGENTS.md#开发顺序每个新功能)执行 |
| 门店部署与人工操作 | [部署要求及适用阶段](../deploy/README.md#适用阶段)、[部署记录模板](../deploy/deployment-record.template.md)、[SOP](sop.md)及其引用的时间、认证、TLS 规则 |

## 规则与事实的来源

| 要确定什么 | 来源 |
|---|---|
| 工程约束、角色边界、交付流程 | [AGENTS.md](../AGENTS.md)；Claude 的补充流程见 [CLAUDE.md](../CLAUDE.md) |
| 业务语义、条件、例外、错误与警告 | [domain.md](domain.md#阅读导航)；认证、备份、自检、TLS 的细则分别在对应专题文档 |
| 已落地的字段与表结构 | `boh-domain` 结构体与迁移 SQL，按业务章节中的代码指向定位；测试入口的契约见 [interfaces.md](interfaces.md) |
| 验收预期与覆盖范围 | 经人确认的锁定测试；文档中的代表算例与待实现用例见[验收用例](domain.md#验收用例)。预期值的修改限制见[测试分工](../AGENTS.md#测试分工) |
| 实现进度与运行证据 | [路线图](../AGENTS.md#路线图)与各节状态说明用于定位；是否已交付须核对当前代码、测试和对应 HEAD 的验证结果 |
| 规范用词 | [术语表](glossary.md)；具体规则仍读其指向的正文 |

发现来源之间不一致时，指出具体位置和差异，按 [AGENTS.md「协作分工」](../AGENTS.md#协作分工)及「开发顺序」处理；不通过改预期值或绕过受保护路径来消除冲突。
