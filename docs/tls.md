# 局域网 HTTPS

本文件是局域网 HTTPS 的规则：监听、证书层级、叶证书的自动签发与续期、启动检查、关闭和对前端 App 的要求。硬规则摘要见 [AGENTS.md](../AGENTS.md)「HTTP 约定」「认证」；部署步骤见 `deploy/README.md`。
随局域网 HTTPS 切片实现。

## 监听

- `listen_addr`：配置了 `[tls]` 时只提供 HTTPS，平板和手机经它访问。局域网上不开明文 HTTP 端口，不做 HTTP 到 HTTPS 的跳转。
- `workstation_addr`（可选）：明文 HTTP，只供门店机本机的工作站浏览器使用，与 `listen_addr` 提供同一个 `Router`（静态文件 + API）。省略时不开这个端口。
  - IP 必须是回环地址（`127.0.0.0/8` 或 `::1`）；其他地址（包括 `0.0.0.0`、`::` 和门店机的局域网 IP）一律拒绝启动。
    - 理由：工作站只能在门店机本机访问；回环流量不经过网卡，浏览器把回环地址当作安全来源。绑定到其他地址会让 PIN 和令牌以明文出现在局域网上。
- release 构建没有 `[tls]` 段时拒绝启动。debug 构建省略 `[tls]` 时，`listen_addr` 提供明文 HTTP（开发与锁定测试用），不注册认证接口（[auth.md](auth.md)「传输加密」）。
- 握手失败不进请求日志：请求日志只记进入应用中间件的请求（AGENTS.md「Rust」）。
- 握手失败的日志写进节点自身的日志，与请求日志同一输出：
  - 每次握手失败记一条 `debug`，字段为来源 IP 和失败原因。
  - 另按时间汇总：每 5 分钟最多一条 `warn`，字段为该时段的失败次数、最近一次失败的来源 IP 和原因；该时段没有失败时不记。
    - 理由：握手失败多由客户端引起（时间错误、未内置根、扫描），逐条记 `warn` 会淹没日志。
  - 握手失败不进 `/health` 的问题清单。

## 证书层级

| 证书 | 有效期 | 签发者 | 保管 |
|---|---|---|---|
| 连锁根 | 20 年，ECDSA P-256 | 自签 | 私钥口令加密，存两只 U 盘分开离线保存，由维护门店机的人保管，写进维护交接；根证书内置 App |
| 门店签发 CA | 5 年 | 连锁根，部署时由维护人员离线签发 | 每店一张；私钥在门店节点上生成，不离开节点 |
| 叶证书 | 90 天（上限见下） | 门店签发 CA，由节点自动签发 | 私钥由节点生成，只在节点上 |

- 门店签发 CA：`basicConstraints` CA、`pathLen = 0`；名称约束（critical）permitted 为 `DNS:<store_id>.node.fengyan.internal`、`IP:10.0.0.0/8`、`IP:172.16.0.0/12`、`IP:192.168.0.0/16`。
  - 名称约束只限制门店签发 CA 能签的名称，不用于隔离门店。
- 生成与签发流程（节点上生成私钥和 CSR，维护人员用根加名称约束签发）见 `deploy/README.md`。
- 叶证书：
  - 密钥 ECDSA P-256；EKU `serverAuth`；签发者名称取门店签发 CA 证书中的主题原文。
  - SAN：`IP:<[tls].ip>`、`DNS:<store_id>.node.fengyan.internal`。`ip` 是部署记录中门店节点的固定 IP。
  - `notBefore = now − 1 天`；`notAfter = min(now + 90 天, 门店签发 CA 的 notAfter)`。
    - 理由：CA 过期后整条链失效，叶证书更长的有效期没有意义，还会让 `tls_cert_not_after` 误导提醒。
  - `now` 取时钟模块的读数，与同一次判断中的节点信号判定是同一次读数。

## 叶证书的签发与续期

