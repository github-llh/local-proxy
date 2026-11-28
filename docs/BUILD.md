# 构建指南

从源码到可运行的 App、再到可分发安装包的完整流程。发布版本（打 tag、CI 产物、签名公证）见
[RELEASE.md](RELEASE.md)。

## 1. 前置依赖

| 依赖 | 版本 / 说明 |
|:--|:--|
| Rust | stable（本仓库在 1.100 上验证）；工具链通常在 `~/.cargo/bin`，非登录 shell 需 `export PATH="$HOME/.cargo/bin:$PATH"` |
| Node.js | 22+ |
| pnpm | 10（`ui/pnpm-lock.yaml` 锁定依赖） |
| 平台构建依赖 | macOS：Xcode Command Line Tools；Windows：MSVC Build Tools + WebView2（Win10/11 自带）；Linux 仅用于 `cargo test`/`cargo clippy`，见 `.github/workflows/ci.yml` |

macOS universal 打包额外需要两个 Rust target：

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
rustup target list --installed | grep apple-darwin
```

首次克隆后安装前端依赖：

```bash
pnpm --dir ui install --frozen-lockfile
```

## 2. 关键约束：安装包必须用 `scripts/build-bundle.mjs`

`src-tauri/tauri.conf.json` 里是 `"bundle": { "active": false }`。核心 `localproxy-core` 与平台 shim
不是普通 Rust 依赖，而是由 [build-bundle.mjs](../scripts/build-bundle.mjs) 在打包时：

1. 按目标架构 `cargo build --release` 编译 `localproxy-core` 与当前平台 shim；
2. 多架构时用 `lipo` 合并成 universal；
3. macOS 上对二者做 `codesign --sign -`（无证书时的 ad hoc）；
4. 以 `--config` 注入 `bundle.active=true` 与 `bundle.resources`，再调用 `tauri build`。

因此**直接运行 `tauri build` 打出来的 `.app` 会缺少 `Contents/Resources/localproxy-core` 和
`liblocalproxy_shim_macos.dylib`**，安装后无法启动被代理应用。始终用脚本打包。

脚本参数：

| 参数 | 用途 |
|:--|:--|
| `universal-apple-darwin` | macOS 双架构（默认打 `app` + `dmg`） |
| `aarch64-apple-darwin` | macOS Apple Silicon 单架构（更快） |
| `x86_64-apple-darwin` | macOS Intel 单架构 |
| `x86_64-pc-windows-msvc` | Windows x64（默认打 `msi` + `nsis`） |

可选第二个参数覆盖 bundle 类型，例如只出 `.app` 不出 dmg：

```bash
node scripts/build-bundle.mjs aarch64-apple-darwin app
```

## 3. 开发构建（debug）

### 3.1 完整应用（菜单栏 + 设置窗口）

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build -p localproxy-core -p localproxy-shim-macos
codesign -s - target/debug/liblocalproxy_shim_macos.dylib   # macOS 必需，否则 dyld 拒绝加载
cd src-tauri && ../ui/node_modules/.bin/tauri dev
```

`tauri dev` 会启动 Vite（`ui`）并打开菜单栏应用；核心由 App 在启动时自动拉起。Windows 把
`localproxy-shim-macos` 换成 `localproxy-shim-win`，不需要手动签名。

开发模式的日志：核心输出到 stderr；shim 需要 `LP_LOG=debug`（由 launcher 注入）；App 侧
`RUST_LOG=debug`。

### 3.2 仅核心（CLI，无 UI）

```bash
cargo run -p localproxy-core                          # 使用内置默认配置
cargo run -p localproxy-core -- --config ./config.json # 指定配置文件
curl -x socks5://127.0.0.1:1080 https://example.com   # 另一个终端验证
```

CLI 不读取桌面 App 的 `config.json`（除非用 `--config` 显式指定），也不参与 App 的单实例锁；
需要与 App 并存时给它单独一份配置和端口。

### 3.3 测试与静态检查

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
pnpm --dir ui build        # tsc --noEmit + vite build
```

macOS 注入链路端到端自检（FakeIP、非阻塞 TCP、UDP 阻断/放行、SOCKS5 中继）：

```bash
cargo build -p localproxy-core -p localproxy-shim-macos
codesign -s - target/debug/liblocalproxy_shim_macos.dylib
scripts/e2e/inject-e2e.sh
```

改动只涉及某个 crate 时可以只跑相关子集，例如：

```bash
cargo test -p localproxy-core config::
cargo clippy -p localproxy-core -p localproxy-app --all-targets -- -D warnings
```

改动 `#[cfg(target_os = ...)]` / `#[cfg(windows)]` 相关代码时，额外跑一次 Linux 交叉检查：

```bash
scripts/check-linux-cfg.sh
```

macOS 上 `cargo clippy` 只编译 macOS 分支，仅在 Windows/Linux 生效的代码（以及被 cfg 掉的
常量、平台专用依赖）不会暴露问题。该脚本用 `x86_64-unknown-linux-gnu` target 交叉 clippy
（只做检查、不需要 GTK），覆盖 net / core / launcher / shim\* / inject；`src-tauri` 需要
webkit2gtk，由 CI 的 ubuntu job 覆盖。

