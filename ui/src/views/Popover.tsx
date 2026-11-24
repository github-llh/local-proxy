// popover 视图：核心状态 + 上游切换 + 运行中应用 + 应用列表与快速启动。

import { useCallback, useEffect, useState } from "react";
import { api, pickAppFile, type AppEntry, type CoreStatus, type SessionInfo } from "../api";

function formatUptime(secs: number): string {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

export default function Popover() {
  const [status, setStatus] = useState<CoreStatus | null>(null);
  const [apps, setApps] = useState<AppEntry[]>([]);
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [s, a, sess] = await Promise.all([api.getStatus(), api.listApps(), api.listSessions()]);
      setStatus(s);
      setApps(a);
      setSessions(sess);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
    const timer = setInterval(refresh, 3000);
    return () => clearInterval(timer);
  }, [refresh]);

  const handleToggleCore = async () => {
    setBusy(true);
    try {
      if (status?.running) {
        await api.stopCore();
      } else {
        await api.startCore();
      }
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleAddApp = async () => {
    const path = await pickAppFile();
    if (!path) return;
    try {
      await api.addApp(path);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleLaunch = async (id: string) => {
    setBusy(true);
    try {
      await api.launchApp(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const handleStop = async (id: string) => {
    try {
      await api.stopApp(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleRemove = async (id: string) => {
    try {
      await api.removeApp(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const runningIds = new Set(sessions.map((s) => s.entry_id));

  return (
    <main className="popover">
      <header className="popover-header">
        <span className={`status-dot ${status?.running ? "on" : "off"}`} />
        <h1>LocalProxy</h1>
        <button
          className={`btn ${status?.running ? "btn-danger" : "btn-primary"}`}
          onClick={handleToggleCore}
          disabled={busy}
        >
          {status?.running ? "停止核心" : "启动核心"}
        </button>
      </header>

      {error && <p className="error">{error}</p>}

      <section className="card">
        <dl className="kv">
          <dt>状态</dt>
          <dd>
            {status?.running
              ? `运行中${status.pid ? ` (pid ${status.pid})` : ""} · ${formatUptime(status.uptime_secs)}`
              : "未运行"}
          </dd>
          <dt>当前上游</dt>
          <dd>
            {status?.active_upstream || "未配置"}
            {status?.upstream_type ? ` (${status.upstream_type})` : ""}
          </dd>
          <dt>SOCKS5</dt>
          <dd>127.0.0.1:{status?.socks5_port ?? "-"}</dd>
          <dt>HTTP</dt>
          <dd>127.0.0.1:{status?.http_port ?? "-"}</dd>
        </dl>
      </section>

      <section className="section">
        <div className="section-head">
          <h2>运行中</h2>
          <span className="muted">{sessions.length}</span>
        </div>
        {sessions.length === 0 ? (
          <p className="muted">暂无运行中的被代理应用</p>
        ) : (
          <ul className="list">
            {sessions.map((s) => (
              <li key={s.entry_id} className="list-item">
                <span className="item-name">{s.entry_name}</span>
                <span className="muted">pid {s.pid}</span>
                <button className="btn btn-small" onClick={() => handleStop(s.entry_id)}>
                  停止
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="section">
        <div className="section-head">
          <h2>应用</h2>
          <button className="btn btn-small" onClick={handleAddApp}>
            + 添加
          </button>
        </div>
        {apps.length === 0 ? (
          <p className="muted">尚未登记应用。添加后即可从代理模式启动。</p>
        ) : (
          <ul className="list">
            {apps.map((app) => (
              <li key={app.id} className="list-item">
                <span className="item-name" title={app.path}>
                  {app.name}
                </span>
                {runningIds.has(app.id) ? (
                  <span className="badge">运行中</span>
                ) : (
                  <button
                    className="btn btn-small btn-primary"
                    onClick={() => handleLaunch(app.id)}
                    disabled={busy}
                  >
                    代理启动
                  </button>
                )}
                <button
                  className="btn btn-small btn-ghost"
                  onClick={() => handleRemove(app.id)}
                  title="移除登记"
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <footer className="popover-footer">
        <span className="muted">菜单栏图标右键 → 设置</span>
      </footer>
    </main>
  );
}
