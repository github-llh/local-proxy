#!/usr/bin/env bash
# 用 Linux target 交叉 clippy，校验非 Tauri crate 的平台 cfg 分支。
#
# 为什么需要：macOS 上 `cargo clippy` 只编译 macOS 分支，仅在 Windows/Linux
# 生效的代码（以及被 cfg 掉的常量、平台专用依赖）不会暴露问题。CI 的
# ubuntu-latest 会完整编译一遍，本脚本让同类问题在本地就能提前发现。
#
# 覆盖范围：net / core / launcher / shim* / inject。
# 不覆盖 src-tauri：它需要 webkit2gtk 的 pkg-config，只能在 Linux runner 上编译。
set -euo pipefail

export PATH="${HOME}/.cargo/bin:${PATH}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

target=x86_64-unknown-linux-gnu
rustup target add "$target" >/dev/null 2>&1 || true

cargo clippy --target "$target" --all-targets \
  -p localproxy-net \
  -p localproxy-core \
  -p localproxy-launcher \
  -p localproxy-shim \
  -p localproxy-shim-macos \
  -p localproxy-shim-win \
  -p localproxy-inject \
  -- -D warnings
