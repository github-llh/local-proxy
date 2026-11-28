# 发版指南

面向维护者：从改版本号到 GitHub Release 资产可下载。打包命令的细节见 [BUILD.md](BUILD.md)；
AI 代理执行发版时的硬约束与速查命令见 [Agent.md](../Agent.md)。

## 1. 版本号

发版前同步两处（不一致会导致安装包文件名与 tag 对不上）：

| 位置 | 字段 | 作用 |
|:--|:--|:--|
| `Cargo.toml` | `[workspace.package] version` | Rust crate 版本、`localproxy-core --version` |
| `src-tauri/tauri.conf.json` | `version` | Tauri 应用版本，决定 `.dmg` / `.msi` / `.exe` 文件名 |

```bash
grep -n '^version' Cargo.toml
grep -n '"version"' src-tauri/tauri.conf.json
```

发布 tag 用 `v` 前缀（`v0.1.1`），`release.yml` 的触发条件是 `tags: ["v*"]`。同一个版本号一旦
发布过就不要再复用，即使 Release 被删除——客户端缓存与下载统计会因此混乱。

## 2. 发布前检查清单

```bash
git pull --ff-only origin main
git status --short                 # 应为干净工作区；未提交改动会进入产物
export PATH="$HOME/.cargo/bin:$PATH"
pnpm --dir ui install --frozen-lockfile
pnpm --dir ui build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace             # 打 tag 前必须全绿
```

