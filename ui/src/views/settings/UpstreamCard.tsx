// 上游代理：列表 + 选中详情、预设/手动新增、删除撤销、连通性测试。

import {useState} from "react";
import {api, type AppConfig, type UpstreamServer} from "../../api";
import {useT} from "../../i18n";
import UpstreamForm from "./UpstreamForm";
import {applyAdvancedOptions} from "../ManagedNodeEditor";

const emptyUpstream: UpstreamServer = {
  id: "",
  name: "",
  type: "socks5",
  host: "127.0.0.1",
  port: 7890,
  username: "",
  password: "",
};

const upstreamPresets: Record<string, Partial<UpstreamServer>> = {
  clash: {name: "Clash / mihomo", port: 7890},
  v2rayn: {name: "v2rayN", port: 10808},
  surge: {name: "Surge", port: 6153},
  stash: {name: "Stash", port: 7890},
  shadowsocks: {name: "Shadowsocks Windows", port: 1080},
  socks5: {name: "SOCKS5"},
  http: {name: "HTTP CONNECT", type: "http"},
};

interface RemovedUpstream {
  server: UpstreamServer;
  index: number;
}

export default function UpstreamCard({
                                       config,
                                       patch,
                                       nodeDrafts,
                                       setNodeDrafts,
                                       markDirty,
                                     }: {
  config: AppConfig;
  patch: (edit: (config: AppConfig) => void) => void;
  nodeDrafts: Record<string, string>;
  setNodeDrafts: (update: (drafts: Record<string, string>) => Record<string, string>) => void;
  markDirty: () => void;
}) {
  const t = useT();
  const [preset, setPreset] = useState("clash");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [removed, setRemoved] = useState<RemovedUpstream | null>(null);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<string | null>(null);

  const servers = config.upstream.servers;
  const activeId = selectedId ?? config.upstream.active;
  const server = servers.find((item) => item.id === activeId) ?? servers[0];

  // 新增不依赖选中项，空列表时也能恢复
  const addUpstream = (manual: boolean) => {
    const values: Partial<UpstreamServer> = manual
      ? {
        name: t("managed.newNode"),
        host: "",
        port: 443,
        managed: {protocol: "vless", options: {}},
      }
      : upstreamPresets[preset];
    const created = {
      ...emptyUpstream,
      ...structuredClone(values),
      id: crypto.randomUUID(),
    };
    setTestResult(null);
    setRemoved(null);
    setSelectedId(created.id);
    patch((c) => {
      c.upstream.servers.push(created);
      c.upstream.active = created.id;
    });
  };

  if (!server) {
    return (
      <section className="card">
        <h2>{t("settings.upstream")}</h2>
        <p className="muted">{t("settings.noUpstreams")}</p>
        <div className="row-actions">
          <button className="btn btn-small" onClick={() => addUpstream(false)}>
            {t("settings.addUpstream")}
          </button>
          <button className="btn btn-small" onClick={() => addUpstream(true)}>
            {t("managed.add")}
          </button>
        </div>
      </section>
    );
  }

  const patchServer = (edit: (server: UpstreamServer) => void) =>
    patch((c) => {
      const target = c.upstream.servers.find((item) => item.id === server.id);
      if (target) edit(target);
    });

  const removeUpstream = () => {
    const index = config.upstream.servers.findIndex((item) => item.id === server.id);
    if (index < 0) return;
    setRemoved({server, index});
    setTestResult(null);
    setSelectedId(null);
    patch((c) => {
      c.upstream.servers = c.upstream.servers.filter((item) => item.id !== server.id);
      if (c.upstream.active === server.id) {
        c.upstream.active = c.upstream.servers[0]?.id ?? "";
      }
    });
  };

  const undoRemove = () => {
    if (!removed) return;
    const {server: restored, index} = removed;
    setRemoved(null);
    setSelectedId(restored.id);
    patch((c) => {
      const next = [...c.upstream.servers];
      next.splice(Math.min(index, next.length), 0, restored);
      c.upstream.servers = next;
    });
  };

  const withNodeDraft = (target: UpstreamServer): UpstreamServer => {
    const draft = nodeDrafts[target.id];
    if (target.managed && draft !== undefined) {
      return {...target, managed: applyAdvancedOptions(target.managed, draft)};
    }
    return target;
  };

  const test = async () => {
    setTesting(true);
    setTestResult(null);
    try {
      const draft = withNodeDraft(server);
      const result = await api.testUpstream(draft.id, draft, config.mihomo_path);
      setTestResult(result.kind === "port" ? `${result.elapsed_ms} ms` : t("managed.engineReady"));
    } catch (error) {
      setTestResult(String(error));
    } finally {
      setTesting(false);
    }
  };

  return (
    <section className="card">
      <div className="card-head">
        <h2>{t("settings.upstream")}</h2>
        <span className="muted">{t("settings.upstreamCount").replace("{count}", `${servers.length}`)}</span>
      </div>

      <ul className="list upstream-list">
        {servers.map((item) => (
          <li
            key={item.id}
            className={`list-item upstream-item ${item.id === server.id ? "selected" : ""}`}
          >
            <button
              type="button"
              className="upstream-pick"
              onClick={() => {
                setSelectedId(item.id);
                setTestResult(null);
              }}
            >
              <span className="item-name">
                {item.name || item.id}
                <span className="muted">
                  {" "}
                  · {displayType(item)} · {endpointOf(item)}
                </span>
              </span>
            </button>
            {item.id === config.upstream.active ? (
              <span className="badge ok">{t("settings.upstreamActive")}</span>
            ) : (
              <button
                className="btn btn-small btn-ghost"
                onClick={() => patch((c) => (c.upstream.active = item.id))}
              >
                {t("settings.setActive")}
              </button>
            )}
          </li>
        ))}
      </ul>

      {removed && (
        <p className="muted undo-row">
          {t("settings.upstreamRemoved").replace("{name}", removed.server.name || removed.server.id)}
          <button className="btn btn-small" onClick={undoRemove}>
            {t("settings.undo")}
          </button>
        </p>
      )}

      <div className="upstream-detail">
        <UpstreamForm
          server={server}
          config={config}
          onServer={patchServer}
          onConfig={patch}
          nodeDraft={nodeDrafts[server.id]}
          onNodeDraft={(value) => {
            setNodeDrafts((drafts) => {
              const next = {...drafts};
              if (value === undefined) {
                delete next[server.id];
              } else {
                next[server.id] = value;
              }
              return next;
            });
            markDirty();
          }}
        />
      </div>

      <div className="row-actions">
        <button className="btn btn-small" onClick={test} disabled={testing}>
          {testing ? "…" : t(server.managed ? "managed.testEngine" : "settings.testUpstream")}
        </button>
        {testResult && <span className="muted" role="status">{testResult}</span>}
      </div>

      <div className="row-actions separated">
        <select
          aria-label={t("settings.upstreamPreset")}
          value={preset}
          onChange={(event) => setPreset(event.target.value)}
        >
          {Object.entries(upstreamPresets).map(([key, profile]) => (
            <option key={key} value={key}>
              {profile.name}
            </option>
          ))}
        </select>
        <button className="btn btn-small" onClick={() => addUpstream(false)}>
          {t("settings.addUpstream")}
        </button>
        <button className="btn btn-small" onClick={() => addUpstream(true)}>
          {t("managed.add")}
        </button>
        <span className="spacer"/>
        {servers.length > 1 && (
          <button className="btn btn-small btn-ghost btn-danger-text" onClick={removeUpstream}>
            {t("settings.deleteUpstream")}
          </button>
        )}
      </div>
    </section>
  );
}

function displayType(server: UpstreamServer): string {
  return server.managed?.protocol ?? server.type;
}

function endpointOf(server: UpstreamServer): string {
  const host = server.host.trim();
  if (!host) return server.managed ? "—" : `${server.port}`;
  return host.includes(":") ? `[${host}]:${server.port}` : `${host}:${server.port}`;
}
