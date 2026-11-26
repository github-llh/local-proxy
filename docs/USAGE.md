# 使用说明

## 安装与运行

开发模式：

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build -p localproxy-core -p localproxy-shim-macos
codesign -s - target/debug/liblocalproxy_shim_macos.dylib   # arm64 必需
cd src-tauri && ../ui/node_modules/.bin/tauri dev
```

核心也可独立运行（无 UI）：

```bash
target/debug/localproxy-core --config ./config.json
# curl -x socks5://127.0.0.1:1080 https://example.com
```

## 基本使用

1. 启动 App：菜单栏出现 LocalProxy 图标，本地核心自动拉起（SOCKS5 1080 / HTTP 1081）；
2. 点击图标打开 popover → 「+ 添加」选择应用（macOS `.app` / Windows `.exe`）；
3. 点击应用的「代理启动」：该应用以代理模式启动，全部网络流量按规则分流；
4. 右键托盘图标 →「设置」：配置上游、路由规则、DNS/UDP、开机自启。

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

1. 在 Clash / mihomo 中开启本机 `external-controller`，如 `127.0.0.1:9090`，并确认 `secret`。代理端口通常是 `mixed-port`，如
   `7890`。
2. 在 LocalProxy「设置 → 上游代理」填写该 Clash 实例的代理地址和端口，启用「Clash / mihomo 节点选择」，填写控制地址与 Secret
   后保存。控制地址接受本机回环 IP 与端口，Secret 随应用配置保存在本机。
3. 打开菜单栏面板的「Clash 节点」，选择策略组和节点。「刷新」会读取 Clash 当前的选择及订阅更新后的列表。

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
- 内网/回环地址默认自动直连（可关闭）。

## UDP 策略（WebRTC/QUIC）

| `udp_mode`  | 行为                                             | 适用                            |
|:------------|:-----------------------------------------------|:------------------------------|
| `block`（默认） | 阻断公网 UDP，返回 `ENETUNREACH` 促使应用回退 TCP           | 防泄漏优先（API/网页场景，QUIC 自动回落 TCP） |
| `proxy`     | SOCKS5 UDP Associate 全代理（ASSOCIATE + 双向 relay） | WebRTC 实时音视频等必须 UDP 的场景       |
| `direct`    | UDP 直连放行                                       | 排障对照                          |

局域网/回环/mDNS 的 UDP 在任何模式下均直连放行（不误伤本地投屏、mDNS 发现等）。
`auto` 会按上游类型解析为 proxy（SOCKS5 上游）或按 `udp_fallback` 处理。

## 风控自检

popover「风控自检」卡片（探测默认关闭，在设置 → 高级开启后点击触发）：

- **出口探测**：通过核心的当前路由访问配置的 HTTP 端点，显示该请求的出口 IP / 国家 / 城市 / ISP；
- **一致性校验**：出口国家 vs 系统时区 vs 系统语言，任一匹配即一致，都不匹配给警告
  （对应 Certta/Verisoul 等平台的 Timezone Mismatch 检测规则）；
- **打开 BrowserLeaks**：用系统默认浏览器打开检测页。要验证指定应用，请在该应用中访问检测页。

「最近连接」折叠列表展示经过核心的连接目标、路由（proxy/direct/blocked）与命中规则，
该列表用来确认进入核心的流量及路由结果。

## 共存与泄漏边界

- **端口**：桌面核心检测已配置或被其他服务占用的端口，自动使用空闲回环端口，菜单栏显示实际端口。偏好端口保留在设置中，重启时重新尝试。CLI
  使用明确配置的监听端口。
- **系统设置**：LocalProxy 仅从自己启动的应用和内核环境中移除代理环境变量。系统代理、PAC、DNS、TUN、路由表及现有代理进程由原软件管理。
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
  "upstream": {
    "active": "clash",
    "servers": [{ "id": "clash", "name": "Clash", "type": "socks5",
                   "host": "127.0.0.1", "port": 7890 }]
  },
  "routing": { "default_action": "proxy", "use_default_private": true },
  "dns_mode": "proxy",
  "proxy_rules": { "udp_mode": "auto", "udp_fallback": "block" },
  "apps": [{ "id": "chrome", "name": "Chrome", "path": "/Applications/Google Chrome.app" }]
}
```
