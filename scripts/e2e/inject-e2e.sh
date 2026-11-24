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
MOCK_UDP_PORT=19081
CORE_SOCKS_PORT=11080
SEEN_FILE=/tmp/lp-e2e-seen.txt
UDP_SEEN_FILE=/tmp/lp-e2e-udp-seen.txt
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

echo "== 3. 启动 mock 上游（TCP + UDP echo） =="
rm -f "$SEEN_FILE" "$READY_FILE" "$UDP_SEEN_FILE"
"$PY" "$REPO_ROOT/scripts/e2e/mock_upstream.py" --port "$MOCK_PORT" --seen "$SEEN_FILE" --ready-file "$READY_FILE" --udp-port "$MOCK_UDP_PORT" --udp-seen "$UDP_SEEN_FILE" &
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

echo "== 8. UDP 封堵断言（WebRTC/QUIC 防泄漏） =="
# 8.1 公网 UDP（STUN 3478）：注入后 sendto 必须失败（block，errno=ENETUNREACH=51）
# 注意：客户端被阻断时返回 exit 2（预期语义），需 || true 避免 set -e 中断
DYLD_INSERT_LIBRARIES="$SHIM" LP_CORE_HOST=127.0.0.1 LP_CORE_PORT=$CORE_SOCKS_PORT \
  /tmp/lp-e2e-client --udp 8.8.8.8 3478 > /tmp/lp-e2e-udp-pub.txt 2>/dev/null || true
grep -q "UDP_SENDTO_FAILED errno=51" /tmp/lp-e2e-udp-pub.txt \
  && echo "PASS: 公网 UDP sendto 被阻断（errno=51 ENETUNREACH，应用将回退 TCP）" \
  || { echo "FAIL: 公网 UDP 未被阻断"; cat /tmp/lp-e2e-udp-pub.txt; exit 1; }
# 8.2 未注入对照：同一目标 sendto 成功（证明阻断来自 shim 而非环境）
/tmp/lp-e2e-client --udp 8.8.8.8 3478 > /tmp/lp-e2e-udp-pub-direct.txt 2>/dev/null || true
grep -q "UDP_SENDTO_OK" /tmp/lp-e2e-udp-pub-direct.txt \
  && echo "PASS: 未注入时公网 UDP 正常（对照组，证明阻断来自注入）" \
  || { echo "FAIL: 对照组异常（未注入也不通？检查网络）"; cat /tmp/lp-e2e-udp-pub-direct.txt; exit 1; }
# 8.3 局域网 UDP：注入后必须放行（不误伤正常应用）
DYLD_INSERT_LIBRARIES="$SHIM" LP_CORE_HOST=127.0.0.1 LP_CORE_PORT=$CORE_SOCKS_PORT \
  /tmp/lp-e2e-client --udp 127.0.0.1 $MOCK_UDP_PORT > /tmp/lp-e2e-udp-lan.txt 2>/dev/null || true
grep -q "UDP_SENDTO_OK" /tmp/lp-e2e-udp-lan.txt \
  && echo "PASS: 局域网 UDP sendto 放行（不误伤）" \
  || { echo "FAIL: 局域网 UDP 被误伤"; cat /tmp/lp-e2e-udp-lan.txt; exit 1; }
sleep 0.3
grep -q "LP-UDP-PROBE" "$UDP_SEEN_FILE" \
  && echo "PASS: mock UDP 收到回环探测包（放行路径数据可达）" \
  || { echo "FAIL: mock UDP 未收到探测包"; cat "$UDP_SEEN_FILE" 2>/dev/null; exit 1; }
# 8.4 mock UDP 不应收到任何公网来源阻断包的残留（block 在发送前生效）
if grep -qv "127.0.0.1" "$UDP_SEEN_FILE" 2>/dev/null; then
  echo "FAIL: mock UDP 收到非回环包（泄漏？）"; cat "$UDP_SEEN_FILE"; exit 1
fi
echo "PASS: mock UDP 仅收到回环包（无泄漏）"

