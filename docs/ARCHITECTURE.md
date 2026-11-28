# LocalProxy 架构

## 总览

```
┌──────────────────────────────────────────────────────────────┐
│ ① 菜单栏控制中心（src-tauri，Tauri v2 + React/TS）            │
│   tray 图标（左键 popover / 右键菜单）                        │
│   popover：核心状态、上游切换、运行中应用、应用快速启动        │
│   settings：监听 / 上游 / 路由规则 / DNS·UDP / 高级            │
│   启动时拉起 ②（sidecar 子进程），退出时结束子进程与临时配置   │
└──────────────────────────┬───────────────────────────────────┘
                           │ std::process（spawn localproxy-core）
┌──────────────────────────▼───────────────────────────────────┐
│ ② 本地代理核心（crates/localproxy-core，Rust/tokio）          │
│   SOCKS5 服务（默认 127.0.0.1:1080）/ HTTP CONNECT（1081）    │
│   规则引擎：CIDR v4/v6 · 域名后缀/通配 · 端口 · 协议           │
│   上游级联：SOCKS5/HTTP + 账密认证，多上游热切换               │
│   统计：连接数/字节/直连代理计数（控制面暴露）                 │
└──────────────────────────┬───────────────────────────────────┘
                           │ 标准 SOCKS5（ATYP_DOMAIN 透传域名）
┌──────────────────────────▼───────────────────────────────────┐
│ ③ per-app 启动注入（crates/localproxy-launcher + shim）       │
│   launcher macOS：解析 .app 主二进制 → posix_spawn +          │
│     DYLD_INSERT_LIBRARIES 注入（不用 open：会清理该环境变量）  │
│   launcher Windows：CreateProcessW(CREATE_SUSPENDED) +        │
│     远程线程 LoadLibraryW 注入 + 远程调 init 装 hook          │
│   shim（Rust cdylib）：                                        │
│     macOS：__interpose 段 hook connect/getaddrinfo            │
│     Windows：vendored MinHook hook ws2_32                     │
└──────────────────────────────────────────────────────────────┘
```

## 数据流（TCP）

1. 应用调用 `getaddrinfo("example.com")` → shim 分配 FakeIP（198.18.x.x），
   记录 `FakeIP → example.com` 映射；应用**不发出真实 DNS 查询**；
2. 应用调用 `connect(FakeIP, 443)` → shim 查表得域名，把**原 socket**
   连到本地核心，并在其上完成 SOCKS5 协商（ATYP_DOMAIN 透传域名）；
3. 核心按规则分流：`direct` 本地解析直连 / `proxy` 级联到上游；
4. 应用后续 `send`/`recv` 对原 socket 的操作自然流经代理。

UDP：`udp_mode=auto` + SOCKS5 上游时走 UDP Associate；其它上游按
`udp_fallback`（block/direct）处理。局域网 UDP 与 TCP 一致直连。

## crate 职责

| crate                   | 职责                                                |
|:------------------------|:--------------------------------------------------|
| `localproxy-net`        | 共享协议库：SOCKS5/HTTP 客户端、FakeIP、域名映射、本地目标判定、纯协议构造/解析 |
| `localproxy-core`       | 代理核心：服务、规则、上游、DNS、统计、CLI                          |
| `localproxy-shim`       | shim 公共逻辑：FakeIP 映射、SockIo 抽象、就地协商、重入防护           |
| `localproxy-shim-macos` | `#![cfg(target_os="macos")]` interpose dylib      |
| `localproxy-shim-win`   | `#![cfg(windows)]` MinHook dylib                  |
| `localproxy-launcher`   | 平台启动器（.app 解析/exec、CreateProcess 注入）              |
| `src-tauri`             | Tauri 壳：tray/窗口/命令层/状态                            |

## 关键设计决策

1. **原 socket 就地协商**（而非 dup2 换 fd）：Windows SOCKET 无 fd 替换
   等价物，就地协商是两平台唯一一致方案；
2. **shim 持有 FakeIP 映射，核心无状态**：核心是标准 SOCKS5/HTTP 代理，
   域名经 ATYP_DOMAIN 透传；shim 保持轻薄，核心可独立测试与 CLI 使用；
3. **协议字节收敛在 net crate，IO 平台化**：`SockIo` trait 抽象
   connect/read/write，协议构造/解析为纯函数，单测覆盖；
4. **平台源码整体 cfg 隔离**：`shim-macos` 与 `shim-win` 分别
   `#![cfg(target_os=...)]` / `#![cfg(windows)]`，任一机器上
   `cargo check --workspace` 都不会构建对方平台源码；
5. **配置单一事实来源**：`localproxy-core::config::AppConfig`，
   App 与 CLI 共用；所有非法值 warn + 回退默认（移植旧项目 fail-safe 语义）。
6. **单实例 + 写串行化**：App 用配置目录下的 `app.lock`（`File::try_lock`）
   保证只有一个实例，第二个实例直接退出；`LOCALPROXY_CONFIG_DIR` 指向独立
   配置目录即可并行运行（各自配置、锁与端口避让）。配置写入统一走
   `commit_config`（`apply_lock` 串行 → 落盘 → 更新内存 → 通知核心重载），
   控制面等待不持有配置锁，应用登记等本地写入走 `mutate_local` 全程持锁。
