<p align="center">
  <img src="app-icon.png" alt="LocalProxy Logo" width="120"/>
</p>

<h1 align="center">LocalProxy</h1>

<p align="center">
  <b>指定任意程序走代理 —— 不开 TUN、不要 root、泄漏零出口、菜单栏常驻</b>
</p>

<p align="center">
  <a href="docs/ARCHITECTURE.md">架构</a> ·
  <a href="docs/USAGE.md">使用</a> ·
  <a href="docs/TROUBLESHOOTING.md">排障</a>
</p>

---

## 简介

LocalProxy 是一个轻量的跨平台本地代理（macOS 优先，Windows 适配中）。在菜单栏登记任意应用（`.app` / `.exe`
），以「代理模式」启动它——该应用的全部网络流量按规则分流到你的上游代理，**其他程序完全不受影响**。

不靠虚拟网卡、不靠 root/管理员权限，靠**启动注入**实现（macOS `DYLD_INSERT_LIBRARIES` + `__interpose` 段 / Windows DLL
注入 + MinHook）。

## 核心特性

**per-app 代理**

- 菜单栏登记应用，一键「代理启动」；流量按规则分流（CIDR / 域名后缀与通配 / 端口 / 协议），内网自动直连
- 上游支持 SOCKS5 / HTTP（含账密认证），多上游**热切换** + TCP 延迟测速
- 本地核心同时是标准代理（SOCKS5 `1080` / HTTP CONNECT `1081`），CLI 可直接使用

**泄漏零出口（默认 fail-closed）**

| 泄漏面    | 机制                                                                                                                                                          |
|:-------|:------------------------------------------------------------------------------------------------------------------------------------------------------------|
| DNS    | `getaddrinfo` / `gethostbyname(2)` / `GetAddrInfoW` 统一返回 FakeIP（198.18/15），域名经 SOCKS5 ATYP_DOMAIN 透传，应用**不发出真实 DNS 查询**；`getnameinfo` 反查还原域名，不暴露 FakeIP 字面量 |
| WebRTC | STUN/UDP 默认**阻断**（`ENETUNREACH`，促使 QUIC→TCP、WebRTC 降级 TCP candidate）；`udp_mode=proxy` 时可全代理（ASSOCIATE + 双向 relay）                                           |
| IPv6   | `ipv6_mode=block` 阻断公网 v6（`EAFNOSUPPORT`），封堵 host candidate 的 IPv6 全局地址                                                                                     |
| 子进程    | macOS 经环境变量天然继承；Windows hook `CreateProcess` 主动注入（Chromium network_service 等多进程覆盖）                                                                          |
| 协商失败   | 一律拒绝（`ECONNREFUSED`/`ENETUNREACH`），**绝不静默直连**                                                                                                               |

**风控自检**

- 出口 IP / 国家 / 城市 / ISP 探测（经上游出口，默认关闭、按需触发）
- **时区/语言一致性**校验：出口国家 vs 系统时区 vs 系统语言，不一致即警告（对应 Certta/Verisoul 等平台的 Timezone Mismatch
  检测规则）
- browserleaks 泄漏自检一键入口

**与代理软件共存**

- 与 Clash Verge / ClashX / mihomo / V2RayN 同时运行：端口分工（本核心 1080-1082）、启动时清理系统代理环境变量（不双跳）、*
  *防自环校验**（上游误配自身端口直接拒绝）
- 本方案为 socket 透传、**不做 MITM**，被代理应用的 TLS 指纹保持原生

**菜单栏形态**

- 托盘图标（三态）+ 弹出面板：核心状态、上游快切、出口探测、最近连接、运行中应用
- 设置窗口：应用管理 / 上游 / 路由规则 / DNS·UDP / 高级
- Tauri v2 + Claude 风格界面，zh-CN / English

## 工作原理

```
┌──────────────────────────────────────────────────────────────┐
│ 菜单栏控制中心 (Tauri v2 + React)                             │
│   tray 图标 + popover（状态/上游/风控/连接）+ 设置窗口         │
└──────────────────────────┬───────────────────────────────────┘
                           │ 启动时拉起 sidecar
┌──────────────────────────▼───────────────────────────────────┐
│ 本地代理核心 (Rust/tokio)                                     │
│   SOCKS5(1080) / HTTP CONNECT(1081) / 控制面(1082)            │
│   规则路由 · 上游级联 · UDP relay · 连接诊断 · 风控探测        │
└──────────────────────────┬───────────────────────────────────┘
                           │ 标准 SOCKS5（ATYP_DOMAIN 透传域名）
┌──────────────────────────▼───────────────────────────────────┐
│ per-app 启动注入                                              │
│   macOS: .app 解析 + exec + DYLD_INSERT_LIBRARIES             │
│   Windows: CreateProcess(suspended) + 远程线程注入            │
│   shim (Rust cdylib): interpose / MinHook                     │
└──────────────────────────────────────────────────────────────┘
```