可选的本地预演：按 CI 的相同命令打一次包，确认产物可启动（见 [BUILD.md 第 4 节](BUILD.md#4-发布构建安装包)）。
CI 用的也是同一个 `scripts/build-bundle.mjs`，本地预演能提前暴露环境差异。

清单：

- [ ] 两处版本号一致且未发布过
- [ ] `cargo test --workspace` 全绿
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 零警告
- [ ] `cargo fmt --all -- --check` 通过
- [ ] `pnpm --dir ui build` 通过
- [ ] 工作区没有意外的未提交改动
- [ ] 没有把证书、token、密码写进仓库或脚本
- [ ] 需要正式分发时，签名/公证 secrets 已配置（见第 4 节）

## 3. 触发发布

### 3.1 打 tag（正式方式）

```bash
git tag -a v0.1.1 -m "Release v0.1.1"
git push origin v0.1.1
```

推送 tag 后**不要**再手动创建 GitHub Release，`release.yml` 会完成构建与发布。

### 3.2 手动触发（应急/重跑）

`release.yml` 支持 `workflow_dispatch`，需要输入不带 `v` 的版本号：

```bash
gh workflow run release.yml -f version=0.1.1
```

无论哪种方式，`release.yml` 都会把 tag 存在的版本号作为 Release tag（手动触发时是 `v<version>`）。

### 3.3 观察执行

```bash
gh run list --workflow release.yml --limit 3
gh run watch <run-id>
```

必须确认 `test`、`build-macos`、`build-windows`、`release` 全部 success。

## 4. CI 在做什么

| Job | Runner | 关键步骤 |
|:--|:--|:--|
| `test` | ubuntu / macos / windows | 安装前端依赖 → `pnpm --dir ui build` → `cargo test --workspace` |
| `build-macos` | macos-latest | `node scripts/build-bundle.mjs universal-apple-darwin`；校验 `Contents/Resources` 含核心与 shim、`lipo` 双架构、`codesign --verify --deep --strict`；生成 `.app.zip` 与 `.dmg`、`SHA256SUMS-macos.txt` |
| `build-windows` | windows-latest | `node scripts/build-bundle.mjs x86_64-pc-windows-msvc`；收集 `.msi` 与 `nsis .exe`、`SHA256SUMS-windows.txt` |
| `release` | ubuntu-latest | 汇总两个构建 job 的产物，创建带自动 release notes 的 Release（非 draft） |

失败时会停在对应 job，不会发布半成品 Release（`release` 依赖两个构建 job）。

## 5. Release 资产

| 文件 | 说明 |
|:--|:--|
| `LocalProxy-universal.app.zip` | 免安装的 `.app`（保留扩展属性签名的 ditto 打包） |
| `LocalProxy_<version>_universal.dmg` | 安装盘 |
| `LocalProxy_<version>_x64_en-US.msi` | Windows MSI 安装包 |
| `LocalProxy_<version>_x64-setup.exe` | Windows NSIS 安装包 |
| `SHA256SUMS-macos.txt` / `SHA256SUMS-windows.txt` | 对应平台产物校验和 |

校验方式：

```bash
shasum -a 256 -c SHA256SUMS-macos.txt      # macOS
sha256sum -c SHA256SUMS-windows.txt        # Windows（Git Bash）
```

## 6. 签名与公证

未配置任何签名 secret 时，CI 走 ad hoc 签名（脚本对核心、shim 与应用使用 `--sign -`）。
产物可以运行，但离开本机会被 Gatekeeper 拦截：接收方需右键 →「打开」，或
`xattr -dr com.apple.quarantine /Applications/LocalProxy.app`。

正式分发请在仓库 Secrets 配置：

| Secret | 用途 |
|:--|:--|
| `APPLE_CERTIFICATE` | Developer ID Application 证书（`.p12` 的 base64） |
| `APPLE_CERTIFICATE_PASSWORD` | 证书密码 |
| `APPLE_SIGNING_IDENTITY` | 签名身份（如 `Developer ID Application: Your Name (TEAMID)`） |
| `APPLE_ID` | 公证账号 |
| `APPLE_PASSWORD` | App 专用密码 |
| `APPLE_TEAM_ID` | 团队 ID |

当前工作流把证书与签名身份传入构建环境；需要 notarytool 公证时，还要把 `APPLE_ID` /
`APPLE_PASSWORD` / `APPLE_TEAM_ID` 一并传入 `build-macos` 的 `env`。否则只有签名、没有公证。

## 7. 发布后验证

1. 打开 Release 页面，确认四类产物齐全、release notes 正常；
2. 下载 `SHA256SUMS-*.txt` 与对应产物，按第 5 节校验；
3. macOS：挂载 dmg → 拖入 `/Applications` → 启动，确认菜单栏图标出现、popover 显示核心运行中与
   实际 SOCKS5/HTTP 端口；
4. 添加一个应用并「代理启动」，确认流量经代理（可用「风控自检 → 探测出口」核对出口 IP）；
5. Windows：安装 msi 或 nsis，确认托盘、设置窗口、注入启动可用；
6. 若这批改动涉及配置结构，用旧版 `config.json` 启动一次，确认能正常迁移（旧字段被忽略或提升，
   不报错）。

## 8. 故障处理

| 现象 | 处理 |
|:--|:--|
| `test` job 失败 | 本地复跑失败平台的相同命令（`cargo test --workspace`）修复后，用**新版本号**重发 |
| macOS 构建 `codesign` 失败 | 检查证书 secrets 是否是有效的 `.p12` base64；无证书时确认走 ad hoc（不要设置空的 `APPLE_SIGNING_IDENTITY`） |
| `lipo` 报缺少架构 | CI 已安装双 target；本地预演时执行 `rustup target add aarch64-apple-darwin x86_64-apple-darwin` |
| 打包产物缺少核心/shim | 说明绕过了 `scripts/build-bundle.mjs`，改用脚本 |
| `build-windows` 缺 WebView2 | runner 自带；本地构建时安装 WebView2 Runtime |
| `tauri build` 报 icon 缺失 | `cd src-tauri && ../ui/node_modules/.bin/tauri icon ../app-icon.png` |
| Release 缺少某个资产 | `fail_on_unmatched_files: true` 会让 job 失败；先修构建，不要手工上传补丁文件 |
| 产物文件名版本与 tag 不一致 | 两处版本号未同步，见第 1 节 |

## 9. 撤销与重发

- **不要**删除 Release 后复用同一个 tag / 版本号重新发布；
- 发布内容有问题时，修复后提升版本号（`0.1.1` → `0.1.2`）重新打 tag；
- 严重问题（例如误发了含密钥的构建）才撤销：先删除 Release，再删除远端 tag，然后在
  `docs/TROUBLESHOOTING.md` 或 Release notes 中说明原因，并用新版本号发布修复；
- 任何情况下都不要手工改写已发布 Release 的资产内容——校验和会与 `SHA256SUMS` 不一致。
