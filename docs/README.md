# 文档索引

本文件只维护阅读入口。规则正文保留在对应专题；任务涉及多个主题时取并集，不要求通读全部文档。正文中的跨主题链接用于定位：与本次行为有关的条件、例外必须读取；未来功能的背景链接不扩展实现范围。

## 目录结构

| 目录 | 正文范围 |
|---|---|
| [engineering/](engineering/) | 开发与审查流程、架构、Rust、存储、HTTP 共同规则 |
| [business/](business/) | 时间、命令提交、主数据、库存、业务操作、员工认证 |
| [operations/](operations/) | 服务生命周期、健康状态、备份、不变量自检、TLS、门店 SOP |
| [interfaces/](interfaces/) | storage、domain、app、server 的稳定测试接口 |

根目录保留本任务索引、[领域总览](domain.md)、[接口索引](interfaces.md)和[术语](glossary.md)。`auth.md`、`backup.md`、`invariant-check.md`、`sop.md`、`tls.md` 只保留章节导航，供现有引用定位；正文按上表分类存放。

## 开始任务

1. 核对当前目录、分支、HEAD、未提交改动与任务范围，读取完整 [AGENTS.md](../AGENTS.md)。
2. 核对[路线图](../AGENTS.md#路线图)、[上线阶段](../AGENTS.md#开发阶段与上线)、相关章节的实现状态；[尚未设计](domain.md#尚未设计动手前先讨论)和[待确认问题](domain.md#待确认问题)不能自行实现。
3. 按下表读取业务专题及其共同规则。实现先读[开发顺序第 5 步](engineering/development.md#开发顺序每个新功能)、[模块职责](engineering/architecture.md#crate-划分)、[Rust 与诊断规则](engineering/rust.md)；设计、锁定测试和 review 按开发顺序中的当前步骤执行。
4. 再定位源码与锁定测试；独立测试审查遵守[测试分工](engineering/development.md#测试分工)的先后顺序、独立副本和禁读范围，不能据此提前读实现。

## 按任务读取

| 任务 | 主题正文 | 必须同时读取的共同规则与验收入口 |
|---|---|---|
| 业务写命令、handler、幂等或确认 | [HTTP、幂等、写命令与异常分级](engineering/http.md)及具体业务接口 | [存储事务与账本](engineering/storage.md)；[ID](business/time.md#id-与时间)、[节点信号](business/time.md#门店节点校时)、[发生时间校验](business/time.md#发生时间的校验)、[营业日等通用规则](business/time.md#通用规则)；[当前身份开发桩](business/auth.md#开发桩)；[app 测试入口](interfaces/app.md)、[相关验收](business/operations.md#验收用例) |
| 主数据、初始化 | [主数据与接口](business/master-data.md) | 写入还适用上一行；含数量时读[数量](business/inventory.md#数量)与[单位](business/inventory.md#单位)；员工/PIN 另读 [business/auth.md](business/auth.md)；CLI 读 [init 契约](interfaces/server.md#boh-serverinit-子命令) |
| 收货、报损、温度记录 | [收货](business/operations.md#收货接口)、[报损](business/operations.md#报损接口)或[温度](business/operations.md#温度记录接口) | 写入共用规则；库存操作读[数值、数量、单位、批次与投影](business/inventory.md)、[库存职责](engineering/architecture.md#库存规则归属)；[已锁定算例与测试文件](business/operations.md#已锁定)。报损另读[吸收边界](business/operations.md#盘点吸收)，不得提前实现尚未落地的判定 |
| 生产、盘点、吸收、纠错、销售导入 | [生产](business/operations.md#生产规则)、[盘点](business/operations.md#盘点)、[吸收](business/operations.md#盘点吸收)、[纠错](business/operations.md#纠错)、[销售导入](business/operations.md#销售导入) | 写入共用规则、[库存共同规则](business/inventory.md)、[库存职责](engineering/architecture.md#库存规则归属)、[待实现验收](business/operations.md#待实现)；补录/锁账读 [business/time.md](business/time.md#补录) 与 [Q6](domain.md#待确认问题) |
| 库存投影或查询 | [库存共同规则与查询](business/inventory.md) | [职责](engineering/architecture.md#库存规则归属)、[存储/重建](engineering/storage.md)、[HTTP 信封与错误](engineering/http.md#http-约定)；按影响读取业务事件、[事件目录](domain.md#事件目录)、[重放验收入口](business/operations.md#已锁定) |
| 节点校时、时间纯函数、营业日 | [节点校时](business/time.md#门店节点校时)、[发生时间校验](business/time.md#发生时间的校验)或[营业日通用规则](business/time.md#通用规则)；[时间与存储故障限制](business/time.md#已知限制) | [Clock](interfaces/storage.md#时钟boh_storageclock)、[domain 时间接口](interfaces/domain.md#boh-domain时间boh_domaintime)、[app 核对测试入口](interfaces/app.md#boh-app带后台任务的测试节点)、[配置/init/进程](interfaces/server.md)；[健康与关闭](operations/runtime.md)。修改节点核对无需读取主数据、库存或销售全文 |
| App 提交、重试、取消、工作站 | [business/command-submission.md](business/command-submission.md) | [前端校时](business/time.md#前端校时)、[补录](business/time.md#补录)、[异常确认](engineering/http.md#异常分级)、[business/auth.md](business/auth.md)、[TLS 的 App 要求](operations/tls.md#对前端-app-的要求)、[SOP](operations/sop.md)；实现阶段按路线图 |
| 认证、权限、设备注册 | [business/auth.md](business/auth.md) | [operations/tls.md](operations/tls.md)、[主数据归属](business/master-data.md#主数据)、[HTTP 与幂等](engineering/http.md)、[认证验收](business/auth.md#验收用例) |
| 备份、不变量自检或健康报告 | [operations/backup.md](operations/backup.md)、[operations/invariant-check.md](operations/invariant-check.md)或 [operations/runtime.md](operations/runtime.md#健康状态get-health) | [SQLite 连接例外](engineering/storage.md#sqlite)、[关闭顺序](operations/runtime.md#优雅关闭)、[测试节点](interfaces/app.md#boh-app带后台任务的测试节点)；[备份测试](operations/backup.md#测试)、[自检测试](operations/invariant-check.md#测试) |
| 配置、监听、启动、关闭或进程 review | [server 契约](interfaces/server.md)、[operations/runtime.md](operations/runtime.md) | [节点校时](business/time.md#门店节点校时)、[备份](operations/backup.md)、[自检](operations/invariant-check.md)、[存储启动/关闭接口](interfaces/storage.md#公开-api)、[TLS 状态与启动/关闭](operations/tls.md)、[部署适用阶段](../deploy/README.md#适用阶段)；[独立进程验收范围](engineering/development.md#测试分工)。不要求库存命令全文 |
| 锁定测试、规范审查或代码 review | [开发顺序](engineering/development.md#开发顺序每个新功能)、[测试分工](engineering/development.md#测试分工)、[Review guidelines](engineering/development.md#review-guidelines)中相应阶段、[稳定接口索引](interfaces.md) | 按被测行为读取业务规范和验收；预期值须人确认，不能用实现推导预期或破坏独立审查的读取范围；代码 review 另读 [工程检查表](engineering/rust.md#机器强制) |
| 依赖、模块归属或工程约束 | [engineering/architecture.md](engineering/architecture.md)、[engineering/rust.md](engineering/rust.md) | [角色和受保护路径](engineering/development.md#协作分工)、[当前开发步骤](engineering/development.md#开发顺序每个新功能)；依赖先登记并经人批准 |
| 门店部署与人工操作 | [部署要求](../deploy/README.md)、[部署记录模板](../deploy/deployment-record.template.md)、[SOP](operations/sop.md) | 按步骤读取时间、认证、TLS 和备份规则；上线时的操作要求不等于当前软件能力 |

## 三个代表任务的阅读路径

- **实现报损**：[开始任务](#开始任务) → [报损接口](business/operations.md#报损接口)及[已锁定表](business/operations.md#已锁定)的报损行 → [HTTP 共同规则](engineering/http.md) → 库存的[数值](business/inventory.md#数值)、[数量](business/inventory.md#数量)、[单位](business/inventory.md#单位)、[批次](business/inventory.md#批次)、[投影](business/inventory.md#投影表) → [库存规则归属](engineering/architecture.md#库存规则归属) → [存储与账本](engineering/storage.md) → 时间的[ID](business/time.md#id-与时间)、[节点信号](business/time.md#门店节点校时)、[发生时间校验](business/time.md#发生时间的校验)、[通用规则](business/time.md#通用规则) → [开发桩](business/auth.md#开发桩) → [Router 测试入口](interfaces/app.md#boh-approuter-测试入口)、[报损 payload](interfaces/domain.md#boh-domain报损-payloadboh_domainwaste) → [spec_waste.rs](../crates/boh-app/tests/spec_waste.rs)、[golden](../crates/boh-app/tests/golden/WASTE_LOGGED@1)、[重放测试](../crates/boh-app/tests/spec_replay.rs)。检查[盘点吸收](business/operations.md#盘点吸收)的当前边界；结果未知语义读[提交状态](business/command-submission.md#1-提交状态app)、[后台重试收到的结果](business/command-submission.md#4-后台重试收到的结果)，无需读销售或认证未来设计全文。
- **修改节点校时**：[开始任务](#开始任务) → [ID](business/time.md#id-与时间)、[校时信号](business/time.md#校时信号)、[门店节点校时](business/time.md#门店节点校时)、[节点时钟接口](business/time.md#节点时钟接口) → [时间与状态文件故障](business/time.md#已知限制) → [Clock](interfaces/storage.md#时钟boh_storageclock)、[TestNode](interfaces/app.md#boh-app带后台任务的测试节点)、[配置](interfaces/server.md#boh-server配置)/[init](interfaces/server.md#boh-serverinit-子命令)/[进程](interfaces/server.md#boh-server服务进程) → [关闭](operations/runtime.md#优雅关闭)和[健康状态](operations/runtime.md#健康状态get-health) → [信号有效性接口](interfaces/domain.md#boh-domain时间boh_domaintime) → [节点校时测试](../crates/boh-app/tests/spec_clock_sync.rs)、[时间测试](../crates/boh-domain/tests/spec_time.rs)、[init 测试](../crates/boh-server/tests/spec_init.rs)和[服务测试](../crates/boh-server/tests/spec_serve.rs)。涉及前端或营业日时才扩展读取相应章节。
- **审查服务启动关闭**：[开始任务](#开始任务)与[开发顺序](engineering/development.md#开发顺序每个新功能)的第 6 步/[测试分工](engineering/development.md#测试分工) → [server 契约](interfaces/server.md) → [服务生命周期与健康状态](operations/runtime.md) → [存储公开 API](interfaces/storage.md#公开-api) → [门店节点校时](business/time.md#门店节点校时) → 备份的[触发](operations/backup.md#触发)/[关闭](operations/backup.md#关闭)与自检的[触发及初始化](operations/invariant-check.md#触发)/[关闭](operations/invariant-check.md#关闭) → TLS 的[实现边界](operations/tls.md#实现)、[监听](operations/tls.md#监听)、[启动检查](operations/tls.md#配置与启动检查)、[关闭](operations/tls.md#关闭) → [部署适用阶段](../deploy/README.md#适用阶段)。按实际改动决定黑盒验收及返修证据沿用范围；无需读取报损或销售业务。

## 规则与事实的来源

| 要确定什么 | 权威来源 |
|---|---|
| 核心硬规则、角色边界与当前阶段 | [AGENTS.md](../AGENTS.md)；详细工程与流程正文按其中链接读取；Claude 补充流程见 [CLAUDE.md](../CLAUDE.md) |
| 业务语义、适用条件、例外、错误与警告 | [领域索引](domain.md#阅读导航)指向的专题，以及认证、备份、自检、TLS 专题；引用位置不复制规则全文 |
| 已落地的字段与表结构 | `boh-domain` 结构体、迁移 SQL；业务章节提供代码定位。未实现结构仍按相应设计章节，不自行冻结未决设计 |
| 稳定测试接口、可见性和测试能力 | [interfaces.md](interfaces.md) 及 storage/domain/app/server 专题；客户端业务 API 在各业务专题 |
| 验收预期与覆盖 | 经人确认的锁定测试；[代表算例与待实现用例](business/operations.md#验收用例)、[认证用例](business/auth.md#验收用例)和各基础设施专题测试要求。不得为通过测试改预期 |
| 实现状态与运行证据 | [路线图](../AGENTS.md#路线图)和原专题的状态说明用于定位；当前 checkout 的代码、测试、对应 HEAD 的验证结果证明交付。静态检查不证明真实进程行为 |
| 规范用词 | [glossary.md](glossary.md)；具体条件仍以其引用的正文为准 |

发现来源冲突时，报告具体位置和差异，按[协作分工](engineering/development.md#协作分工)及[设计变更流程](engineering/development.md#开发顺序每个新功能)处理，不自行选一方、改预期或绕过受保护路径。
