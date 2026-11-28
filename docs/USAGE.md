# 使用说明

## 安装与运行

**安装包**：从 GitHub Release 下载 `.dmg`（macOS）/ `.msi` 或 `-setup.exe`（Windows）安装后启动，
菜单栏出现图标即表示核心已就绪。未签名构建首次打开需右键 →「打开」，或
`xattr -dr com.apple.quarantine /Applications/LocalProxy.app`。

**从源码运行**（含调试、CLI 用法、打包安装包）见 [BUILD.md](BUILD.md)。

## 基本使用

1. 启动 App：菜单栏出现 LocalProxy 图标，本地核心自动拉起（SOCKS5 1080 / HTTP 1081）；
2. 点击图标打开 popover → 「+ 添加」选择应用（macOS `.app` / Windows `.exe`）；
3. 点击应用的「代理启动」，由当前平台的注入层接管已覆盖的网络 API，并按规则分流；
4. 右键托盘图标 →「设置」：配置监听、上游、路由规则、DNS/UDP、应用、高级项。
   设置页的改动在点击「保存」后生效（`⌘/Ctrl + S` 亦可），标题栏会提示
   「未保存的更改」；「重新加载」丢弃草稿并读取磁盘上的当前配置。

设置窗口的分区：

- **监听**：监听地址、SOCKS5 / HTTP CONNECT / 控制面端口、日志级别。桌面核心会避开被占用的端口，
  面板显示实际端口，这里填写的是偏好值。
- **上游代理**：左侧列表列出全部上游（类型 · 端点），点选查看详情；「设为使用」切换激活上游
  （保存后生效）。新增支持预设（Clash/mihomo、v2rayN、Surge、Stash、Shadowsocks、SOCKS5、HTTP）
  与「手动添加节点」。删除后可在同一次编辑里「撤销」。
- **路由规则**：见下方「路由规则」，规则表可编辑全部匹配字段。
- **DNS 与 UDP**：解析模式、UDP 模式与降级、IPv6、允许代理的端口（留空 = 全部；全部无效输入会保留原值）。
- **应用**：登记、启用、编辑名称/备注/附加启动参数、移除。
- **高级**：界面语言（保存后即时生效）、开机自启、出口 IP 探测端点与超时。

## 上游配置

默认上游 `127.0.0.1:7890`（socks5，Clash 类代理软件常用）。支持多上游，
菜单栏面板和设置均可切换。新 TCP 连接和 UDP 关联使用切换后的上游，已建立的连接继续使用原上游。
监听地址和端口在核心重启后生效。

### 连接现有代理软件

在「设置 → 上游代理」选择预设并点击「添加上游」，按客户端当前监听配置修改地址和端口。下面的端口是预设值。

| 客户端                           | 预设代理端口                         | 节点选择                          |
|:------------------------------|:-------------------------------|:------------------------------|
| Clash Verge / ClashX / mihomo | SOCKS5 / mixed `7890`          | 可启用 Clash 兼容控制接口              |
| Stash（macOS）                  | SOCKS5 / mixed `7890`          | 可启用 Clash 兼容控制接口              |
| v2rayN                        | SOCKS5 / mixed `10808`         | 沿用客户端当前策略。客户端启用兼容控制接口时可手填     |
| Surge（macOS）                  | SOCKS5 `6153`，也可手填 HTTP `6152` | 沿用 Surge 当前策略，需在 Surge 启用相应监听 |
| Shadowsocks Windows           | SOCKS5 `1080`                  | 沿用客户端当前策略                     |
| sing-box、Xray、Hiddify 或其他客户端  | 填写实际 SOCKS5 / HTTP CONNECT 监听  | 沿用客户端当前策略                     |

### 在 LocalProxy 中切换 Clash 节点

1. 确认 Clash 的控制面监听方式：
   - mihomo / ClashX / Clash for Windows：TCP 回环端口，如 `127.0.0.1:9090`，Secret 可能为空；
   - **Clash Verge Rev：默认使用 Unix domain socket**（`verge-mihomo -ext-ctl-unix
     /var/folders/.../verge-mihomo.sock`），配置模板里的 `9097` 通常**并未监听 TCP**，
     且 Secret 是随机值。
2. 在 LocalProxy「设置 → 上游代理」填写该 Clash 实例的代理地址与端口（通常是 `mixed-port`，如 `7897`），
   启用「Clash / mihomo 节点选择」。控制地址可以填：
   - `127.0.0.1:9097`（TCP，仅回环）；
   - `unix:/path/to/verge-mihomo.sock`（Unix socket）。
3. 不确定时点「自动检测」：它会读 Clash 的运行配置拿 Secret，扫描进程参数与临时目录里的
   `mihomo`/`clash` socket，并用 `/version` 校验，成功后自动填入。
4. 打开菜单栏面板的「Clash 节点」，选择策略组和节点。「刷新」会读取 Clash 当前的选择及订阅更新后的列表。
   卡片报错时可以直接点「自动检测并修复」，不需要再回设置页。

