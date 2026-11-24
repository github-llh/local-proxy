// 设置窗口：端口 / 上游 / 路由规则 / DNS 与 UDP / 高级。

import { useCallback, useEffect, useState } from "react";
import { api, pickAppFile, type AppConfig, type AppEntry, type RoutingRule, type UpstreamServer } from "../api";

const emptyUpstream: UpstreamServer = {
  id: "",
  name: "",
  type: "socks5",
  host: "127.0.0.1",
  port: 7890,
  username: "",
  password: "",
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
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [apps, setApps] = useState<AppEntry[]>([]);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [c, a] = await Promise.all([api.getConfig(), api.listApps()]);
      setConfig(c);
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

  const handleSave = async () => {
    if (!config) return;
    try {
      await api.saveConfig(config);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
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

  if (!config) {
    return (
      <main className="settings">
        <p className="muted">{error ?? "加载中…"}</p>
      </main>
    );
  }

  const activeUpstream =
    config.upstream.servers.find((s) => s.id === config.upstream.active) ?? config.upstream.servers[0];

  return (
    <main className="settings">
      <header className="settings-header">
        <h1>LocalProxy 设置</h1>
        <div className="header-actions">
          {saved && <span className="badge ok">已保存</span>}
          <button className="btn btn-primary" onClick={handleSave}>
            保存
          </button>
        </div>
      </header>

      {error && <p className="error">{error}</p>}

      <section className="card">
        <h2>监听</h2>
        <div className="form-grid">
          <label>
            监听地址
            <input
              value={config.listen_host}
              onChange={(e) => patch((c) => (c.listen_host = e.target.value))}
            />
          </label>
          <label>
            SOCKS5 端口
            <input
              type="number"
              value={config.socks5_port}
              onChange={(e) => patch((c) => (c.socks5_port = Number(e.target.value)))}
            />
          </label>
          <label>
            HTTP CONNECT 端口
            <input
              type="number"
              value={config.http_port}
              onChange={(e) => patch((c) => (c.http_port = Number(e.target.value)))}
            />
          </label>
          <label>
            日志级别
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
        <h2>上游代理</h2>
        <div className="form-grid">
          <label>
            激活上游
            <select
              value={config.upstream.active}
              onChange={(e) => patch((c) => (c.upstream.active = e.target.value))}
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
              名称
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
            <label>
              类型
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
            <label>
              地址
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
              端口
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
            <label>
              用户名（可选）
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
              密码（可选）
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
          </div>
        )}
        <div className="row-actions">
          <button
            className="btn btn-small"
            onClick={() =>
              patch((c) => {
                const id = `up-${Date.now().toString(36)}`;
                c.upstream.servers.push({ ...emptyUpstream, id, name: `上游 ${c.upstream.servers.length + 1}` });
                c.upstream.active = id;
              })
            }
          >
            + 添加上游
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
              删除当前上游
            </button>
          )}
        </div>
      </section>

      <section className="card">
        <h2>路由规则</h2>
        <div className="form-grid">
          <label>
            默认动作
            <select
              value={config.routing.default_action}
              onChange={(e) => patch((c) => (c.routing.default_action = e.target.value))}
            >
              <option value="proxy">proxy（默认走代理）</option>
              <option value="direct">direct（默认直连）</option>
            </select>
          </label>
          <label>
            优先级模式
            <select
              value={config.routing.priority_mode}
              onChange={(e) => patch((c) => (c.routing.priority_mode = e.target.value))}
            >
              <option value="order">order（数组序）</option>
              <option value="number">number（priority 降序）</option>
            </select>
          </label>
        </div>
        <label className="checkbox">
          <input
            type="checkbox"
            checked={config.routing.use_default_private}
            onChange={(e) => patch((c) => (c.routing.use_default_private = e.target.checked))}
          />
          内网/回环地址自动直连（推荐）
        </label>
        <table className="rules-table">
          <thead>
            <tr>
              <th>启用</th>
              <th>名称</th>
              <th>动作</th>
              <th>域名 / CIDR</th>
              <th>端口</th>
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
          onClick={() => patch((c) => c.routing.rules.push({ ...emptyRule }))}
        >
          + 添加规则
        </button>
      </section>

      <section className="card">
        <h2>DNS 与 UDP</h2>
        <div className="form-grid">
          <label>
            DNS 模式
            <select
              value={config.dns_mode}
              onChange={(e) => patch((c) => (c.dns_mode = e.target.value))}
            >
              <option value="proxy">proxy（交上游解析，防泄露）</option>
              <option value="direct">direct（本地解析）</option>
            </select>
          </label>
          <label>
            UDP 模式
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
            UDP 失败降级
            <select
              value={config.proxy_rules.udp_fallback}
              onChange={(e) => patch((c) => (c.proxy_rules.udp_fallback = e.target.value))}
            >
              <option value="block">block（失败即阻断）</option>
              <option value="direct">direct（回退直连）</option>
            </select>
          </label>
          <label>
            IPv6
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
        <p className="muted">允许代理的端口（逗号分隔，留空 = 全部）</p>
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
        <h2>应用</h2>
        <div className="row-actions">
          <button className="btn btn-small" onClick={handleAddApp}>
            + 添加应用
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
        <h2>高级</h2>
        <label className="checkbox">
          <input
            type="checkbox"
            checked={config.ui.launch_at_login}
            onChange={(e) => patch((c) => (c.ui.launch_at_login = e.target.checked))}
          />
          开机自动启动（M3b 生效）
        </label>
        <label className="checkbox">
          <input
            type="checkbox"
            checked={config.ui.keep_core_alive}
            onChange={(e) => patch((c) => (c.ui.keep_core_alive = e.target.checked))}
          />
          App 退出后核心继续常驻
        </label>
        <label className="checkbox">
          <input
            type="checkbox"
            checked={config.ui.exit_ip_probe}
            onChange={(e) => patch((c) => (c.ui.exit_ip_probe = e.target.checked))}
          />
          启用出口 IP 探测（popover 显示）
        </label>
      </section>
    </main>
  );
}
