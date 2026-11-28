# 排障

## 应用启动后网络不通

1. **核心是否在运行**：popover 显示「未运行」→ 点「启动核心」；
2. **端口是否被占用**：桌面核心会自动避让，面板会显示实际 SOCKS5/HTTP 端口。若核心启动失败，
   检查设置的监听地址是否为回环地址（`127.0.0.1` / `localhost`），以及上游是否可达
   （`lsof -i :7890` 之类）；
3. **shim 是否存在**：开发模式需先构建并 adhoc 签名：
   `cargo build -p localproxy-shim-macos && codesign -s - target/debug/liblocalproxy_shim_macos.dylib`；
4. **上游是否可达**：设置里核对上游地址/端口；上游不可达时核心日志有
   `代理拒绝 (REP=...)` 提示，SOCKS5/HTTP 类型选错会有协议错配提示；
5. 用 popover 的「最近连接」确认目标是否进入核心、命中哪条规则、结果是 proxy/direct/blocked。

## 「代理启动」报注入失败

- macOS：提示 hardened runtime → 该应用无法注入，见 [USAGE](USAGE.md#已知限制不开-tun-的固有权衡)；
- Windows：提示受安全软件保护 → 检查 AV/EDR 拦截，或换非保护进程验证。

## 应用能上网但出口 IP 没变

- 该应用可能绕过了 BSD socket（自建网络栈或使用内核旁路）；
- 检查是否命中直连规则（内网 IP / `.local` 域名默认直连）；
- 确认 DNS 走的是 FakeIP：`LP_LOG=debug` 下日志中应有 `shim: FakeIP <域名> -> 198.18.x.x`。

## 启动时提示「LocalProxy 已在运行」

这是单实例保护：同一配置目录下只允许一个菜单栏实例，避免两个图标争抢端口与策略目录。

- macOS 会弹系统通知；菜单栏里已有图标，点开即可；
- 刚退出又立刻启动时锁会在 1.5 秒内重试，通常不用管；
- 需要并行多实例（多账号、测试），给第二个实例独立配置目录：

  ```bash
  LOCALPROXY_CONFIG_DIR=/tmp/localproxy-alt ./LocalProxy.app/Contents/MacOS/localproxy-app
  ```

- 若确认没有实例在跑却仍被占用，检查是否有残留进程：`pgrep -fl localproxy-app`；
  锁文件在配置目录的 `app.lock`，进程退出后内核自动释放，不需要手工删除。

## 端口与监听

- **桌面模式**：核心优先使用设置里的端口；被其他程序占用时自动换用空闲回环端口，并在面板显示实际值。
  设置里的端口是偏好值，下次启动会重新尝试。
- **CLI 模式**：`localproxy-core --config ...` 严格使用配置里的端口，冲突时直接报错，需要手改配置。
- 监听地址只接受回环地址或 `localhost`，绑定到 `0.0.0.0` 会被拒绝（这是有意的安全限制）。

## 设置保存失败

保存时 App 会先落盘再让核心热重载；核心校验不通过时不会生效：

- 提示「核心未应用配置，已恢复原配置」→ 说明核心拒绝了新配置（例如独立节点参数错误、
  上游指向了核心自己的监听端口形成回环、Clash 控制地址不是本机回环）。按提示修正后重试；
- 提示「核心未确认生效的上游」→ 核心版本与 App 不匹配，更新核心程序；
- 提示「开机自启设置失败」→ 系统级自启项写入失败，App 会把该开关回滚，检查系统权限后重试；
- 未保存的改动在标题栏显示「未保存的更改」；「重新加载」会丢弃草稿并重新读取磁盘配置。

## 打包 / 安装问题

构建、签名、图标、架构缺失等问题集中在 [BUILD.md](BUILD.md#6-常见构建问题)；
发版相关的 CI 失败与产物校验见 [RELEASE.md](RELEASE.md#8-故障处理)。

安装后被 Gatekeeper 拦（「已损坏，无法打开」）是未签名/未公证构建的预期行为：

```bash
xattr -dr com.apple.quarantine /Applications/LocalProxy.app
```

## 日志位置

- 核心日志：stderr。开发模式 `RUST_LOG=debug target/debug/localproxy-core`；
  桌面模式由 App 启动，可临时用 CLI 以同一配置复现；
- shim 日志：环境变量 `LP_LOG=debug`（由 launcher 注入，默认关闭）；
- App 日志：`RUST_LOG=debug` 后运行 `tauri dev`；
- 配置与锁：见 [BUILD.md 第 5 节](BUILD.md#5-配置与数据位置)。

## e2e 自检脚本

一键验证注入链路（mock 上游 + 注入测试客户端）：

```bash
cargo build -p localproxy-core -p localproxy-shim-macos
codesign -s - target/debug/liblocalproxy_shim_macos.dylib
scripts/e2e/inject-e2e.sh
```

通过标准：FakeIP 生效、域名透传（mock 收到 CONNECT example.com:80）、HTTP 响应经代理到达、
未注入流量不经核心。