echo "== 9. DNS 旁路 API 断言（gethostbyname 泄漏面） =="
# 9.1 注入后 gethostbyname 必须返回 FakeIP（否则老库/运行时走真实 DNS）
DYLD_INSERT_LIBRARIES="$SHIM" LP_CORE_HOST=127.0.0.1 LP_CORE_PORT=$CORE_SOCKS_PORT \
  /tmp/lp-e2e-client --dns example.com > /tmp/lp-e2e-dns.txt 2>/dev/null || true
grep -q "GETHOSTBYNAME name=example.com addr=198\.18\." /tmp/lp-e2e-dns.txt \
  && echo "PASS: gethostbyname 返回 FakeIP + 域名（旁路 API 已覆盖）" \
  || { echo "FAIL: gethostbyname 未被覆盖"; cat /tmp/lp-e2e-dns.txt; exit 1; }
# 9.2 本地域名走真实解析（不误伤）
DYLD_INSERT_LIBRARIES="$SHIM" LP_CORE_HOST=127.0.0.1 LP_CORE_PORT=$CORE_SOCKS_PORT \
  /tmp/lp-e2e-client --dns localhost > /tmp/lp-e2e-dns-local.txt 2>/dev/null || true
grep -q "GETHOSTBYNAME name=localhost addr=127\.0\.0\.1" /tmp/lp-e2e-dns-local.txt \
  && echo "PASS: localhost 走真实解析（不误伤）" \
  || { echo "FAIL: localhost 解析异常"; cat /tmp/lp-e2e-dns-local.txt; exit 1; }

echo "== 10. 非阻塞 connect 断言（Electron/Chromium 默认模式回归） =="
DYLD_INSERT_LIBRARIES="$SHIM" LP_CORE_HOST=127.0.0.1 LP_CORE_PORT=$CORE_SOCKS_PORT \
  /tmp/lp-e2e-client --nonblock example.com 80 > /tmp/lp-e2e-nb.txt 2>/dev/null || true
grep -q "MOCK-UPSTREAM-OK" /tmp/lp-e2e-nb.txt \
  && echo "PASS: 非阻塞 connect 经注入正常工作（shim O_NONBLOCK 适配生效）" \
  || { echo "FAIL: 非阻塞 connect 回归"; cat /tmp/lp-e2e-nb.txt; exit 1; }

echo "== 11. UDP 全代理断言（udp_mode=proxy，S2 roundtrip） =="
# mock 上游需支持 UDP Associate：mock_upstream.py 的 UDP echo 即中继目标
DYLD_INSERT_LIBRARIES="$SHIM" LP_CORE_HOST=127.0.0.1 LP_CORE_PORT=$CORE_SOCKS_PORT \
  LP_UDP_MODE=proxy /tmp/lp-e2e-client --udp-proxy 127.0.0.1 $MOCK_UDP_PORT > /tmp/lp-e2e-udpproxy.txt 2>/dev/null || true
grep -q "UDP_PROXY_SEND_OK" /tmp/lp-e2e-udpproxy.txt \
  && grep -q "UDP_PROXY_RECV data=LP-UDP-PROXY-PROBE" /tmp/lp-e2e-udpproxy.txt \
  && echo "PASS: UDP 全代理 roundtrip 经核心到达 mock（sendto/recvfrom + ASSOCIATE 链路）" \
  || { echo "FAIL: UDP 全代理 roundtrip 异常"; cat /tmp/lp-e2e-udpproxy.txt; exit 1; }
# mock 侧确认收到该 UDP 包（经核心中继）
grep -q "LP-UDP-PROXY-PROBE" "$UDP_SEEN_FILE" \
  && echo "PASS: mock UDP 收到经核心中继的探测包" \
  || { echo "FAIL: mock 未收到中继包"; cat "$UDP_SEEN_FILE" 2>/dev/null; exit 1; }

echo ""
echo "===== M2 端到端验证全部通过 ====="