LocalProxy **不需要**开启 Clash 的系统代理或 TUN：它只连接 Clash 的代理端口（转发流量）
与控制端口（切换节点），系统代理、PAC、TUN 都由 Clash 自己管理。

规则模式下，Clash 按目标匹配策略组。全局模式下，`GLOBAL` 策略组决定出口。在 LocalProxy 切换节点会更新 Clash
的共享策略组，也会影响使用该组的其他流量。订阅导入和更新在 Clash 中完成。可用策略组取决于当前订阅配置。

### 手动添加独立节点

1. 在「设置 → 上游代理」点击「手动添加节点」，填写名称、节点服务器、端口和对应协议字段。
2. 使用系统中已安装的 mihomo。路径留空会查找同目录、PATH 和已知的 Clash Verge 安装位置。也可以手动选择内核可执行文件。
3. 在「传输与协议高级参数」填写单节点的 Mihomo JSON 对象，例如 `{"tls":true,"network":"ws","ws-opts":{"path":"/proxy"}}`
   。不同协议、传输、加密和扩展字段由所选 mihomo 版本解析。保存后可以在菜单栏和其他上游之间切换。

支持 Shadowsocks / ShadowsocksR、VMess、VLESS、Trojan、Hysteria2、TUIC、AnyTLS、WireGuard、Snell、SSH、HTTP(S) 和 SOCKS5（含
TLS）。HTTP 与 SOCKS5 常规上游可以直接填写，无需启动
mihomo。「检查节点配置」验证内核能否加载并启动该节点，实际出口可通过应用或出口探测确认。 [Mihomo 单节点参数](https://wiki.metacubex.one/config/proxies/)

独立节点使用单独的临时配置及带随机认证的回环 SOCKS5 监听。切换时，新连接进入新上游，旧连接仍持有原内核。桌面核心退出后回收所启动的独立内核。节点配置和密钥留在本机配置文件中。

## 路由规则

- 默认动作：`proxy`（全部走代理）或 `direct`（全部直连）；
- 规则字段：域名（支持 `.cn` 后缀与 `*.example.com` 通配）、IPv4/IPv6 CIDR、
  端口（`443` / `10000-20000`）、协议（tcp/udp）；
- 优先级：`order`（数组序，首个命中生效）或 `number`（priority 降序）；
  设置页的规则表可编辑全部字段（域名、IPv4/IPv6 CIDR、端口、协议、优先级），
  `order` 模式下用上移/下移调整顺序，`number` 模式下才启用优先级输入；
- 可用「启用路由规则」整体关闭规则表（关闭后按默认动作处理）；
- 内网/回环地址默认自动直连（可关闭）。

## DNS 和 UDP 策略

`dns_mode=proxy` 对已接管的公网域名查询返回 FakeIP，连接时将域名交给核心分流。`dns_mode=direct` 将新查询交回系统解析器，已缓存的
FakeIP 仍可用于连接。显式直连规则或节点服务器需要在本地解析域名时，也会使用系统 DNS。

默认 `udp_mode=auto`：按当前上游类型解析——SOCKS5 上游为 `proxy`（按规则分流），
HTTP 上游不支持 UDP，按 `udp_fallback` 处理（默认 `block`）。

| `udp_mode`  | 行为                                                   | 适用                            |
|:------------|:-----------------------------------------------------|:------------------------------|
| `block`     | 阻断公网 UDP，返回 `ENETUNREACH` 促使应用回退 TCP                 | 防泄漏优先（API/网页场景，QUIC 自动回落 TCP） |
| `proxy`     | 核心按路由规则处理每个 UDP 报文，命中代理的报文通过 SOCKS5 UDP Associate 转发 | 需要 UDP 分流的场景                  |
| `direct`    | UDP 直连放行                                             | 排障对照                          |
| `auto`（默认） | 按上游类型解析为 `proxy`，或按 `udp_fallback` 处理                 | 多数场景                          |

注入层对局域网、回环和标准 mDNS 组播保留直连。手工连接核心 SOCKS5 端口的客户端遵循核心的 UDP 策略。
核心中继也检查「允许代理的端口」。

桌面配置热重载后，已启动应用的下一次受支持 DNS 查询或 UDP 发送即采用新策略。切到阻断后，已连接的公网 UDP socket
也会在受支持的发送入口被阻断。核心退出后，发布的策略自动失效，旧会话停止向已释放的核心 TCP 端口拨号。

## 风控自检

popover「风控自检」卡片（探测默认关闭，在设置 → 高级开启后点击触发）：

- **出口探测**：通过核心的当前路由访问配置的 HTTP 端点，显示该请求的出口 IP / 国家 / 城市 / ISP；
- **一致性校验**：出口国家 vs 系统时区 vs 系统语言，任一匹配即一致，都不匹配给警告
  （对应 Certta/Verisoul 等平台的 Timezone Mismatch 检测规则）；
- **打开 BrowserLeaks**：用系统默认浏览器打开检测页。要验证指定应用，请在该应用中访问检测页。

「最近连接」折叠列表展示经过核心的连接目标、路由（proxy/direct/blocked）与命中规则，
该列表用来确认进入核心的流量及路由结果。

## 共存与泄漏边界

- **单实例**：App 用配置目录下的 `app.lock` 保证同时只有一个实例，第二个实例给出提示后退出
  （macOS 显示系统通知），不会出现两个菜单栏图标争抢端口。退出后立刻重启时锁会在 1.5 秒内重试，
  不会误判为「已在运行」。
- **并行多实例**：设置环境变量 `LOCALPROXY_CONFIG_DIR` 指向独立目录即可再开一个实例，
  它使用自己的 `config.json`、单实例锁与端口避让，因此可以与默认安装并存（多账号、测试）。
  CLI 的 `localproxy-core --config` 本来就按文件路径工作，不受单实例限制。
- **端口**：桌面核心检测已配置或被其他服务占用的端口，自动使用空闲回环端口，菜单栏显示实际端口。偏好端口保留在设置中，重启时重新尝试。CLI
  使用明确配置的监听端口。
- **系统设置**：LocalProxy 仅从自己启动的应用和内核环境中移除代理环境变量。系统代理、PAC、DNS、TUN、路由表及现有代理进程由原软件管理。
- **停止应用**：菜单栏的停止操作使用启动时保留的进程身份，仅作用于该次启动的进程组或 Job Object。
- **外部代理策略**：切换 Clash 兼容策略组会影响同组的其他流量。需要为 LocalProxy 单独选出口时，使用独立节点。
- **已覆盖的出站**：代理协商失败时拒绝连接。macOS 的 FakeIP 解析失败时也会返回错误，公网地址反查使用数字地址或返回无名称，避免向系统发出
  PTR 查询。
- **边界**：应用可读取系统代理或使用未覆盖的 DNS / 网络 API，外部 TUN 也可能捕获 LocalProxy 或独立 mihomo
  的出站。独立节点服务器使用域名时仍可能需要系统解析。时区、语言、浏览器指纹仍由应用和系统决定。
- **局域网与 UDP**：回环、私网和本地域名保留既有放行行为。显式的 `direct` 规则和 UDP 直连设置允许相应流量直连。Windows
  注入层在受支持的 socket 入口阻断必须代理的公网 UDP，完整 UDP 中继可由应用直接使用 LocalProxy 的 SOCKS5 端口。Windows 的网络
  API 覆盖与 macOS 不同。

## 已知限制（不开 TUN 的固有权衡）

| 限制                      | 说明                                                                                                     |
|:------------------------|:-------------------------------------------------------------------------------------------------------|
| SIP 保护的系统二进制不可注入        | `/usr/bin/*`、`/bin/*` 等 Apple 平台二进制会被剥离 `DYLD_INSERT_LIBRARIES`；请使用 Homebrew/nvm 等非系统版本                |
| hardened runtime 应用不可注入 | 启用了 `runtime` 标志的 app（部分官方 Node/Electron 构建）会被 dyld 拒绝加载库；launcher 会提前检测并提示，可改用 `--proxy-server` 类启动参数 |
| 仅代理「从菜单栏启动」的应用          | 已运行的应用、系统服务不在管辖范围（这是不开 TUN 的前提）                                                                        |
| Windows 注入受安全软件影响       | 部分 AV/EDR 拦截远程线程注入，会有明确错误提示                                                                            |

## 配置文件

`config.json`（macOS `~/Library/Application Support/local-proxy/`，
Windows `%APPDATA%/local-proxy/`）由 App 自动维护，也可手改：

```jsonc
{
  "socks5_port": 1080,
  "http_port": 1081,
  "control_port": 1082,
  "upstream": {
    "active": "clash",
    "servers": [{ "id": "clash", "name": "Clash", "type": "socks5",
                   "host": "127.0.0.1", "port": 7890 }]
  },
  "routing": {
    "enabled": true,
    "priority_mode": "order",
    "default_action": "proxy",
    "use_default_private": true,
    "rules": [{
      "id": "rule-0",                 // 界面行 key；缺失时自动补齐
      "name": "内网域名",
      "enabled": true,
      "action": "direct",
      "priority": 0,                  // 仅 priority_mode=number 时参与排序
      "domains": [".corp.example.com"],
      "ip_cidrs_v4": ["10.10.0.0/16"],
      "ip_cidrs_v6": [],
      "ports": ["443"],
      "protocols": ["tcp"]            // 空数组 = tcp/udp 都匹配
    }]
  },
  "dns_mode": "proxy",                // DNS 模式的唯一来源
  "proxy_rules": { "udp_mode": "auto", "udp_fallback": "block" },
  "apps": [{ "id": "chrome", "name": "Chrome", "path": "/Applications/Google Chrome.app" }]
}
```

旧配置里的 `proxy_rules.dns_mode` 仍可读取：加载时会提升为顶层 `dns_mode`，
保存后只保留顶层字段。

同目录还有 `app.lock`（单实例锁，进程退出自动释放）。整个目录可用环境变量
`LOCALPROXY_CONFIG_DIR` 覆盖，用于并行多实例或测试。
