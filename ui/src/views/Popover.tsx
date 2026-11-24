// popover 视图：核心状态 + 上游切换 + 运行中应用 + 应用列表与快速启动。

import { useCallback, useEffect, useState } from "react";
import { api, pickAppFile, type AppEntry, type CoreStatus, type SessionInfo } from "../api";
import { useT } from "../i18n";

function formatUptime(secs: number): string {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

export default function Popover() {
  const t = useT();
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
        <h1>{t("app.name")}</h1>
        <button
          className={`btn ${status?.running ? "btn-danger" : "btn-primary"}`}
          onClick={handleToggleCore}
          disabled={busy}
        >
          {status?.running ? t("popover.stopCore") : t("popover.startCore")}
        </button>
      </header>

      {error && <p className="error">{error}</p>}

      <section className="card">
        <dl className="kv">
          <dt>{t("popover.status")}</dt>
          <dd>
            {status?.running
              ? `${t("popover.running")}${status.pid ? ` (pid ${status.pid})` : ""} · ${formatUptime(status.uptime_secs)}`
              : t("popover.notRunning")}
          </dd>
          <dt>{t("popover.upstream")}</dt>
          <dd>
            {status?.active_upstream || t("popover.notConfigured")}
            {status?.upstream_type ? ` (${status.upstream_type})` : ""}
          </dd>
          <dt>{t("popover.socks5")}</dt>
          <dd>127.0.0.1:{status?.socks5_port ?? "-"}</dd>
          <dt>{t("popover.http")}</dt>
          <dd>127.0.0.1:{status?.http_port ?? "-"}</dd>
        </dl>
      </section>

      <section className="section">
        <div className="section-head">
          <h2>{t("popover.runningApps")}</h2>
          <span className="muted">{sessions.length}</span>
        </div>
        {sessions.length === 0 ? (
          <p className="muted">{t("popover.noRunningApps")}</p>
        ) : (
          <ul className="list">
            {sessions.map((s) => (
              <li key={s.entry_id} className="list-item">
                <span className="item-name">{s.entry_name}</span>
                <span className="muted">pid {s.pid}</span>
                <button className="btn btn-small" onClick={() => handleStop(s.entry_id)}>
                  {t("popover.stop")}
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="section">
        <div className="section-head">
          <h2>{t("popover.apps")}</h2>
          <button className="btn btn-small" onClick={handleAddApp}>
            {t("popover.add")}
          </button>
        </div>
        {apps.length === 0 ? (
          <p className="muted">{t("popover.noApps")}</p>
        ) : (
          <ul className="list">
            {apps.map((app) => (
              <li key={app.id} className="list-item">
                <span className="item-name" title={app.path}>
                  {app.name}
                </span>
                {runningIds.has(app.id) ? (
                  <span className="badge">{t("popover.running")}</span>
                ) : (
                  <button
                    className="btn btn-small btn-primary"
                    onClick={() => handleLaunch(app.id)}
                    disabled={busy}
                  >
                    {t("popover.launch")}
                  </button>
                )}
                <button
                  className="btn btn-small btn-ghost"
                  onClick={() => handleRemove(app.id)}
                  title="remove"
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <footer className="popover-footer">
        <span className="muted">{t("popover.footerHint")}</span>
      </footer>
    </main>
  );
}
