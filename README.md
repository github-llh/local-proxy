<p align="center">
  <img src="app-icon.png" alt="LocalProxy Logo" width="120"/>
</p>

<h1 align="center">LocalProxy</h1>

<p align="center">
  <b>🚀 指定任意程序走代理 —— 不开 TUN、不要 root、菜单栏常驻</b>
</p>

---

## 简介

LocalProxy 是一个轻量的跨平台本地代理（macOS 优先，Windows 适配中）：

- **per-app 代理**：在菜单栏登记任意应用（`.app` / `.exe`），以「代理模式」启动它，其全部网络流量按规则分流到你的上游代理；其他程序完全不受影响；
- **不开 TUN**：无虚拟网卡、无 root/管理员权限，靠启动注入（macOS `DYLD_INSERT_LIBRARIES` + interpose / Windows DLL 注入 + MinHook）实现；
- **本地代理核心**：内置 SOCKS5（默认 `127.0.0.1:1080`）+ HTTP CONNECT（`1081`），也供遵守代理设置的 CLI / 浏览器手动使用；
- **规则路由**：CIDR / 域名后缀与通配符 / 端口 / 协议分流，内网自动直连；
- **菜单栏形态**：托盘图标 + 弹出面板（状态、上游切换、出口 IP、流量、运行中应用）+ 完整设置窗口；
- **桌面端配置**：Tauri v2 构建，Claude 风格界面，zh-CN / English。

## 架构

```
菜单栏控制中心 (Tauri + React)
   └── 本地代理核心 (Rust: SOCKS5/HTTP + 规则 + 上游级联 + 控制面)
           └── per-app 启动注入 (shim: macOS interpose dylib / Windows MinHook DLL)
```

## 开发

```bash
# 前置：Rust stable（~/.cargo/bin 加入 PATH）、Node 22+、pnpm 9+
cargo test --workspace          # 全量测试
cargo run -p localproxy-core    # 独立运行代理核心
pnpm --dir ui dev               # 前端开发服务器
pnpm --dir src-tauri tauri dev  # 完整应用（开发模式）
```

## 配置

`config.json`（macOS `~/Library/Application Support/local-proxy/`，Windows `%APPDATA%/local-proxy/`）：

```jsonc
{
  "socks5_port": 1080,
  "http_port": 1081,
  "upstream": {
    "active": "clash",
    "servers": [{ "id": "clash", "name": "Clash", "type": "socks5", "host": "127.0.0.1", "port": 7890 }]
  },
  "routing": { "default_action": "proxy", "use_default_private": true },
  "apps": [{ "id": "chrome", "name": "Chrome", "path": "/Applications/Google Chrome.app" }]
}
```

## 许可

BSD-2-Clause（含 vendored MinHook，© Tsuda Kageyu）。
