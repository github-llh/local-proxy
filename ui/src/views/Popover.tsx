// popover 视图：核心状态 + 上游切换 + 运行中应用 + 应用列表与快速启动。

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  api,
  pickAppFile,
  type AppEntry,
  type ConnectionRecord,
  type CoreStatus,
  type ExitIpInfo,
  type SessionInfo,
  type UpstreamServer,
} from "../api";
import { useT } from "../i18n";
import { checkConsistency, systemContext } from "../consistency";
import { open } from "@tauri-apps/plugin-shell";

function formatUptime(secs: number): string {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}


/// 一致性徽标（出口国家 vs 系统时区/语言）。
function ConsistencyBadge({ countryCode }: { countryCode: string }) {
  const t = useT();
  const { timeZone, language } = systemContext();
  const result = checkConsistency(countryCode, timeZone, language);
  if (result.level === "ok") {
    return <p className="consistency ok">✅ {t("risk.consistent")}</p>;
  }
  if (result.level === "warn") {
    return (
      <p className="consistency warn" title={`${timeZone} / ${language}`}>
        ⚠️ {t("risk.mismatch")} {result.detail}
      </p>
    );
  }
  return null;
}

export default function Popover() {
  const t = useT();
  const [status, setStatus] = useState<CoreStatus | null>(null);
  const [apps, setApps] = useState<AppEntry[]>([]);
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [showUpstreams, setShowUpstreams] = useState(false);
  const [upstreams, setUpstreams] = useState<UpstreamServer[]>([]);
  const [activeUpstreamId, setActiveUpstreamId] = useState("");
  const [latencies, setLatencies] = useState<Record<string, number>>({});
  const [testingId, setTestingId] = useState<string | null>(null);
  const [exitIp, setExitIp] = useState<ExitIpInfo | null>(null);
  const [probing, setProbing] = useState(false);
  const [showConns, setShowConns] = useState(false);
  const [connections, setConnections] = useState<ConnectionRecord[]>([]);

  const refresh = useCallback(async () => {
    try {
      const [s, a, sess, cfg] = await Promise.all([
        api.getStatus(),
        api.listApps(),
        api.listSessions(),
        api.getConfig(),
      ]);
      setStatus(s);
      setApps(a);
      setSessions(sess);
      setUpstreams(cfg.upstream.servers);
      setActiveUpstreamId(cfg.upstream.active);
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

  useEffect(() => {
    if (!showConns) return;
    api
      .listRecentConnections(20)
      .then(setConnections)
      .catch(() => setConnections([]));
  }, [showConns]);

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

  const activeLatency = activeUpstreamId ? latencies[activeUpstreamId] ?? null : null;

  const handleSelectUpstream = async (id: string) => {
    try {
      await api.selectUpstream(id);
      setActiveUpstreamId(id);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleTestUpstream = async (id: string) => {
    setTestingId(id);
    try {
      const ms = await api.testUpstream(id);
      setLatencies((prev) => ({ ...prev, [id]: ms }));
    } catch (e) {
      setError(String(e));
    } finally {
      setTestingId(null);
    }
  };

  const handleProbe = async () => {
    setProbing(true);
    try {
      const info = await api.probeExitIp();
      setExitIp(info);
    } catch (e) {
      setError(String(e));
    } finally {
      setProbing(false);
    }
  };

  const handleLeakTest = async () => {
    try {
      await open("https://browserleaks.com/webrtc");
    } catch (e) {
      setError(String(e));
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

  // 最近启动的排前面（无记录的按名称）
  const sortedApps = useMemo(
    () =>
      [...apps].sort((a, b) => {
        const la = a.last_launched_at ?? 0;
        const lb = b.last_launched_at ?? 0;
        if (lb !== la) return lb - la;
        return a.name.localeCompare(b.name);
      }),
    [apps]
  );

  // 相对时间（刚刚 / N 分钟前 / N 小时前 / N 天前 / 日期）
  const formatRelative = (unixSecs: number): string => {
    const now = Math.floor(Date.now() / 1000);
    const diff = Math.max(0, now - unixSecs);
    if (diff < 60) return t("time.justNow");
    if (diff < 3600) return `${Math.floor(diff / 60)}${t("time.minutesAgo")}`;
    if (diff < 86400) return `${Math.floor(diff / 3600)}${t("time.hoursAgo")}`;
    if (diff < 86400 * 30) return `${Math.floor(diff / 86400)}${t("time.daysAgo")}`;
    return new Date(unixSecs * 1000).toLocaleDateString();
  };

  const handleToggleEnabled = async (id: string, enabled: boolean) => {
    try {
      await api.setAppEnabled(id, enabled);
      await refresh();
    } catch (e) {
      setError(String(e));
    }
  };

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
          <h2>{t("risk.title")}</h2>
          <div className="row-actions">
            <button className="btn btn-small" onClick={handleProbe} disabled={probing}>
              {probing ? "…" : t("risk.probe")}
            </button>
            <button className="btn btn-small btn-ghost" onClick={handleLeakTest}>
              {t("risk.leakTest")}
            </button>
          </div>
        </div>
        {exitIp ? (
          <div className="card risk-card">
            <dl className="kv">
              <dt>{t("risk.exitIp")}</dt>
              <dd>{exitIp.ip || "-"}</dd>
              {exitIp.country && (
                <>
                  <dt>{t("risk.location")}</dt>
                  <dd>
                    {exitIp.country}
                    {exitIp.city ? ` · ${exitIp.city}` : ""}
                  </dd>
                </>
              )}
              {exitIp.isp && (
                <>
                  <dt>ISP</dt>
                  <dd>{exitIp.isp}</dd>
                </>
              )}
            </dl>
            <ConsistencyBadge countryCode={exitIp.country_code} />
          </div>
        ) : (
          <p className="muted">{t("risk.hint")}</p>
        )}
      </section>

      <section className="section">
        <div className="section-head">
          <h2>{t("popover.upstream")}</h2>
          <button
            className="btn btn-small btn-ghost"
            onClick={() => setShowUpstreams((v) => !v)}
          >
            {showUpstreams ? t("popover.collapse") : t("popover.switch")}
          </button>
        </div>
        <div className="list-item upstream-active">
          <span className="item-name">
            {status?.active_upstream || t("popover.notConfigured")}
            {status?.upstream_type ? ` (${status.upstream_type})` : ""}
          </span>
          {activeLatency !== null && (
            <span className="muted">{activeLatency} ms</span>
          )}
        </div>
        {showUpstreams && (
          <ul className="list">
            {upstreams.map((u) => (
              <li key={u.id} className="list-item">
                <input
                  type="radio"
                  name="upstream"
                  checked={u.id === activeUpstreamId}
                  onChange={() => handleSelectUpstream(u.id)}
                  title={t("popover.selectUpstream")}
                />
                <span className="item-name" title={`${u.host}:${u.port}`}>
                  {u.name || u.id}
                  <span className="muted">
                    {" "}
                    {u.type} · {u.host}:{u.port}
                  </span>
                </span>
                <button
                  className="btn btn-small btn-ghost"
                  onClick={() => handleTestUpstream(u.id)}
                  disabled={testingId === u.id}
                  title={t("popover.testUpstream")}
                >
                  {testingId === u.id ? "…" : latencies[u.id] !== undefined ? `${latencies[u.id]}ms` : t("popover.test")}
                </button>
              </li>
            ))}
          </ul>
        )}
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
          <h2>{t("conn.title")}</h2>
          <button className="btn btn-small btn-ghost" onClick={() => setShowConns((v) => !v)}>
            {showConns ? t("conn.collapse") : t("conn.expand")}
          </button>
        </div>
        {showConns && (
          <>
            {connections.length === 0 ? (
              <p className="muted">{t("conn.empty")}</p>
            ) : (
              <ul className="list conn-list">
                {connections.map((c, i) => (
                  <li key={`${c.ts_ms}-${i}`} className="list-item">
                    <span className={`conn-dot ${c.action}`} />
                    <span className="item-name" title={`${c.host}:${c.port}${c.rule ? ` · ${c.rule}` : ""}`}>
                      {c.host}:{c.port}
                    </span>
                    <span className="muted">{c.action}</span>
                  </li>
                ))}
              </ul>
            )}
          </>
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
            {sortedApps.map((app) => (
              <li key={app.id} className={`list-item ${app.enabled ? "" : "dimmed"}`}>
                <input
                  type="checkbox"
                  checked={app.enabled}
                  onChange={(e) => handleToggleEnabled(app.id, e.target.checked)}
                  title={t("popover.enableApp")}
                />
                <span className="item-name" title={app.path}>
                  {app.name}
                  {app.last_launched_at ? (
                    <span className="muted"> · {formatRelative(app.last_launched_at)}</span>
                  ) : null}
                </span>
                {runningIds.has(app.id) ? (
                  <span className="badge">{t("popover.running")}</span>
                ) : (
                  <button
                    className="btn btn-small btn-primary"
                    onClick={() => handleLaunch(app.id)}
                    disabled={busy || !app.enabled}
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