**TCP 数据流**：`getaddrinfo("example.com")` 被拦截 → 返回 FakeIP 并记录映射（应用不发出真实 DNS）→ `connect(FakeIP, 443)`
被拦截 → 原 socket 就地连接核心并完成 SOCKS5 协商（域名透传）→ 核心按规则分流：直连本地解析 / 上游级联。
**UDP 数据流**：`sendto` 被拦截 → 默认阻断（应用回退 TCP）；`udp_mode=proxy` 时先 ASSOCIATE 建立 relay，封装目标后经核心中继。

## 快速开始

```bash
# 前置：Rust stable、Node 22+、pnpm 9+；前端依赖已装（ui/node_modules）
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --workspace

# 方式一：完整应用（菜单栏）——注意在 src-tauri 目录调用 ui 下的 CLI
codesign -s - target/debug/liblocalproxy_shim_macos.dylib   # macOS 首次必需
cd src-tauri && ../ui/node_modules/.bin/tauri dev

# 方式二：仅核心（CLI，无 UI）
cargo run -p localproxy-core
curl -x socks5://127.0.0.1:1080 https://example.com   # 另一个终端

# 一键自检（注入链路 14 组断言：UDP 封堵/放行、DNS 零泄露、
# FakeIP、非阻塞 connect、UDP 全代理、未注入对照）
scripts/e2e/inject-e2e.sh
```

## 使用

1. **添加应用**：popover「+ 添加」选择 `.app` / `.exe`（或手动编辑 `config.json` 的 `apps`）
2. **代理启动**：点击应用的「代理启动」——以注入模式启动，流量全部经核心分流
3. **选择上游**：在菜单栏面板切换 SOCKS5 / HTTP 上游，默认使用 `127.0.0.1:7890`。上游切换对新建连接生效。
4. **选择 Clash 节点**：在设置的上游代理中启用 Clash / mihomo 节点选择，填入控制地址和 Secret。保存后可从菜单栏面板选择策略组和节点，订阅继续由
   Clash 管理。参见 [使用指南](docs/USAGE.md#在-localproxy-中切换-clash-节点)。
5. **规则**：设置窗口编辑（域名 `.cn` 后缀、`*.example.com` 通配、CIDR、端口、协议），内网/回环自动直连。

## 安全模型

- **默认 fail-closed**：一切非局域网出站（TCP/UDP/DNS）必经核心；任何协商失败即拒绝，不静默直连
- **精确放行（不误伤）**：回环、私网（10/8、172.16/12、192.168/16、169.254/16、fc00::/7、fe80::/10）、mDNS（`.local`/`.localhost`/
  `.lan`/`.home`/`.internal`）、规则命中 `direct` 的流量行为不变——e2e 对每类均有回归断言
- **优雅降级**：阻断路径返回应用可理解的标准 errno（`ENETUNREACH` 促使 QUIC/WebRTC 回退 TCP，`EAFNOSUPPORT` 拒绝
  v6），不崩溃、不挂起；非阻塞 socket 的 connect 语义完整保留（临时切阻塞协商后恢复，Electron/Chromium 默认模式回归覆盖）
- **可审计**：popover「最近连接」展示每个连接的路由/规则/结果；泄漏投诉可举证

## 配置

`config.json`（macOS `~/Library/Application Support/local-proxy/`，Windows `%APPDATA%/local-proxy/`）：

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
  "proxy_rules": { "udp_mode": "block", "ipv6_mode": "proxy" },
  "apps": [{ "id": "chrome", "name": "Chrome",
             "path": "/Applications/Google Chrome.app" }]
}
```

## 平台支持与已知限制

| 平台                  | 状态                              |
|:--------------------|:--------------------------------|
| macOS（arm64/x86_64） | ✅ 完整（注入链路 e2e 实测）               |
| Windows（x64）        | 🚧 代码完成，CI 验证（编译由 CI 覆盖，未在本机验证） |

不开 TUN 的固有权衡：

- 仅代理**从菜单栏启动**的应用（已运行的应用、系统服务不在管辖范围）
- SIP 保护的系统二进制（`/usr/bin/*`）与启用 hardened runtime 的应用不可注入（launcher 启动前检测并提示替代方案）
- 详见 [docs/USAGE.md](docs/USAGE.md) 与 [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md)

## 开发

```bash
cargo test --workspace                       # 全量测试
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
pnpm --dir ui build                          # 前端
```

## 许可

BSD-2-Clause（含 vendored MinHook，© Tsuda Kageyu）。
