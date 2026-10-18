# 门店节点部署要求

规则见 `docs/`：时间与网络见 [domain.md「时间」](../docs/domain.md#时间)门店节点校时的部署要求，局域网 HTTPS 见 [tls.md](../docs/tls.md)，备份见 [backup.md](../docs/backup.md)。本文件只写部署时的操作步骤。每店的具体地址和日期写进[部署记录](deployment-record.template.md)，不提交到仓库。

## 适用阶段

项目尚未上线；当前实现范围见 [AGENTS.md「路线图」](../AGENTS.md#路线图)。本文件描述门店上线时的目标部署步骤：

- 局域网 HTTPS 尚未实现。下文的 HTTPS 监听、门店签发 CA、叶证书自动签发与 `workstation_addr` 步骤随局域网 HTTPS 切片交付生效。
- [配置样例](config.example.toml)中的 TLS 与工作站配置目前保持注释；实现之前启用这些配置会被当前配置解析器拒绝。
- App、MDM 托管配置与工作站页面的操作和验收，随对应客户端交付执行；工作站的 HTTP 监听与网页前端分别交付。

## 文件

| 文件 | 安装位置 |
|---|---|
| `boh.service` | `/etc/systemd/system/boh.service` |
| `config.example.toml` | `/etc/boh/config.toml`（按部署记录填写） |
| `60-boh-timesyncd.conf` | `/etc/systemd/timesyncd.conf.d/60-boh-timesyncd.conf` |
| `60-boh-resolved.conf` | `/etc/systemd/resolved.conf.d/60-boh-resolved.conf` |

## 网络与固定 IP

1. 门店局域网中属于门店的设备（门店节点、平板、门店统一采购的手机、标签打印机等）都用固定 IP。设在设备上还是在路由器上（DHCP 保留）都可以，只要实际地址与部署记录一致。
   - 设在设备上时，地址放在路由器 DHCP 地址池之外，避免冲突。
   - 例：路由器网段 `192.168.1.0/24`、网关 `192.168.1.1`、DHCP 地址池 `192.168.1.100`–`192.168.1.199`，门店节点 `192.168.1.20`。
2. 门店节点的有线网口用固定 IP；无线网卡保存店长手机热点，无线连接的默认路由优先于有线网口（`docs/domain.md`「时间」）。有线网口的路由 metric 要保持这一点。
3. 把路由器的网段、网关、DHCP 地址池和每台设备的地址写进部署记录。
4. 任何设备或路由器重置、更换之后，按部署记录恢复原来的设置和地址。门店节点的 IP 写在叶证书中（`[tls].ip`），恢复原地址后平板不用改设置。
5. 门店业务设备（门店节点、平板、门店手机、标签打印机）用专用 Wi-Fi / 网段，与顾客、访客网络隔离。
6. 路由器关闭 UPnP 和远程管理（从外网访问路由器管理界面）。
7. `listen_addr` 写成有线网口的固定 IP（等于 `[tls].ip`），不写 `0.0.0.0`。
8. 配置持久化的主机入站防火墙：HTTPS 端口只允许从有线业务网口入站，拒绝无线热点及其他接口的入站流量；不能仅按目的 IP 放行。规则随系统启动恢复，在开放节点服务前生效；本机工作站的回环 HTTP 端口按 `docs/tls.md` 保持可用。把实际有线、无线网口名、HTTPS 端口和规则保存位置记进部署记录。
9. 从有线业务网验证 HTTPS 可访问；从热点侧验证无线地址及有线固定 IP 的 HTTPS 均不可访问，后者须覆盖流量经无线接口到达有线目标 IP 的路径。重启后复核规则仍有效，记录结果。这是 Linux 门店机的部署验收，不由 macOS 上的 Rust 测试代替。

## 门店机加固

- SSH 只允许密钥登录（`PasswordAuthentication no`、`KbdInteractiveAuthentication no`），不在路由器上做端口映射，不暴露到公网。
- kiosk 浏览器只允许打开本机工作站页面。Chromium 用企业策略（`/etc/chromium/policies/managed/boh-kiosk.json`）：

  ```json
  {
    "URLBlocklist": ["*"],
    "URLAllowlist": ["http://127.0.0.1:8080"]
  }
  ```

  端口与 `workstation_addr` 一致；kiosk 打开 `http://127.0.0.1:8080/`。

## 门店签发 CA

连锁根的私钥由维护门店机的人离线保管（`docs/tls.md`「证书层级」）。每店部署时签发一张门店签发 CA，有效期 5 年。

**专用离线签发机**：根私钥只在一台专用的签发电脑上解密使用。这台电脑不联网（拔网线、关闭无线），不用于办公或其他用途，只装签发所需的工具（`step`、`openssl`）。CSR 和签出的证书用 U 盘在签发机与门店节点之间拷贝。根私钥 U 盘只插在这台电脑上。

**维护交接**：更换维护门店机的人时，当面交接两只根私钥 U 盘、根私钥口令（与 U 盘分开交接）、专用签发机和各店的部署记录；在部署记录中写明交接日期与交接人。

1. 在门店节点上（root）生成私钥和 CSR，私钥不离开节点：

   ```sh
   install -d -m 0700 /etc/boh/tls
   openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out /etc/boh/tls/store-ca.key
   chmod 0600 /etc/boh/tls/store-ca.key
   openssl req -new -key /etc/boh/tls/store-ca.key -subj "/O=Fengyan/CN=Fengyan Store CA <store_id>" -out store-ca.csr
   ```

2. 把 `store-ca.csr` 用 U 盘拷到专用离线签发机上，用 `step` 加名称约束签发（`<store_id>` 换成配置中的门店 ID）：

   `store-ca.tpl`：

   ```json
   {
     "subject": {{ toJson .Subject }},
     "keyUsage": ["certSign", "crlSign"],
     "basicConstraints": {"isCA": true, "maxPathLen": 0},
     "nameConstraints": {
       "critical": true,
       "permittedDNSDomains": ["<store_id>.node.fengyan.internal"],
       "permittedIPRanges": ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]
     }
   }
   ```

   ```sh
   step certificate sign --template store-ca.tpl --not-after 43824h store-ca.csr root.crt root.key > store-ca.crt
   ```

3. 把 `store-ca.crt` 拷回门店节点 `/etc/boh/tls/store-ca.crt`（root，0600），打开 `boh.service` 中的 `LoadCredential=` 和配置中的 `[tls]`。
4. 用 `openssl x509 -in /etc/boh/tls/store-ca.crt -noout -text` 核对名称约束和有效期，把到期日写进部署记录和维护计划。
5. 计算本店 CA 指纹，写进部署记录：

   ```sh
   openssl x509 -in /etc/boh/tls/store-ca.crt -noout -pubkey | openssl pkey -pubin -outform DER | openssl dgst -sha256 -hex
   ```

   输出中 `= ` 之后的 64 位小写十六进制就是指纹。
6. 在 MDM 中把指纹录入本店平板和手机的 App 托管配置（Android managed configurations / iOS 托管应用配置），与门店节点地址一起下发。逐台确认设备已应用预期配置，并写进部署记录；后台显示已下发不代替这一步。托管配置中没有本店指纹时 App 拒绝连接。
7. 重启服务。节点校时信号和 CA 有效期满足 `docs/tls.md` 的签发条件后，叶证书由节点自动签发到 `/var/lib/boh/tls/`，不需要手工操作；节点信号无效时按 SOP 联网校时后再核验。逐台确认本店平板和手机能连上，完成核对的设备方可投入使用。

到期前更换（5 年一次）：

1. 在独立的 root 专用暂存目录中生成新私钥、CSR 和新 CA，核对名称约束、有效期与指纹；此时不覆盖当前使用的 CA 文件，私钥仍不离开节点。
2. 在 MDM 托管配置中**加入**新指纹（新旧并存），逐台确认本店平板和手机已应用，写进部署记录。未收到配置的离线设备不算完成更新。
3. 完成第 2 步后，替换节点上的 CA 私钥与证书文件并重启服务，逐台确认设备能连接新证书。
4. 从托管配置中删除旧指纹，再逐台确认设备已应用只有新指纹的配置，写进部署记录。

## 节点更换与硬盘报废

门店机更换、重装或疑似被攻破时：

1. 新机器按本文件重新部署：新私钥、新的门店签发 CA，`[tls].ip` 保持部署记录中的地址。
2. 计算新 CA 的指纹，写进部署记录；在 MDM 中把本店托管配置**改为只有新指纹**（删除旧指纹），逐台核实平板和手机已应用。只有完成更新的设备才不再接受旧 CA 签的证书。
3. 逐台确认设备能连上，再通知店长哪些设备可以恢复录入；未确认配置已生效的设备暂停使用，待收到配置并完成核对后再恢复。TLS 方面平板不需要重新注册；认证状态按「门店节点恢复」的原则处理。
4. 第 1–3 步完成之前，门店按 SOP「门店节点宕机」纸面登记。

- 旧的门店签发 CA 不吊销，靠 App 不再接受它的指纹失效。旧硬盘上有门店签发 CA 的私钥，报废时物理销毁。
