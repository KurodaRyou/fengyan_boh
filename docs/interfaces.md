# 锁定测试使用的接口

锁定测试只通过本索引及其职责专题列出的接口访问系统（见 [测试分工](engineering/development.md#测试分工)）。签名变更必须以 `docs:` 提交更新对应专题。
时间与营业日规则见 [时间](business/time.md#时间)。

本索引及专题定义测试入口；面向客户端的 HTTP 业务契约见 [接口](domain.md#接口)，认证规则见 [business/auth.md](business/auth.md)。标注「随局域网 HTTPS 切片」的字段与入口尚未实现，其余接口按当前已落地切片提供；总体进度见 [路线图](../AGENTS.md#路线图)。

## 按职责读取

| 职责 | 正文 |
|---|---|
| storage | [连接、Writer、Readers、testing、Clock](interfaces/storage.md) |
| domain | [时间纯函数、闭店时刻、报损 payload](interfaces/domain.md) |
| app | [Router 与带后台任务的 TestNode](interfaces/app.md) |
| server | [配置、init、重建与服务进程](interfaces/server.md) |

### boh-storage

见 [boh-storage](interfaces/storage.md#boh-storage)。

### 公开 API

见 [公开 API](interfaces/storage.md#公开-api)。

### boh_storage::testing

见 [boh_storage::testing](interfaces/storage.md#boh_storagetesting)。

### 时钟（`boh_storage::clock`）

见 [时钟（`boh_storage::clock`）](interfaces/storage.md#时钟boh_storageclock)。

### boh-domain：时间（`boh_domain::time`）

见 [boh-domain：时间（`boh_domain::time`）](interfaces/domain.md#boh-domain时间boh_domaintime)。

### 闭店备份时刻

见 [闭店备份时刻](interfaces/domain.md#闭店备份时刻)。

### boh-domain：报损 payload（`boh_domain::waste`）

见 [boh-domain：报损 payload（`boh_domain::waste`）](interfaces/domain.md#boh-domain报损-payloadboh_domainwaste)。

### boh-app：Router 测试入口

见 [boh-app：Router 测试入口](interfaces/app.md#boh-approuter-测试入口)。

### boh-app：带后台任务的测试节点

见 [boh-app：带后台任务的测试节点](interfaces/app.md#boh-app带后台任务的测试节点)。

### boh-server：配置

见 [boh-server：配置](interfaces/server.md#boh-server配置)。

### boh-server：`init` 子命令

见 [boh-server：`init` 子命令](interfaces/server.md#boh-serverinit-子命令)。

### boh-server：`rebuild-projections` 子命令

见 [boh-server：`rebuild-projections` 子命令](interfaces/server.md#boh-serverrebuild-projections-子命令)。

### boh-server：服务进程

见 [boh-server：服务进程](interfaces/server.md#boh-server服务进程)。
