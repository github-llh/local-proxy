// 设置窗口：端口 / 上游 / 路由规则 / DNS 与 UDP / 高级。

import {useCallback, useEffect, useState} from "react";
import {api, type AppConfig, type AppEntry, pickAppFile, type RoutingRule, type UpstreamServer} from "../api";
import {useT} from "../i18n";
import {open} from "@tauri-apps/plugin-dialog";
import ManagedNodeEditor, {applyAdvancedOptions} from "./ManagedNodeEditor";

const emptyUpstream: UpstreamServer = {
  id: "",
  name: "",
  type: "socks5",
  host: "127.0.0.1",
  port: 7890,
  username: "",
  password: "",
};

const defaultClashController = {address: "127.0.0.1:9090", secret: ""};

const upstreamPresets: Record<string, Partial<UpstreamServer>> = {
  clash: {name: "Clash / mihomo", port: 7890},
  v2rayn: {name: "v2rayN", port: 10808},
  surge: {name: "Surge", port: 6153},
  stash: {name: "Stash", port: 7890},
  shadowsocks: {name: "Shadowsocks Windows", port: 1080},
  socks5: {name: "SOCKS5"},
  http: {name: "HTTP CONNECT", type: "http"},
};

const emptyRule: RoutingRule = {
  name: "",
  enabled: true,
  action: "proxy",
  priority: 0,
  ip_cidrs_v4: [],
  ip_cidrs_v6: [],
  domains: [],
  ports: [],
  protocols: ["tcp"],
};

