# Agent.md — LocalProxy 自动发布指南

> 面向 Claude / GPT / DeepSeek / GLM 等 AI 代理。
> 目标：在当前仓库完成版本发布，让 GitHub Actions 自动构建双平台产物并上传 Release。

## 0. 一句话流程

```text
push tag vX.Y.Z
  -> GitHub Actions: Release workflow
  -> matrix: macOS universal (arm64 + x86_64) / Windows x64
  -> cargo test --workspace（三平台）
  -> scripts/build-bundle.mjs（核心、shim 与 app/dmg 或 msi/nsis）
  -> 按平台生成 SHA256SUMS + GitHub Release 资产
```

## 1. 固定坐标

| 项目    | 值                                           |
|:------|:--------------------------------------------|
| 仓库    | `https://github.com/github-llh/local-proxy` |
| 主工作流  | `.github/workflows/release.yml`             |
| 当前版本  | `0.1.0`                                     |
| 下一个版本 | `0.1.1`（或按语义化提升）                            |

## 2. 标准发布流程

### 2.1 前置检查

```bash
git pull --ff-only origin main
git status --short          # 应为干净工作区
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --workspace      # 本地必须全绿
pnpm --dir ui build         # 前端必须可构建
```

- 确认版本号从未在 GitHub Releases 使用过；
- 不要在仓库、脚本或提交中写入任何 token / 证书。

### 2.2 触发发布

```bash
git tag -a v0.1.1 -m "Release v0.1.1"
git push origin v0.1.1
```

推送 tag 后不要再手动创建 GitHub Release；`release.yml` 会完成全部构建与发布。

### 2.3 观察 Actions

```bash
gh run list --workflow release.yml --limit 3
```

必须确认 jobs：`test`（ubuntu/macos/windows）、`build-macos`、`build-windows`、`release` 全部 success。

在对应平台本地构建安装包，先安装锁定的前端依赖，再运行打包脚本。

```bash
pnpm --dir ui install --frozen-lockfile
# macOS（需要安装 arm64 和 x86_64 Rust target）
node scripts/build-bundle.mjs universal-apple-darwin
# Windows（需要安装 MSVC x64 Rust target）
node scripts/build-bundle.mjs x86_64-pc-windows-msvc
```

## 3. macOS 签名与公证（可选）

未配置签名 secrets 时，构建脚本对 macOS 应用包、核心和 shim 使用 ad hoc 签名。

| Secret                                          | 用途                                       |
|:------------------------------------------------|:-----------------------------------------|
| `APPLE_CERTIFICATE`                             | Developer ID Application 证书（.p12 base64） |
| `APPLE_CERTIFICATE_PASSWORD`                    | 证书密码                                     |
| `APPLE_SIGNING_IDENTITY`                        | 签名身份                                     |
| `APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` | notarytool 公证                            |

Tauri 从构建环境读取签名和公证变量。当前发布工作流传入证书和签名身份，需要公证时还需将对应凭据传入构建环境。

## 4. 绝对不要

- 不要提交证书、密码、token；
- 不要复用已删除的 Release / tag；
- 不要在 `release.yml` 之外手动改产物后覆盖 Release 资产；
- 不要跳过 `cargo test --workspace` 直接打 tag。

## 5. 常见故障

| 现象                    | 处理                                                                        |
|:----------------------|:--------------------------------------------------------------------------|
| `test` job 失败         | 本地复跑 `cargo test --workspace` 修复后再打 tag                                   |
| macOS 构建失败 `codesign` | 检查证书 secrets。无证书时确认构建使用 ad hoc 签名                                         |
| Windows 构建失败 WebView2 | 确认 `windows-latest` runner 自带 WebView2                                    |
| tauri build 报 icon 缺失 | `cd src-tauri && ../ui/node_modules/.bin/tauri icon ../app-icon.png` 重新生成 |

## 6. 关键文件

| 文件                              | 作用                         |
|:--------------------------------|:---------------------------|
| `.github/workflows/release.yml` | tag 发布、三平台测试、双平台构建、Release |
| `.github/workflows/ci.yml`      | PR/push 的测试与 lint          |
| `scripts/build-bundle.mjs`      | 构建并打包核心与当前平台的 shim         |
| `src-tauri/tauri.conf.json`     | 应用配置（bundle targets/icon）  |
| `ui/package.json`               | 前端依赖                       |
