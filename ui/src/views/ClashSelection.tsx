import {useEffect, useState} from "react";
import {api, type ClashState} from "../api";
import {useT} from "../i18n";

export default function ClashSelection({upstreamId, onChanged}: {
  upstreamId: string;
  onChanged: () => void;
}) {
  const t = useT();
  const [snapshot, setSnapshot] = useState<ClashState | null>(null);
  const [groupName, setGroupName] = useState("");
  const [revision, setRevision] = useState(0);
  const [loading, setLoading] = useState(true);
  const [switching, setSwitching] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    api.getClashState(upstreamId).then(
      (data) => {
        if (active) setSnapshot(data);
      },
      (e) => {
        if (active) {
          setSnapshot(null);
          setError(String(e));
        }
      }
    ).finally(() => {
      if (active) setLoading(false);
    });
    return () => {
      active = false;
    };
  }, [upstreamId, revision]);

  const refresh = () => {
    setLoading(true);
    setError(null);
    setRevision((value) => value + 1);
  };

  const group = snapshot?.groups.find((group) => group.name === groupName)
    ?? snapshot?.groups.find((group) => group.name === "GLOBAL")
    ?? snapshot?.groups[0];
  const busy = loading || switching;
  const mode = snapshot?.mode;
  const modeHint = mode === "rule" || mode === "global" || mode === "direct" ? t(`clash.mode.${mode}`) : null;

  const select = async (choice: string) => {
    if (!group || !choice || busy) return;
    setSwitching(true);
    setError(null);
    try {
      await api.selectClashProxy(upstreamId, group.name, choice);
      onChanged();
      refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setSwitching(false);
    }
  };

  return (
    <section className="section">
      <div className="section-head">
        <h2>{t("clash.title")}</h2>
        <button className="btn btn-small" onClick={refresh} disabled={busy}>
          {loading ? t("settings.loading") : t("clash.refresh")}
        </button>
      </div>
      {error && <p className="error" role="alert">{error}</p>}
      {snapshot && (
        <div className="card">
          {modeHint && <p className="muted">{modeHint}</p>}
          {group ? (
            <div className="form-grid">
              <label>
                {t("clash.group")}
                <select value={group.name} disabled={busy} onChange={(e) => setGroupName(e.target.value)}>
                  {snapshot.groups.map((group) => <option key={group.name} value={group.name}>{group.name}</option>)}
                </select>
              </label>
              <label>
                {t("clash.node")}
                <select
                  value={group.choices.includes(group.selected) ? group.selected : ""}
                  disabled={busy || snapshot.mode === "direct"}
                  onChange={(e) => select(e.target.value)}
                >
                  {!group.choices.includes(group.selected) &&
                    <option value="" disabled>{t("clash.chooseNode")}</option>}
                  {group.choices.map((choice) => <option key={choice} value={choice}>{choice}</option>)}
                </select>
              </label>
            </div>
          ) : <p className="muted">{t("clash.noGroups")}</p>}
          {group && snapshot.mode !== "direct" && <p className="muted">{t("clash.shared")}</p>}
        </div>
      )}
    </section>
  );
}
