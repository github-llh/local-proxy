import { useEffect, useState } from "react";

interface Health {
  running: boolean;
  socks5_port: number;
  active_upstream: string;
}

declare global {
  interface Window {
    __LOCALPROXY__?: Health;
  }
}

export default function App() {
  const [health, setHealth] = useState<Health>({
    running: false,
    socks5_port: 1080,
    active_upstream: "",
  });

  useEffect(() => {
    // M3：经 Tauri invoke 拉取核心状态；M0 用注入的占位数据展示骨架
    if (window.__LOCALPROXY__) {
      setHealth(window.__LOCALPROXY__);
    }
  }, []);

  return (
    <main className="app">
      <header className="app-header">
        <span className={`status-dot ${health.running ? "on" : "off"}`} />
        <h1>LocalProxy</h1>
      </header>
      <section className="card">
        <p className="muted">M0 骨架：菜单栏控制中心构建中。</p>
        <dl className="kv">
          <dt>核心状态</dt>
          <dd>{health.running ? "运行中" : "未运行"}</dd>
          <dt>SOCKS5 端口</dt>
          <dd>{health.socks5_port}</dd>
          <dt>当前上游</dt>
          <dd>{health.active_upstream || "未配置"}</dd>
        </dl>
      </section>
    </main>
  );
}
