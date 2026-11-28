<p align="center">
  <img src="app-icon.png" alt="LocalProxy Logo" width="120"/>
</p>

<h1 align="center">LocalProxy</h1>

<p align="center">
  <b>指定任意程序走代理 —— 不开 TUN、不要 root、泄漏零出口、菜单栏常驻</b>
</p>

<p align="center">
  <a href="docs/USAGE.md">使用</a> ·
  <a href="docs/BUILD.md">构建</a> ·
  <a href="docs/RELEASE.md">发版</a> ·
  <a href="docs/ARCHITECTURE.md">架构</a> ·
  <a href="docs/TROUBLESHOOTING.md">排障</a>
</p>

---

## 简介

LocalProxy 是一个轻量的跨平台本地代理（macOS 优先，Windows 适配中）。在菜单栏登记任意应用（`.app` / `.exe`
），以「代理模式」启动它——该应用的全部网络流量按规则分流到你的上游代理，**其他程序完全不受影响**。

不靠虚拟网卡、不靠 root/管理员权限，靠**启动注入**实现（macOS `DYLD_INSERT_LIBRARIES` + `__interpose` 段 / Windows DLL
注入 + MinHook）。

## 文档导航

| 我想…        | 看                                                  |
|:-----------|:---------------------------------------------------|
| 安装并日常使用    | [docs/USAGE.md](docs/USAGE.md)                     |
| 从源码构建、打安装包 | [docs/BUILD.md](docs/BUILD.md)                     |
| 发布新版本      | [docs/RELEASE.md](docs/RELEASE.md)                 |
| 了解内部实现     | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)       |
| 排查问题       | [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) |

## 核心特性

**per-app 代理**

- 菜单栏登记应用，一键「代理启动」；流量按规则分流（CIDR / 域名后缀与通配 / 端口 / 协议），内网自动直连
- 接入 Clash / mihomo、Stash、v2rayN、Surge、Shadowsocks Windows 等客户端的 SOCKS5 / HTTP 端口，支持多上游热切换
- 手动添加 Shadowsocks、VMess、VLESS、Trojan、Hysteria2、TUIC、AnyTLS、WireGuard 等节点，由独立 mihomo 实例承载
- 本地核心同时是标准代理（SOCKS5 `1080` / HTTP CONNECT `1081`），CLI 可直接使用

**代理边界**

