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

### 在 LocalProxy 中切换 Clash 节点

1. 在 Clash / mihomo 中开启本机 `external-controller`，如 `127.0.0.1:9090`，并确认 `secret`。代理端口通常是 `mixed-port`，如
   `7890`。
2. 在 LocalProxy「设置 → 上游代理」填写该 Clash 实例的代理地址和端口，启用「Clash / mihomo 节点选择」，填写控制地址与 Secret
   后保存。控制地址接受本机回环 IP 与端口，Secret 随应用配置保存在本机。
3. 打开菜单栏面板的「Clash 节点」，选择策略组和节点。「刷新」会读取 Clash 当前的选择及订阅更新后的列表。

规则模式下，Clash 按目标匹配策略组。全局模式下，`GLOBAL` 策略组决定出口。在 LocalProxy 切换节点会更新 Clash
的共享策略组，也会影响使用该组的其他流量。订阅导入和更新在 Clash 中完成。可用策略组取决于当前订阅配置。

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

- **出口探测**：经上游访问 HTTP 端点，显示出口 IP / 国家 / 城市 / ISP——即被代理应用可见的出口身份；
- **一致性校验**：出口国家 vs 系统时区 vs 系统语言，任一匹配即一致，都不匹配给警告
  （对应 Certta/Verisoul 等平台的 Timezone Mismatch 检测规则）；
- **泄漏自检**：browserleaks.com 一键打开（检测浏览器指纹；注入应用的泄漏用「最近连接」佐证）。

「最近连接」折叠列表展示每个连接的目标、路由（proxy/direct/blocked）与命中规则，
排障与泄漏投诉时可举证 fail-closed 生效。

## 与 Clash / V2RayN 等代理软件共存

LocalProxy 定位是 **per-app 注入层**，Clash Verge / ClashX / mihomo / V2RayN 等作为
**上游分流层**，两者同时开、互不干扰：

- **端口不冲突**：核心默认 `1080/1081/1082`，Clash 默认 `7890/7891/7892`；若你改过
  Clash 端口产生冲突，在设置里改核心端口（启动前有端口预检与友好报错）；
- **上游指向 Clash**：默认上游 `127.0.0.1:7890`（Clash mixed 端口）。LocalProxy 的规则先决定流量是否交给 Clash。进入 Clash
  后，出口由 Clash 当前模式和策略组决定。
- **防自环**：上游若误配为本核心自己的端口（如链式代理配反），启动时直接报错拒绝，
  不会形成无限回环；
- **系统代理不干扰**：启动被代理应用时，launcher 会清理 `http_proxy`/`ALL_PROXY` 等
  环境变量——应用处于「无代理」环境，由 shim 全权接管，不会与 Clash 的系统代理双跳；
- **TUN 模式不冲突**：Clash 开 TUN 时，注入进程的连接在 socket 层就被 shim 拦截
  （先于内核网络栈），不进入 TUN 路由；未注入的进程仍走 Clash TUN，行为不变；
- **Fake-IP 段**：默认 `198.18.0.0/15` 与 Clash 默认 fake-ip-range 相同，但注入进程
  的解析被 hook 直接接管（不经过系统 DNS/Clash DNS 劫持），无实际冲突；如仍有
  顾虑可在设置修改 `fake_ip.cidr`。

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
