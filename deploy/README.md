# 门店节点部署要求

规则见 `docs/`：时间与网络见 `docs/domain.md`「时间」门店节点校时的部署要求，局域网 HTTPS 见 `docs/tls.md`，备份见 `docs/backup.md`。本文件只写部署时的操作步骤。每店的具体地址和日期写进部署记录（模板 `deployment-record.template.md`），不提交到仓库。

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
7. `listen_addr` 写成有线网口的固定 IP（等于 `[tls].ip`），不写 `0.0.0.0`：门店机的无线网卡连店长热点，不能在热点上暴露 HTTPS 端口。

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
6. 在 MDM 中把指纹录入本店平板和手机的 App 托管配置（Android managed configurations / iOS 托管应用配置），与门店节点地址一起下发。托管配置中没有本店指纹时 App 拒绝连接。
7. 重启服务。叶证书由节点自动签发到 `/var/lib/boh/tls/`，不需要手工操作。在一台平板上确认能连上。

到期前更换（5 年一次）：重复第 1–5 步生成新私钥、CSR 和新 CA；先在 MDM 托管配置中**加入**新指纹（新旧并存），再替换节点上的两个文件并重启服务；确认平板能连上后，从托管配置中删除旧指纹。

## 节点更换与硬盘报废

门店机更换、重装或疑似被攻破时：

1. 新机器按本文件重新部署：新私钥、新的门店签发 CA，`[tls].ip` 保持部署记录中的地址。
2. 计算新 CA 的指纹，写进部署记录；在 MDM 中把本店托管配置**改为只有新指纹**（删除旧指纹），使旧 CA 签的证书不再被 App 接受。
3. 在一台平板上确认能连上，再通知店长恢复录入。TLS 方面平板不需要重新注册；认证状态按「门店节点恢复」的原则处理。
4. 第 1–3 步完成之前，门店按 SOP「门店节点宕机」纸面登记。

- 旧的门店签发 CA 不吊销，靠 App 不再接受它的指纹失效。旧硬盘上有门店签发 CA 的私钥，报废时物理销毁。