- 已接管的代理连接在协商失败时拒绝出站。透传应用域名给上游，macOS 的部分系统解析入口支持 FakeIP。
- 支持公网 IPv6 阻断和 UDP 策略，实际覆盖取决于平台与对应 API。回环、私网和显式直连规则保留。
- 时区、语言和浏览器环境由应用和系统决定。详见 [共存与泄漏边界](docs/USAGE.md#共存与泄漏边界)。

**风控自检**

- 出口 IP / 国家 / 城市 / ISP 探测（经上游出口，默认关闭、按需触发）
- **时区/语言一致性**校验：出口国家 vs 系统时区 vs 系统语言，不一致即警告（对应 Certta/Verisoul 等平台的 Timezone Mismatch
  检测规则）
- browserleaks 泄漏自检一键入口

**与代理软件共存**

- 桌面核心会避开已配置或已占用的代理端口，实际监听地址显示在菜单栏。仅清理自己启动的子进程继承的代理环境变量。
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

### 安装包

从 GitHub Release 下载对应平台产物：

- macOS：`LocalProxy_<version>_universal.dmg`（Intel + Apple Silicon 通用）
- Windows：`LocalProxy_<version>_x64_en-US.msi` 或 `LocalProxy_<version>_x64-setup.exe`

启动后菜单栏出现图标，本地核心自动拉起（SOCKS5 `1080` / HTTP CONNECT `1081` / 控制面 `1082`；
端口被占用时自动避让，实际端口显示在面板）。未签名构建首次打开需右键 →「打开」，或
`xattr -dr com.apple.quarantine /Applications/LocalProxy.app`。

### 从源码运行

```bash
# 前置：Rust stable、Node 22+、pnpm 10
export PATH="$HOME/.cargo/bin:$PATH"
pnpm --dir ui install --frozen-lockfile

# 完整应用（菜单栏 + 设置窗口）
cargo build -p localproxy-core -p localproxy-shim-macos
codesign -s - target/debug/liblocalproxy_shim_macos.dylib   # macOS 必需
cd src-tauri && ../ui/node_modules/.bin/tauri dev

# 仅核心（CLI，无 UI）
cargo run -p localproxy-core -- --config ./config.json
curl -x socks5://127.0.0.1:1080 https://example.com        # 另一个终端验证
```

### 打安装包

```bash
node scripts/build-bundle.mjs universal-apple-darwin   # macOS：.app + .dmg
node scripts/build-bundle.mjs x86_64-pc-windows-msvc   # Windows：.msi + nsis（需在 Windows 上执行）
```

不要直接跑 `tauri build`：核心与 shim 由构建脚本编译并注入 `bundle.resources`，直接构建的 App 会缺少它们。
完整说明、产物路径与自检命令见 [docs/BUILD.md](docs/BUILD.md)。

### 测试

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
pnpm --dir ui build
scripts/e2e/inject-e2e.sh     # macOS 注入链路端到端自检
```

## 使用

1. **添加应用**：popover「+ 添加」选择 `.app` / `.exe`（或手动编辑 `config.json` 的 `apps`）
2. **代理启动**：点击应用的「代理启动」——以注入模式启动，受支持的连接按核心规则分流
3. **选择上游**：在菜单栏面板切换 SOCKS5 / HTTP 上游，默认使用 `127.0.0.1:7890`。上游切换对新建连接生效。
4. **选择 Clash 节点**：在设置的上游代理中启用 Clash / mihomo 节点选择，填入控制地址和 Secret。保存后可从菜单栏面板选择策略组和节点，订阅继续由
   Clash 管理。参见 [使用指南](docs/USAGE.md#在-localproxy-中切换-clash-节点)。
5. **手动节点**：在上游设置中手动添加节点，使用本机 mihomo。参见 [独立节点说明](docs/USAGE.md#手动添加独立节点)。
6. **规则**：设置窗口的规则表可编辑域名（`.cn` 后缀、`*.example.com` 通配）、IPv4/IPv6 CIDR、
   端口（`443`、`10000-20000`）、协议与优先级；`order` 模式用 ↑↓ 调整顺序，`number` 模式按优先级降序；
   内网/回环默认自动直连。

## 安全模型

- LocalProxy 和独立内核的监听限制在本机回环。独立内核使用随机认证，与其他代理隔离。
- 已接管的 TCP 连接和 SOCKS5 UDP 关联在代理失败时返回错误。配置的直连规则继续生效。
- 「最近连接」展示进入本地核心的目标、路由、规则和结果。其他代理的系统设置和 TUN 路由由对应软件管理。

## 配置

`config.json`（macOS `~/Library/Application Support/local-proxy/`，Windows `%APPDATA%/local-proxy/`）：

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
    "rules": [{ "id": "rule-0", "name": "内网域名", "action": "direct",
                "domains": [".corp.example.com"], "ports": ["443"],
                "protocols": ["tcp"] }]
  },
  "dns_mode": "proxy",
  "proxy_rules": { "udp_mode": "auto", "udp_fallback": "block", "ipv6_mode": "proxy" },
  "apps": [{ "id": "chrome", "name": "Chrome",
             "path": "/Applications/Google Chrome.app" }]
}
```

完整字段、旧配置迁移与规则语义见 [docs/USAGE.md](docs/USAGE.md#配置文件)。
`LOCALPROXY_CONFIG_DIR` 可指向独立目录以并行运行多个实例。

## 平台支持与已知限制

| 平台                  | 状态                                         |
|:--------------------|:-------------------------------------------|
| macOS（arm64/x86_64） | ✅ 完整（注入链路 e2e 实测）                          |
| Windows（x64）        | 🚧 代码完成；CI 覆盖三平台测试与 msi/nsis 构建，注入链路尚未实机验证 |

不开 TUN 的固有权衡：

- 仅代理**从菜单栏启动**的应用（已运行的应用、系统服务不在管辖范围）
- SIP 保护的系统二进制（`/usr/bin/*` 等）会在启动前被明确拒绝：dyld 会静默忽略注入，进程能启动却不走代理
- hardened runtime 的应用：Chromium 系（Chrome/Edge/Brave/Vivaldi/Opera）自动改用原生 `--proxy-server` +
  独立配置目录；其他应用仍不可注入，会给出可操作的错误
- 详见 [docs/USAGE.md](docs/USAGE.md) 与 [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md)

## 开发

构建、测试、打包与产物自检的完整流程见 [docs/BUILD.md](docs/BUILD.md)；发版流程见
[docs/RELEASE.md](docs/RELEASE.md)。提交前的最低要求：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm --dir ui build
```

## 许可

BSD-2-Clause（含 vendored MinHook，© Tsuda Kageyu）。