**可用**：叶证书文件存在、能解析、与叶私钥配对、由当前配置的门店签发 CA 签发、IP SAN 等于配置的 `ip`、按节点时钟在有效期内。

- 启动时：
  - 有可用的叶证书：加载它。
  - 没有可用的叶证书、节点信号有效：签发正式证书，持久化后加载。
  - 没有可用的叶证书、节点信号无效：签发**临时证书**并加载。
- 运行中由续期任务每 1 分钟判断一次（间隔按单调时间计）：
  - 当前是临时证书，节点信号有效：签发正式证书，持久化后替换。
  - 当前是正式证书，剩余有效期 ≤ 30 天，节点信号有效，且新证书的 `notAfter` 会晚于现有证书：签发正式证书，持久化后替换。
    - 门店签发 CA 剩余不足 30 天时，新证书的 `notAfter` 与现有的相同，不再重签，直到更换门店签发 CA。
- 临时证书：参数与正式证书相同；只在内存中，**不写文件**；`/health` 的 `tls_cert_provisional` 为 `true`。
  - 理由：节点时钟出错时签出的证书若写进文件，下次启动会被当作可用而继续使用，并覆盖原有的正式证书。
- 正式证书只在节点信号有效时签发；临时证书是唯一的例外。
- 门店签发 CA 按节点时钟已过期：不签发叶证书。有叶证书文件能解析、与叶私钥配对、由该 CA 签发且 IP SAN 与 `ip` 相符时照常加载；没有时 `listen_addr` 不开 HTTPS 端口，`workstation_addr` 照常。
- 替换经 `RustlsConfig` 重新加载，不重启进程。
- 密钥生成和签名在 `spawn_blocking` 中执行，不进入 `Writer::call` / `Readers::call` 闭包。
- 例：节点时钟被重置到 2000-01-01、校时状态文件已不存在、叶证书文件损坏。节点签出 `notBefore = 1999-12-31`、`notAfter = 2000-03-31` 的临时证书；平板按正确的时间判断它已过期，握手失败。店长按 SOP 打开热点，节点校时成功后，下一次判断签出正式证书，平板恢复连接。

**持久化**：叶证书与叶私钥写在 `[tls].cert_dir`（示例 `/var/lib/boh/tls`）中的 `node.key`、`node.crt`（PEM：叶证书在前，随后是门店签发 CA 证书）。

- 启动时 `cert_dir` 不存在则创建（权限 0700）；创建失败或不是目录时拒绝启动。`node.key` 权限 0600。
- 写入顺序：写私钥临时文件 → fsync → 重命名为 `node.key` → 写证书临时文件 → fsync → 重命名为 `node.crt` → fsync 目录。全部成功才替换正在使用的证书。
  - 任一步失败：记 `error`，继续使用当前证书（含临时证书），下一次判断时重新执行全部步骤。
  - 中途断电留下不配对的文件时，下次启动按「可用」判定发现并重签。
- 叶证书和叶私钥不写进数据库，不放在 `backup_dir` 中，不进备份。

**日志**：签发、替换、临时证书转为正式证书记 `info`；签发或持久化失败记 `error`，在续期任务中记录一次，附失败阶段（生成密钥、签发、写私钥、写证书、目录 fsync、重新加载）。

## 配置与启动检查

```toml
listen_addr = "0.0.0.0:8443"
workstation_addr = "127.0.0.1:8080"

[tls]
ip = "192.168.1.20"
ca_cert_path = "/run/credentials/boh.service/store-ca.crt"
ca_key_path = "/run/credentials/boh.service/store-ca.key"
cert_dir = "/var/lib/boh/tls"
```

- `[tls]` 的四项都必填。`ip` 是 IPv4 地址，必须在门店签发 CA 名称约束的三个私有网段内。`ca_key_path` 是 PKCS#8 PEM 私钥。
- 门店签发 CA 的证书和私钥部署在 `/etc/boh/tls/`（root，0600），经 systemd `LoadCredential=` 以只读方式交给服务（`deploy/boh.service`）。
  - 理由：`DynamicUser=yes` 的 UID 启动前未知，无法预先改属主；凭据文件权限是 0440，程序不检查「必须 0600」。