export default function Settings() {
  const t = useT();
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [apps, setApps] = useState<AppEntry[]>([]);
  const [previousActive, setPreviousActive] = useState("");
  const [saved, setSaved] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<string | null>(null);
  const [preset, setPreset] = useState("clash");
  const [nodeDrafts, setNodeDrafts] = useState<Record<string, string>>({});
  const [engineResult, setEngineResult] = useState<string | null>(null);
  const [checkingEngine, setCheckingEngine] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [c, a] = await Promise.all([api.getConfig(), api.listApps()]);
      setConfig(c);
      setPreviousActive(c.upstream.active);
      setApps(a);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const patch = (fn: (c: AppConfig) => void) => {
    setConfig((prev) => {
      if (!prev) return prev;
      const next = structuredClone(prev);
      fn(next);
      return next;
    });
    setSaved(false);
  };

  function withNodeDraft(server: UpstreamServer): UpstreamServer {
    const draft = nodeDrafts[server.id];
    if (server.managed && draft !== undefined) {
      try {
        return {...server, managed: applyAdvancedOptions(server.managed, draft)};
      } catch {
        throw new Error(`${server.name || server.id} · ${t("managed.invalidJson")}`);
      }
    }
    return server;
  }

  const handleSave = async () => {
    if (!config) return;
    setSaving(true);
    setError(null);
    try {
      const snapshot = structuredClone(config);
      snapshot.upstream.servers = snapshot.upstream.servers.map(withNodeDraft);
      const next = await api.saveConfig(snapshot, previousActive);
      setConfig((draft) => draft === config ? next : draft);
      setPreviousActive(next.upstream.active);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleTestUpstream = async () => {
    if (!activeUpstream) return;
    setTesting(true);
    setTestResult(null);
    try {
      const result = await api.testUpstream(activeUpstream.id, withNodeDraft(activeUpstream), config?.mihomo_path);
      setTestResult(result.kind === "port" ? `${result.elapsed_ms} ms` : t("managed.engineReady"));
    } catch (e) {
      setTestResult(String(e));
    } finally {
      setTesting(false);
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

  if (!config) {
    return (
      <main className="settings">
        <p className="muted">{error ?? t("settings.loading")}</p>
      </main>
    );
  }

  const activeUpstream =
    config.upstream.servers.find((s) => s.id === config.upstream.active) ?? config.upstream.servers[0];
  const clashController = activeUpstream?.clash;
  const managed = activeUpstream?.managed;

  function addUpstream(manual: boolean) {
    const values: Partial<UpstreamServer> = manual
      ? {name: t("managed.newNode"), host: "", port: 443, managed: {protocol: "vless", options: {}}}
      : upstreamPresets[preset];
    const upstream = {...emptyUpstream, ...structuredClone(values), id: crypto.randomUUID()};
    setTestResult(null);
    patch((c) => {
      c.upstream.servers.push(upstream);
      c.upstream.active = upstream.id;
    });
  }

  async function chooseEngine() {
    try {
      const path = await open({multiple: false, directory: false, title: t("managed.chooseEngine")});
      if (typeof path === "string") {
        patch((c) => (c.mihomo_path = path));
        setEngineResult(null);
      }
    } catch (e) {
      setEngineResult(String(e));
    }
  }

  async function checkEngine() {
    setCheckingEngine(true);
    try {
      setEngineResult(await api.findMihomo(config?.mihomo_path ?? ""));
    } catch (e) {
      setEngineResult(String(e));
    } finally {
      setCheckingEngine(false);
    }
  }

  const patchUpstream = (edit: (server: UpstreamServer) => void) => {
    patch((c) => {
      const server = c.upstream.servers.find((s) => s.id === activeUpstream?.id);
      if (server) edit(server);
    });
  };

  return (
    <main className="settings">
      <header className="settings-header">
        <h1>{t("settings.title")}</h1>
        <div className="header-actions">
          {saved && <span className="badge ok">{t("settings.saved")}</span>}
          <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
            {t("settings.save")}
          </button>
        </div>
      </header>

      {error && <p className="error">{error}</p>}

      <section className="card">
        <h2>{t("settings.listen")}</h2>
        <div className="form-grid">
          <label>
            {t("settings.listenHost")}
            <input
              value={config.listen_host}
              onChange={(e) => patch((c) => (c.listen_host = e.target.value))}
            />
          </label>
          <label>
            {t("settings.socks5Port")}
            <input
              type="number"
              value={config.socks5_port}
              onChange={(e) => patch((c) => (c.socks5_port = Number(e.target.value)))}
            />
          </label>
          <label>
            {t("settings.httpPort")}
            <input
              type="number"
              value={config.http_port}
              onChange={(e) => patch((c) => (c.http_port = Number(e.target.value)))}
            />
          </label>
          <label>
            {t("settings.logLevel")}
            <select
              value={config.log_level}
              onChange={(e) => patch((c) => (c.log_level = e.target.value))}
            >
              <option value="error">error</option>
              <option value="warn">warn</option>
              <option value="info">info</option>
              <option value="debug">debug</option>
            </select>
          </label>
        </div>
      </section>

      <section className="card">
        <h2>{t("settings.upstream")}</h2>
        <div className="form-grid">
          <label>
            {t("settings.activeUpstream")}
            <select
              value={config.upstream.active}
              onChange={(e) => {
                setTestResult(null);
                patch((c) => (c.upstream.active = e.target.value));
              }}
            >
              {config.upstream.servers.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name || s.id}
                </option>
              ))}
            </select>
          </label>
        </div>
        {activeUpstream && (
          <div className="form-grid">
            <label>
              {t("settings.name")}
              <input
                value={activeUpstream.name}
                onChange={(e) =>
                  patch((c) => {
                    const s = c.upstream.servers.find((x) => x.id === activeUpstream.id);
                    if (s) s.name = e.target.value;
                  })
                }
              />
            </label>
            {!managed && (
              <label>
                {t("settings.type")}
                <select
                  value={activeUpstream.type}
                  onChange={(e) =>
                    patch((c) => {
                      const s = c.upstream.servers.find((x) => x.id === activeUpstream.id);
                      if (s) s.type = e.target.value;
                    })
                  }
                >
                  <option value="socks5">socks5</option>
                  <option value="http">http</option>
                </select>
              </label>
            )}
            <label>
              {t("settings.host")}
              <input
                value={activeUpstream.host}
                onChange={(e) =>
                  patch((c) => {
                    const s = c.upstream.servers.find((x) => x.id === activeUpstream.id);
                    if (s) s.host = e.target.value;
                  })
                }
              />
            </label>
            <label>
              {t("settings.port")}
              <input
                type="number"
                value={activeUpstream.port}
                onChange={(e) =>
                  patch((c) => {
                    const s = c.upstream.servers.find((x) => x.id === activeUpstream.id);
                    if (s) s.port = Number(e.target.value);
                  })
                }
              />
            </label>
            {!managed && <>
              <label>
                {t("settings.username")}
                <input
                  value={activeUpstream.username}
                  onChange={(e) =>
                    patch((c) => {
                      const s = c.upstream.servers.find((x) => x.id === activeUpstream.id);
                      if (s) s.username = e.target.value;
                    })
                  }
                />
              </label>
              <label>
                {t("settings.password")}
                <input
                  type="password"
                  value={activeUpstream.password}
                  onChange={(e) =>
                    patch((c) => {
                      const s = c.upstream.servers.find((x) => x.id === activeUpstream.id);
                      if (s) s.password = e.target.value;
                    })
                  }
                />
              </label>
            </>}
          </div>
        )}
        {activeUpstream && managed && (
          <>
            <ManagedNodeEditor
              node={managed}
              draft={nodeDrafts[activeUpstream.id]}
              onNode={(node) => patchUpstream((server) => (server.managed = node))}
              onDraft={(value) => {
                setNodeDrafts((drafts) => {
                  const next = {...drafts};
                  if (value === undefined) {
                    delete next[activeUpstream.id];
                  } else {
                    next[activeUpstream.id] = value;
                  }
                  return next;
                });
                setSaved(false);
              }}
            />
            <div className="form-grid">
              <label>
                {t("managed.engine")}
                <input
                  value={config.mihomo_path}
                  placeholder={t("managed.autoEngine")}
                  onChange={(event) => {
                    patch((c) => (c.mihomo_path = event.target.value));
                    setEngineResult(null);
                  }}
                />
              </label>
            </div>
            <div className="row-actions">
              <button className="btn btn-small" onClick={chooseEngine}>{t("managed.chooseEngine")}</button>
              <button className="btn btn-small" onClick={checkEngine} disabled={checkingEngine}>
                {t("managed.checkEngine")}
              </button>
            </div>
            {engineResult && <p className="muted" role="status">{engineResult}</p>}
          </>
        )}
        {activeUpstream && !managed && (
          <>
            <label className="checkbox">
              <input
                type="checkbox"
                checked={clashController !== undefined}
                onChange={(e) => patchUpstream((server) => {
                  if (e.target.checked) {
                    server.clash = {...defaultClashController};
                  } else {
                    delete server.clash;
                  }
                })}
              />
              {t("clash.enable")}
            </label>
            {clashController && (
              <>
                <div className="form-grid">
                  <label>
                    {t("clash.address")}
                    <input
                      value={clashController.address}
                      onChange={(e) => {
                        const clash = {...clashController, address: e.target.value};
                        patchUpstream((server) => (server.clash = clash));
                      }}
                    />
                  </label>
                  <label>
                    {t("clash.secret")}
                    <input
                      type="password"
                      value={clashController.secret}
                      onChange={(e) => {
                        const clash = {...clashController, secret: e.target.value};
                        patchUpstream((server) => (server.clash = clash));
                      }}
                    />
                  </label>
                </div>
                <p className="muted">{t("clash.settingsHint")}</p>
              </>
            )}
          </>
        )}
        <div className="row-actions">
          <button className="btn btn-small" onClick={handleTestUpstream} disabled={testing}>
            {testing ? "…" : t(managed ? "managed.testEngine" : "settings.testUpstream")}
          </button>
          {testResult && <span className="muted">{testResult}</span>}
          <select aria-label={t("settings.upstreamPreset")} value={preset}
                  onChange={(event) => setPreset(event.target.value)}>
            {Object.entries(upstreamPresets).map(([key, profile]) =>
              <option key={key} value={key}>{profile.name}</option>
            )}
          </select>
          <button className="btn btn-small" onClick={() => addUpstream(false)}>
            {t("settings.addUpstream")}
          </button>
          <button className="btn btn-small" onClick={() => addUpstream(true)}>
            {t("managed.add")}
          </button>
          {config.upstream.servers.length > 1 && (
            <button
              className="btn btn-small btn-ghost"
              onClick={() =>
                patch((c) => {
                  c.upstream.servers = c.upstream.servers.filter((s) => s.id !== activeUpstream?.id);
                  if (c.upstream.active === activeUpstream?.id) {
                    c.upstream.active = c.upstream.servers[0]?.id ?? "";
                  }
                })
              }
            >
              {t("settings.deleteUpstream")}
            </button>
          )}
        </div>
      </section>

      <section className="card">
        <h2>{t("settings.routing")}</h2>
        <div className="form-grid">
          <label>
            {t("settings.defaultAction")}
            <select
              value={config.routing.default_action}
              onChange={(e) => patch((c) => (c.routing.default_action = e.target.value))}
            >
              <option value="proxy">{t("settings.defaultActionProxy")}</option>
              <option value="direct">{t("settings.defaultActionDirect")}</option>
            </select>
          </label>
          <label>
            {t("settings.priorityMode")}
            <select
              value={config.routing.priority_mode}
              onChange={(e) => patch((c) => (c.routing.priority_mode = e.target.value))}
            >
              <option value="order">{t("settings.priorityOrder")}</option>
              <option value="number">{t("settings.priorityNumber")}</option>
            </select>
          </label>
        </div>
        <label className="checkbox">
          <input
            type="checkbox"
            checked={config.routing.use_default_private}
            onChange={(e) => patch((c) => (c.routing.use_default_private = e.target.checked))}
          />
          {t("settings.autoPrivate")}
        </label>
        <table className="rules-table">
          <thead>
          <tr>
            <th>{t("settings.enabled")}</th>
            <th>{t("settings.name")}</th>
            <th>{t("settings.action")}</th>
            <th>{t("settings.domains")}</th>
            <th>{t("settings.port")}</th>
            <th></th>
          </tr>
          </thead>
          <tbody>
          {config.routing.rules.map((rule, idx) => (
            <tr key={idx}>
              <td>
                <input
                  type="checkbox"
                  checked={rule.enabled}
                  onChange={(e) =>
                    patch((c) => (c.routing.rules[idx].enabled = e.target.checked))
                  }
                />
              </td>
              <td>
                <input
                  value={rule.name}
                  onChange={(e) => patch((c) => (c.routing.rules[idx].name = e.target.value))}
                />
              </td>
              <td>
                <select
                  value={rule.action}
                  onChange={(e) => patch((c) => (c.routing.rules[idx].action = e.target.value))}
                >
                  <option value="proxy">proxy</option>
                  <option value="direct">direct</option>
                </select>
              </td>
              <td>
                <input
                  placeholder=".cn, .example.com"
                  value={rule.domains.join(", ")}
                  onChange={(e) =>
                    patch((c) => {
                      c.routing.rules[idx].domains = e.target.value
                        .split(",")
                        .map((s) => s.trim())
                        .filter(Boolean);
                    })
                  }
                />
              </td>
              <td>
                <input
                  placeholder="80, 443"
                  value={rule.ports.join(", ")}
                  onChange={(e) =>
                    patch((c) => {
                      c.routing.rules[idx].ports = e.target.value
                        .split(",")
                        .map((s) => s.trim())
                        .filter(Boolean);
                    })
                  }
                />
              </td>
              <td>
                <button
                  className="btn btn-small btn-ghost"
                  onClick={() => patch((c) => c.routing.rules.splice(idx, 1))}
                >
                  ×
                </button>
              </td>
            </tr>
          ))}
          </tbody>
        </table>
        <button
          className="btn btn-small"
          onClick={() => patch((c) => c.routing.rules.push({...emptyRule}))}
        >
          {t("settings.addRule")}
        </button>
      </section>

      <section className="card">
        <h2>{t("settings.dnsUdp")}</h2>
        <div className="form-grid">
          <label>
            {t("settings.dnsMode")}
            <select
              value={config.dns_mode}
              onChange={(e) => patch((c) => (c.dns_mode = e.target.value))}
            >
              <option value="proxy">{t("settings.dnsProxy")}</option>
              <option value="direct">{t("settings.dnsDirect")}</option>
            </select>
          </label>
          <label>
            {t("settings.udpMode")}
            <select
              value={config.proxy_rules.udp_mode}
              onChange={(e) => patch((c) => (c.proxy_rules.udp_mode = e.target.value))}
            >
              <option value="auto">auto</option>
              <option value="proxy">proxy</option>
              <option value="direct">direct</option>
              <option value="block">block</option>
            </select>
          </label>
          <label>
            {t("settings.udpFallback")}
            <select
              value={config.proxy_rules.udp_fallback}
              onChange={(e) => patch((c) => (c.proxy_rules.udp_fallback = e.target.value))}
            >
              <option value="block">{t("settings.udpFallbackBlock")}</option>
              <option value="direct">{t("settings.udpFallbackDirect")}</option>
            </select>
          </label>
          <label>
            {t("settings.ipv6")}
            <select
              value={config.proxy_rules.ipv6_mode}
              onChange={(e) => patch((c) => (c.proxy_rules.ipv6_mode = e.target.value))}
            >
              <option value="proxy">proxy</option>
              <option value="direct">direct</option>
              <option value="block">block</option>
            </select>
          </label>
        </div>
        <p className="muted">{t("settings.allowedPorts")}</p>
        <input
          value={config.proxy_rules.allowed_ports.join(", ")}
          onChange={(e) =>
            patch((c) => {
              c.proxy_rules.allowed_ports = e.target.value
                .split(",")
                .map((s) => Number(s.trim()))
                .filter((n) => Number.isFinite(n) && n > 0 && n <= 65535);
            })
          }
        />
      </section>

      <section className="card">
        <h2>{t("settings.apps")}</h2>
        <div className="row-actions">
          <button className="btn btn-small" onClick={handleAddApp}>
            {t("settings.addApp")}
          </button>
        </div>
        <ul className="list">
          {apps.map((app) => (
            <li key={app.id} className="list-item">
              <span className="item-name">{app.name}</span>
              <span className="muted path">{app.path}</span>
            </li>
          ))}
        </ul>
      </section>

      <section className="card">
        <h2>{t("settings.advanced")}</h2>
        <label className="checkbox">
          <input
            type="checkbox"
            checked={config.ui.launch_at_login}
            onChange={(e) => patch((c) => (c.ui.launch_at_login = e.target.checked))}
          />
          {t("settings.launchAtLogin")}
        </label>

        <label className="checkbox">
          <input
            type="checkbox"
            checked={config.ui.exit_ip_probe}
            onChange={(e) => patch((c) => (c.ui.exit_ip_probe = e.target.checked))}
          />
          {t("settings.exitIpProbe")}
        </label>
        <p className="muted">{t("settings.probeEndpointsHint")}</p>
        <textarea
          rows={2}
          value={config.probe.endpoints.join("\n")}
          onChange={(e) =>
            patch((c) => {
              c.probe.endpoints = e.target.value
                .split("\n")
                .map((s) => s.trim())
                .filter(Boolean);
            })
          }
        />
        <label>
          {t("settings.probeTimeout")}
          <input
            type="number"
            value={config.probe.timeout_ms}
            onChange={(e) => patch((c) => (c.probe.timeout_ms = Number(e.target.value)))}
          />
        </label>
      </section>
    </main>
  );
}
