// 应用：登记、启用、编辑名称/备注/附加参数、移除。

import {useState} from "react";
import {api, type AppEntry, pickAppFile} from "../../api";
import {useT} from "../../i18n";

interface EditDraft {
  name: string;
  note: string;
  args: string;
  enabled: boolean;
}

export default function AppsCard({
                                   apps,
                                   onChanged,
                                 }: {
  apps: AppEntry[];
  onChanged: () => Promise<void> | void;
}) {
  const t = useT();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState<EditDraft>({name: "", note: "", args: "", enabled: true});

  const run = async (task: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await task();
      await onChanged();
    } catch (activationError) {
      setError(String(activationError));
    } finally {
      setBusy(false);
    }
  };

  const add = () =>
    run(async () => {
      const path = await pickAppFile();
      if (!path) return;
      await api.addApp(path);
    });

  const remove = (app: AppEntry) => run(async () => {
    await api.removeApp(app.id);
    if (editing === app.id) setEditing(null);
  });

  const startEditing = (app: AppEntry) => {
    setEditing(app.id);
    setDraft({
      name: app.name,
      note: app.note,
      args: app.extra_args.join(" "),
      enabled: app.enabled,
    });
  };

  const saveEditing = (app: AppEntry) =>
    run(async () => {
      await api.updateApp(app.id, {
        name: draft.name,
        note: draft.note,
        extraArgs: draft.args.split(/\s+/).filter(Boolean),
        enabled: draft.enabled,
      });
      setEditing(null);
    });

  return (
    <section className="card">
      <div className="card-head">
        <h2>{t("settings.apps")}</h2>
        <button className="btn btn-small" onClick={add} disabled={busy}>
          {t("settings.addApp")}
        </button>
      </div>
      {error && <p className="error" role="alert">{error}</p>}
      {apps.length === 0 ? (
        <p className="muted">{t("settings.noApps")}</p>
      ) : (
        <ul className="list">
          {apps.map((app) => (
            <li key={app.id} className="app-row">
              <div className={`list-item ${app.enabled ? "" : "dimmed"}`}>
                <input
                  type="checkbox"
                  checked={app.enabled}
                  disabled={busy}
                  title={t("popover.enableApp")}
                  onChange={(event) =>
                    run(async () => {
                      await api.setAppEnabled(app.id, event.target.checked);
                    })
                  }
                />
                <span className="item-name" title={app.path}>
                  {app.name}
                  {app.note ? <span className="muted"> · {app.note}</span> : null}
                </span>
                <span className="muted path">{app.path}</span>
                <button
                  className="btn btn-small btn-ghost"
                  onClick={() => (editing === app.id ? setEditing(null) : startEditing(app))}
                  disabled={busy}
                >
                  {editing === app.id ? t("settings.cancel") : t("settings.edit")}
                </button>
                <button
                  className="btn btn-small btn-ghost btn-danger-text"
                  onClick={() => remove(app)}
                  disabled={busy}
                >
                  {t("settings.remove")}
                </button>
              </div>
              {editing === app.id && (
                <div className="app-editor">
                  <div className="form-grid">
                    <label>
                      {t("settings.name")}
                      <input
                        value={draft.name}
                        onChange={(event) => setDraft({...draft, name: event.target.value})}
                      />
                    </label>
                    <label>
                      {t("settings.appNote")}
                      <input
                        value={draft.note}
                        placeholder={t("settings.appNoteHint")}
                        onChange={(event) => setDraft({...draft, note: event.target.value})}
                      />
                    </label>
                    <label>
                      {t("settings.appArgs")}
                      <input
                        value={draft.args}
                        placeholder="--proxy-server=..."
                        spellCheck={false}
                        autoComplete="off"
                        onChange={(event) => setDraft({...draft, args: event.target.value})}
                      />
                    </label>
                  </div>
                  <div className="row-actions">
                    <button
                      className="btn btn-small btn-primary"
                      onClick={() => saveEditing(app)}
                      disabled={busy}
                    >
                      {t("settings.save")}
                    </button>
                  </div>
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