- 拒绝启动：
  - release 构建没有 `[tls]`；
  - `[tls]` 字段缺失或取值非法；
  - `workstation_addr` 不是回环地址；
  - 门店签发 CA 的证书或私钥读不了、解析失败、私钥与证书不匹配；证书不是 CA，或没有名称约束；
  - `cert_dir` 创建失败或不是目录。
- 不拒绝启动：门店签发 CA 按节点时钟已过期或尚未生效、叶证书不可用。记 `error`，由 `/health` 的问题清单报告（AGENTS.md「HTTP 约定」）。
  - 理由：节点时钟本身可能是错的；进程照常启动，查询、校时和工作站尽量可用。
- 叶证书的 `ip` 与配置不符时，按「可用」判定重签；改 `ip` 后重启服务即可。

## 时间

- 证书有效期由平板按自己的系统时钟核对；平板由 MDM 强制自动设置时间（[domain.md](domain.md)「时间」前端校时）。
- 节点签发证书只经时钟模块读时间。rustls `ServerConfig` 的 `time_provider` 注入时钟模块提供的实现。
- `time`、`x509-parser` 中读系统时间的方法由 `clippy.toml` 禁用（AGENTS.md「Rust」）。

## 关闭

- 「停止接收 HTTP」一步同时停止 `listen_addr` 和 `workstation_addr`（`axum-server` 的 `Handle` 优雅关闭）。
- 续期任务：等待下一次判断的间隔直接取消；已经开始的签发与持久化执行完再退出（`spawn_blocking` 开始运行后不能中止）。

## 节点更换

- 新机器生成新的门店签发 CA 私钥与 CSR，由连锁根的保管人签发新的门店签发 CA；`ip` 保持部署记录中的地址。App 内置的根不变，平板不需要证书操作；认证状态按「门店节点恢复」的原则重新注册。
- 新的门店签发 CA 装好之前平板无法连接，门店按 [SOP](sop.md)「门店节点宕机」纸面登记。
- 旧的门店签发 CA 不吊销；旧硬盘报废时物理销毁（`deploy/README.md`）。

## 对前端 App 的要求

- 根证书内置 App，作为唯一的信任锚：
  - Android：Network Security Config 的 `<base-config>` 中 `<trust-anchors>` 只列内置根，不列 `system`、`user`。
  - iOS：在 URLSession 的服务器信任挑战中以内置根调用 `SecTrustSetAnchorCertificates`，再 `SecTrustEvaluateWithError`；失败时取消挑战。Info.plist 写 `NSLocalNetworkUsageDescription`。
- 按部署记录中的门店节点 IP 访问 `https://<ip>:<端口>`，用平台标准的主机名校验（核对 IP SAN）；不另做叶证书固定，不写自定义名称校验。
- TLS 握手失败按「未发出」处理（[domain.md](domain.md)「命令提交与结果未知」）。

## 实现

- TLS 用 `rustls`（`ring`），HTTPS 监听用 `axum-server`，PEM 读取用 `rustls-pki-types`，叶证书签发用 `rcgen`（见 AGENTS.md「技术栈」）。
- 叶证书的签发与续期在 `boh-app`，进程装配在 `boh-server`。
- 切片的第一个提交先用 `cargo zigbuild --release` 实测 `x86_64-unknown-linux-musl` 和 `aarch64-unknown-linux-musl` 两个目标。

## 与上行同步的关系

- 上行同步的 mTLS 沿用同一个连锁根：节点访问总部的客户端证书由单独的「门店客户端签发 CA」签发（EKU `clientAuth`），用单独的私钥，不与局域网服务器证书共用。
- 总部同步接口的服务器证书来源随上行同步设计定。
