# 排障

## 应用启动后网络不通

1. **核心是否在运行**：popover 显示「未运行」→ 点「启动核心」；
   若启动失败，检查 1080/1081 端口是否被占用（`lsof -i :1080`）；
2. **shim 是否存在**：开发模式需先
   `cargo build -p localproxy-shim-macos` 且 adhoc 签名；
3. **上游是否可达**：设置里核对上游地址/端口；上游不可达时核心日志有
   `代理拒绝 (REP=...)` 提示，SOCKS5/HTTP 类型选错会有协议错配提示。

## 「代理启动」报注入失败

- macOS：提示 hardened runtime → 该应用无法注入，见 [USAGE](USAGE.md) 已知限制；
- Windows：提示受安全软件保护 → 检查 AV/EDR 拦截，或换非保护进程验证。

## 应用能上网但出口 IP 没变

- 该应用可能绕过了 BSD socket（自建网络栈或使用内核旁路）；
- 检查是否命中直连规则（内网 IP / `.local` 域名默认直连）；
- 确认 DNS 走的是 FakeIP：日志中 `shim: FakeIP <域名> -> 198.18.x.x`。

## 端口冲突

核心默认 1080/1081/1082。被占用时在设置中修改，保存后重启核心生效。

## 日志位置

- 核心日志：`RUST_LOG=info target/debug/localproxy-core`（stderr）；
- shim 日志：环境变量 `LP_LOG=debug`（由 launcher 注入，默认 off）；
- App 日志：`RUST_LOG=debug` 后运行 `tauri dev`。

## e2e 自检脚本

一键验证注入链路（mock 上游 + 注入测试客户端）：

```bash
scripts/e2e/inject-e2e.sh
```

通过标准：FakeIP 生效、域名透传（mock 收到 CONNECT example.com:80）、
HTTP 响应经代理到达、未注入流量不经核心。
