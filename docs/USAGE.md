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
popover/设置中可热切换（不影响已建连接）。首次启动未配置时，按本机实际
代理软件端口修改。

## 路由规则

- 默认动作：`proxy`（全部走代理）或 `direct`（全部直连）；
- 规则字段：域名（支持 `.cn` 后缀与 `*.example.com` 通配）、IPv4/IPv6 CIDR、
  端口（`443` / `10000-20000`）、协议（tcp/udp）；
- 优先级：`order`（数组序，首个命中生效）或 `number`（priority 降序）；
- 内网/回环地址默认自动直连（可关闭）。

## 已知限制（不开 TUN 的固有权衡）

| 限制 | 说明 |
|:--|:--|
| SIP 保护的系统二进制不可注入 | `/usr/bin/*`、`/bin/*` 等 Apple 平台二进制会被剥离 `DYLD_INSERT_LIBRARIES`；请使用 Homebrew/nvm 等非系统版本 |
| hardened runtime 应用不可注入 | 启用了 `runtime` 标志的 app（部分官方 Node/Electron 构建）会被 dyld 拒绝加载库；launcher 会提前检测并提示，可改用 `--proxy-server` 类启动参数 |
| 仅代理「从菜单栏启动」的应用 | 已运行的应用、系统服务不在管辖范围（这是不开 TUN 的前提） |
| Windows 注入受安全软件影响 | 部分 AV/EDR 拦截远程线程注入，会有明确错误提示 |

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
