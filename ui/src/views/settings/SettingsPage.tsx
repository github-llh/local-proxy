// 设置窗口容器：加载配置、维护编辑草稿、保存与提示。
//
// 卡片只负责渲染与就地修改，保存时统一应用「独立节点高级参数」草稿。

import {useCallback, useEffect, useState} from "react";
import {emit} from "@tauri-apps/api/event";
import {api, type AppConfig, type AppEntry} from "../../api";
import {useConfigDraft} from "../../hooks/useConfigDraft";
import {useT} from "../../i18n";
import AdvancedCard from "./AdvancedCard";
import AppsCard from "./AppsCard";
import DnsUdpCard from "./DnsUdpCard";
import ListenCard from "./ListenCard";
import RoutingCard from "./RoutingCard";
import UpstreamCard from "./UpstreamCard";
import {applyAdvancedOptions} from "../ManagedNodeEditor";

export default function SettingsPage() {
  const t = useT();
  const {config, dirty, patch, replace, markDirty, current} = useConfigDraft();
  const [apps, setApps] = useState<AppEntry[]>([]);
  const [previousActive, setPreviousActive] = useState("");
  const [nodeDrafts, setNodeDrafts] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [justSaved, setJustSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const [loaded, list] = await Promise.all([api.getConfig(), api.listApps()]);
    replace(loaded);
    setPreviousActive(loaded.upstream.active);
    setApps(list);
    setNodeDrafts({});
    setError(null);
  }, [replace]);

  // 应用登记只影响列表，不重新加载配置，避免覆盖尚未保存的编辑。
  const refreshApps = useCallback(async () => {
    setApps(await api.listApps());
  }, []);

  useEffect(() => {
    refresh().catch((reason) => setError(String(reason)));
  }, [refresh]);

  const handleSave = useCallback(async () => {
    // 用同步副本读取，确保「失焦提交的列表草稿」在同一次保存中生效
    const draft = current();
    if (!draft) return;
    setSaving(true);
    setError(null);
    try {
      const snapshot = structuredClone(draft);
      snapshot.upstream.servers = snapshot.upstream.servers.map((server) => {
        const nodeDraft = nodeDrafts[server.id];
        if (server.managed && nodeDraft !== undefined) {
          try {
            return {...server, managed: applyAdvancedOptions(server.managed, nodeDraft)};
          } catch {
            throw new Error(`${server.name || server.id} · ${t("managed.invalidJson")}`);
          }
        }
        return server;
      });
      const next: AppConfig = await api.saveConfig(snapshot, previousActive);
      replace(next);
      setPreviousActive(next.upstream.active);
      setApps(next.apps);
      setNodeDrafts({});
      setJustSaved(true);
      window.setTimeout(() => setJustSaved(false), 2000);
      // 语言切换即时生效：广播到所有窗口（含 popover）
      void emit("language", next.ui.language).catch(() => {
      });
    } catch (reason) {
      setError(String(reason));
    } finally {
      setSaving(false);
    }
  }, [current, nodeDrafts, previousActive, replace, t]);

  // ⌘/Ctrl + S 保存
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "s") {
        event.preventDefault();
        // 先让正在编辑的输入框失焦，把列表草稿提交到编辑副本
        if (document.activeElement instanceof HTMLElement) {
          document.activeElement.blur();
        }
        void handleSave();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [handleSave]);

  const reload = async () => {
    try {
      await refresh();
    } catch (reason) {
      setError(String(reason));
    }
  };

  if (!config) {
    return (
      <main className="settings">
        <p className="muted">{error ?? t("settings.loading")}</p>
      </main>
    );
  }

  const status = saving
    ? t("settings.saving")
    : justSaved && !dirty
      ? t("settings.saved")
      : dirty
        ? t("settings.unsaved")
        : "";

  return (
    <main className="settings">
      <header className="settings-header">
        <h1>{t("settings.title")}</h1>
        <div className="header-actions">
          <span className={`save-state ${status ? "on" : ""}`} role="status">
            {status}
          </span>
          <button className="btn btn-small btn-ghost" onClick={reload} disabled={saving}>
            {t("settings.reload")}
          </button>
          <button className="btn btn-primary" onClick={handleSave} disabled={saving}>
            {saving ? t("settings.saving") : t("settings.save")}
          </button>
        </div>
      </header>

      {error && <p className="error" role="alert">{error}</p>}

      <ListenCard config={config} patch={patch}/>
      <UpstreamCard
        config={config}
        patch={patch}
        nodeDrafts={nodeDrafts}
        setNodeDrafts={setNodeDrafts}
        markDirty={markDirty}
      />
      <RoutingCard config={config} patch={patch}/>
      <DnsUdpCard config={config} patch={patch}/>
      <AppsCard apps={apps} onChanged={refreshApps}/>
      <AdvancedCard config={config} patch={patch}/>
    </main>
  );
}
