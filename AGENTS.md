# AGENTS.md

LocalProxy：跨平台本地代理（菜单栏 + per-app 启动注入，无 TUN）。

- 构建体系：Cargo workspace（Rust）+ Tauri v2（src-tauri）+ Vite/React（ui/）。
- 代码规范：`cargo fmt --all` + `cargo clippy --workspace -- -D warnings` 必须零警告；前端 `pnpm --dir ui build`（tsc + vite）必须通过。
- 测试：`cargo test --workspace` 全绿是提交前提；改动平台 `cfg` 代码后跑 `scripts/check-linux-cfg.sh`（Linux 分支的 dead_code / 未使用依赖只能这样在本地暴露）。
- 涉及网络/并发/时序的测试至少重复运行 10 次：单次通过不代表稳定（CI 不同平台的调度会暴露偶发失败）。
- 发布流程见 [Agent.md](Agent.md)。
- Rust 工具链位于 `~/.cargo/bin`（非登录 shell 需 `export PATH="$HOME/.cargo/bin:$PATH"`）。

## 目录速览

| 路径 | 职责 |
|:--|:--|
| `crates/localproxy-net` | 共享协议库：SOCKS5/HTTP 客户端、FakeIP、域名映射、本地目标判定 |
| `crates/localproxy-core` | 代理核心：SOCKS5/HTTP 服务、规则路由、上游级联、DNS、控制面、CLI |
| `crates/localproxy-shim` | 注入 shim 公共逻辑（平台无关） |
| `crates/localproxy-shim-macos` | macOS shim：DYLD_INSERT_LIBRARIES + `__interpose` 段 |
| `crates/localproxy-shim-win` | Windows shim：DLL 注入 + vendored MinHook |
| `crates/localproxy-launcher` | 平台启动器：`.app` 解析/exec、CreateProcess 注入 |
| `src-tauri` | Tauri 菜单栏应用（tray/popover/设置窗口/命令层） |
| `ui` | 前端（Vite + React + TS，Claude 风格） |
| `docs` | 文档：使用（USAGE）、构建（BUILD）、发版（RELEASE）、架构、排障 |
