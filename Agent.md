# Agent.md — LocalProxy 发布速查（面向 AI 代理）

> 人类维护者的完整发版说明见 [docs/RELEASE.md](docs/RELEASE.md)，构建细节见
> [docs/BUILD.md](docs/BUILD.md)。本文件只保留执行发版所需的最短路径与硬约束，避免与上述文档重复。

## 0. 一句话流程

```text
改版本号（两处）→ 本地全量检查 → push tag vX.Y.Z
  → GitHub Actions: release.yml
  → matrix: macOS universal (arm64 + x86_64) / Windows x64
  → cargo test --workspace（三平台）
  → scripts/build-bundle.mjs（核心、shim 与 app/dmg 或 msi/nsis）
  → 按平台生成 SHA256SUMS + GitHub Release 资产
```

## 1. 固定坐标

| 项目 | 值 |
|:--|:--|
| 仓库 | `https://github.com/github-llh/local-proxy` |
| 主工作流 | [.github/workflows/release.yml](.github/workflows/release.yml) |
| 构建脚本 | [scripts/build-bundle.mjs](scripts/build-bundle.mjs) |
| 版本号位置 | `Cargo.toml` 的 `[workspace.package] version` + `src-tauri/tauri.conf.json` 的 `version` |

## 2. 执行步骤

```bash
# 2.1 前置检查（必须全绿）
git pull --ff-only origin main
git status --short                       # 应为干净工作区
export PATH="$HOME/.cargo/bin:$PATH"
pnpm --dir ui install --frozen-lockfile
pnpm --dir ui build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# 2.2 同步两处版本号，确认该版本从未发布过

# 2.3 触发发布
git tag -a v0.1.1 -m "Release v0.1.1"
git push origin v0.1.1

# 2.4 观察
gh run list --workflow release.yml --limit 3
```

必须确认 `test`、`build-macos`、`build-windows`、`release` 全部 success。
推送 tag 后**不要**手动创建 GitHub Release。

需要本地复现 CI 产物时（例如排障），用的是同一个脚本：

```bash
pnpm --dir ui install --frozen-lockfile
rustup target add aarch64-apple-darwin x86_64-apple-darwin   # macOS universal
node scripts/build-bundle.mjs universal-apple-darwin         # macOS
node scripts/build-bundle.mjs x86_64-pc-windows-msvc         # Windows（需在 Windows 上执行）
```

产物与自检命令见 [docs/BUILD.md](docs/BUILD.md#4-发布构建安装包)。

## 3. 硬约束

- 不要跳过 `cargo test --workspace` 直接打 tag；
- 不要直接运行 `tauri build` 代替构建脚本（会缺少核心与 shim）；
- 不要提交证书、密码、token，也不要写进脚本或 workflow；
- 不要复用已删除的 Release / tag / 版本号，出问题用递增版本号重发；
- 不要在 `release.yml` 之外手动改写已发布 Release 的资产；
- 签名/公证 secret 未配置时 CI 使用 ad hoc 签名，产物不可直接分发给他人。

## 4. 细节去哪看

| 需要 | 文档 |
|:--|:--|
| 版本号、检查清单、CI job、产物清单、发布后验证、回滚 | [docs/RELEASE.md](docs/RELEASE.md) |
| 依赖、开发构建、打包命令、产物路径、自检、构建故障 | [docs/BUILD.md](docs/BUILD.md) |
| 签名与公证的 secret 列表 | [docs/RELEASE.md](docs/RELEASE.md#6-签名与公证) |
| 运行时问题（注入失败、端口、单实例、日志） | [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) |

## 5. 常见失败（速查）

| 现象 | 处理 |
|:--|:--|
| `test` job 失败 | 本地复跑同命令修复后，用新版本号重发 |
| macOS `codesign` 失败 | 检查证书 secret 是否为有效 `.p12` base64；无证书时不要设置空的 `APPLE_SIGNING_IDENTITY` |
| 打包产物缺核心/shim | 绕过了 `scripts/build-bundle.mjs` |
| `tauri build` 报 icon 缺失 | `cd src-tauri && ../ui/node_modules/.bin/tauri icon ../app-icon.png` |
| 产物文件名版本与 tag 不一致 | 两处版本号未同步 |