## 4. 发布构建（安装包）

macOS：

```bash
export PATH="$HOME/.cargo/bin:$PATH"
pnpm --dir ui install --frozen-lockfile
node scripts/build-bundle.mjs universal-apple-darwin
```

Windows（在 Windows 上执行）：

```powershell
pnpm --dir ui install --frozen-lockfile
node scripts/build-bundle.mjs x86_64-pc-windows-msvc
```

前端不需要手动 `pnpm build`，`tauri build` 的 `beforeBuildCommand` 会执行；但 `pnpm install` 必须先做。

### 4.1 产物位置

| 平台 | 路径 |
|:--|:--|
| macOS 应用 | `target/universal-apple-darwin/release/bundle/macos/LocalProxy.app` |
| macOS 安装盘 | `target/universal-apple-darwin/release/bundle/dmg/*.dmg` |
| Windows 安装包 | `target/x86_64-pc-windows-msvc/release/bundle/msi/*.msi`、`.../nsis/*.exe` |

单架构构建把 `universal-apple-darwin` 换成对应 target 即可。

`.app` 内的资源布局：

```
LocalProxy.app/Contents/
├── MacOS/localproxy-app            # Tauri 主程序
└── Resources/
    ├── localproxy-core             # 代理核心（sidecar）
    └── liblocalproxy_shim_macos.dylib  # 注入 shim
```

### 4.2 打包自检

```bash
app=target/universal-apple-darwin/release/bundle/macos/LocalProxy.app
ls "$app/Contents/Resources"
lipo "$app/Contents/MacOS/localproxy-app" -verify_arch arm64 x86_64
lipo "$app/Contents/Resources/localproxy-core" -verify_arch arm64 x86_64
lipo "$app/Contents/Resources/liblocalproxy_shim_macos.dylib" -verify_arch arm64 x86_64
codesign --verify --deep --strict --all-architectures "$app"
open "$app"
```

`lipo` 的参数顺序是「先文件、后 `-verify_arch`」，写反会报
`unknown architecture specification flag`。

启动后应看到菜单栏图标、popover 显示核心运行中，且 SOCKS5/HTTP 端口有实际值。再点「代理启动」
选一个应用验证注入链路。

单架构构建时只校验对应架构（`arm64` 或 `x86_64`）。

### 4.3 签名与首次打开

未配置证书时脚本使用 ad hoc 签名（`APPLE_SIGNING_IDENTITY=-`），这足以在本机运行，但分发给他人时
macOS Gatekeeper 会拦截。接收方可以右键 →「打开」，或执行：

```bash
xattr -dr com.apple.quarantine /Applications/LocalProxy.app
```

正式分发需要 Developer ID 证书与公证，见 [RELEASE.md](RELEASE.md#6-签名与公证)。

## 5. 配置与数据位置

| 平台 | 默认目录 |
|:--|:--|
| macOS | `~/Library/Application Support/local-proxy/` |
| Windows | `%APPDATA%\local-proxy\` |
| Linux | `$XDG_CONFIG_HOME/local-proxy/`，未设置时 `~/.config/local-proxy/` |

目录内容：`config.json`（配置，含 control token 与上游密码，权限 600）、`app.lock`（单实例锁）。
环境变量 `LOCALPROXY_CONFIG_DIR` 可覆盖整个目录，用于并行多实例或测试：

```bash
LOCALPROXY_CONFIG_DIR=/tmp/lp-test "./LocalProxy.app/Contents/MacOS/localproxy-app"
```

## 6. 常见构建问题

| 现象 | 处理 |
|:--|:--|
| `tauri` 命令找不到 | 用 `ui/node_modules/.bin/tauri`，或在 `src-tauri` 目录下 `../ui/node_modules/.bin/tauri dev` |
| `请先执行 pnpm --dir ui install --frozen-lockfile` | 前端依赖未装，或 lockfile 与 `package.json` 不一致 |
| 打包出的 App 没有核心/shim | 直接跑了 `tauri build`，改用 `scripts/build-bundle.mjs` |
| `lipo` 报缺少架构 | `rustup target add aarch64-apple-darwin x86_64-apple-darwin` |
| macOS 注入失效、dyld 报错 | 开发模式忘记 `codesign -s -` shim；或目标 App 由 SIP / hardened runtime 保护 |
| `tauri build` 报 icon 缺失 | `cd src-tauri && ../ui/node_modules/.bin/tauri icon ../app-icon.png` |
| Windows 构建缺 WebView2 | 安装 Microsoft Edge WebView2 Runtime，或确认 runner 自带 |
| 目标机器上「已损坏，无法打开」 | ad hoc 签名的预期行为，右键打开或清除 quarantine；正式分发需公证 |
| 本地 macOS 通过、CI 的 Linux job 报 `dead_code` 或 `unused dependency` | 平台专用常量/依赖没收敛到 cfg 作用域；跑 `scripts/check-linux-cfg.sh` 复现，常量放进使用它的 `cfg` 块内，依赖移到 `[target.'cfg(...)'.dependencies]` |

更多运行时问题见 [TROUBLESHOOTING.md](TROUBLESHOOTING.md)。
