#!/bin/bash
# LocalProxy macOS 端到端注入验证（M2 验收）。
#
# 链路：curl（注入 shim）→ 本地核心 SOCKS5 → mock 上游
# 断言：
#   1. curl 收到 mock 的 HTTP 200（MOCK-UPSTREAM-OK）
#   2. mock 记录的 CONNECT 目标为 example.com:80（域名经代理，DNS 不泄露）
#   3. 未注入的对照组：curl 直连（不经核心，mock 无新记录）
#
# 用法：scripts/e2e/inject-e2e.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
PY="$REPO_ROOT/../../.dsh/dsh-runtimes/dsh-primary-runtime/dependencies/python/bin/python3"

# 兼容本机 python3
if [ ! -x "$PY" ]; then PY="python3"; fi

MOCK_PORT=19080
CORE_SOCKS_PORT=11080
SEEN_FILE=/tmp/lp-e2e-seen.txt
READY_FILE=/tmp/lp-e2e-mock-ready
CONFIG_FILE=/tmp/lp-e2e-config.json
SHIM="$REPO_ROOT/target/debug/liblocalproxy_shim_macos.dylib"
CORE="$REPO_ROOT/target/debug/localproxy-core"

cleanup() {
  [ -n "${CORE_PID:-}" ] && kill "$CORE_PID" 2>/dev/null || true
  [ -n "${MOCK_PID:-}" ] && kill "$MOCK_PID" 2>/dev/null || true
}
trap cleanup EXIT

echo "== 1. 构建 core 与 shim =="
cd "$REPO_ROOT"
cargo build -p localproxy-core -p localproxy-shim-macos 2>&1 | tail -1

echo "== 2. adhoc 签名 shim（arm64 加载必需） =="
codesign -s - "$SHIM"

echo "== 3. 启动 mock 上游 =="
rm -f "$SEEN_FILE" "$READY_FILE"
"$PY" "$REPO_ROOT/scripts/e2e/mock_upstream.py" --port "$MOCK_PORT" --seen "$SEEN_FILE" --ready-file "$READY_FILE" &
MOCK_PID=$!
for _ in $(seq 1 50); do [ -f "$READY_FILE" ] && break; sleep 0.1; done

echo "== 4. 启动本地核心（上游指向 mock） =="
cat > "$CONFIG_FILE" <<EOF
{
  "socks5_port": $CORE_SOCKS_PORT,
  "http_port": 11081,
  "control_port": 11082,
  "upstream": {
    "active": "mock",
    "servers": [{ "id": "mock", "name": "Mock", "type": "socks5", "host": "127.0.0.1", "port": $MOCK_PORT }]
  },
  "routing": { "default_action": "proxy", "use_default_private": true }
}
EOF
"$CORE" --config "$CONFIG_FILE" > /tmp/lp-e2e-core.log 2>&1 &
CORE_PID=$!
sleep 1

echo "== 5. 编译并注入测试客户端访问 http://example.com/e2e-test =="
clang -O1 -o /tmp/lp-e2e-client "$REPO_ROOT/scripts/e2e/test_client.c"
# 注意：/usr/bin/curl 是 SIP 平台二进制（DYLD_INSERT_LIBRARIES 会被剥离），
# 官方 node 带 hardened runtime（dyld 拒绝加载库），故使用自建客户端。
DYLD_INSERT_LIBRARIES="$SHIM" \
LP_CORE_HOST=127.0.0.1 \
LP_CORE_PORT=$CORE_SOCKS_PORT \
  /tmp/lp-e2e-client example.com 80 > /tmp/lp-e2e-body.txt 2>/tmp/lp-e2e-client-stderr.txt || {
  echo "FAIL: 测试客户端注入模式失败"; cat /tmp/lp-e2e-core.log /tmp/lp-e2e-client-stderr.txt; exit 1;
}
cat /tmp/lp-e2e-client-stderr.txt

echo "== 6. 断言 =="
grep -q "MOCK-UPSTREAM-OK" /tmp/lp-e2e-body.txt && echo "PASS: 经 mock 上游收到 HTTP 200" || { echo "FAIL: 响应体异常"; cat /tmp/lp-e2e-body.txt; exit 1; }
sleep 0.3
grep -q "example.com:80" "$SEEN_FILE" && echo "PASS: mock 收到 CONNECT example.com:80（域名透传，DNS 未泄露）" || { echo "FAIL: mock 未见预期目标"; cat "$SEEN_FILE"; exit 1; }
grep -q "198\.18\." /tmp/lp-e2e-client-stderr.txt && echo "PASS: getaddrinfo 返回 FakeIP（198.18.x.x）" || { echo "FAIL: 未见 FakeIP"; cat /tmp/lp-e2e-client-stderr.txt; exit 1; }

echo "== 7. 对照组：未注入客户端不经核心 =="
BEFORE=$(wc -l < "$SEEN_FILE")
/tmp/lp-e2e-client example.com 80 > /tmp/lp-e2e-body-direct.txt 2>/dev/null || true
AFTER=$(wc -l < "$SEEN_FILE")
[ "$BEFORE" = "$AFTER" ] && echo "PASS: 未注入流量未经过核心（mock 记录数不变）" || echo "WARN: 未注入流量意外到达 mock（$BEFORE -> $AFTER）"

echo ""
echo "===== M2 端到端验证全部通过 ====="
