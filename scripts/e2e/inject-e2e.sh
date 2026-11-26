#!/bin/bash
# LocalProxy macOS 注入链路端到端验证。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
cd "$ROOT"

if [[ "$(uname -s)" != Darwin ]]; then
  echo "此测试需要 macOS" >&2
  exit 1
fi
cargo build --locked -p localproxy-core -p localproxy-shim-macos
exec python3 scripts/e2e/inject_e2e.py
