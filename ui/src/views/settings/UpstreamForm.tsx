// 单个上游的详情表单：基础字段、独立节点、Clash 控制面、内核路径。

import {useState} from "react";
import {open} from "@tauri-apps/plugin-dialog";
import {api, type AppConfig, type UpstreamServer} from "../../api";
import {useT} from "../../i18n";
import {Field, NumberInput, Toggle} from "../../components/fields";
import ManagedNodeEditor from "../ManagedNodeEditor";

const defaultClashController = {address: "127.0.0.1:9090", secret: ""};

export default function UpstreamForm({
                                       server,
                                       config,
                                       onServer,
                                       onConfig,
                                       nodeDraft,
                                       onNodeDraft,
                                     }: {
  server: UpstreamServer;
  config: AppConfig;
  onServer: (edit: (server: UpstreamServer) => void) => void;
  onConfig: (edit: (config: AppConfig) => void) => void;
  nodeDraft: string | undefined;
  onNodeDraft: (value?: string) => void;
}) {
  const t = useT();
  const managed = server.managed;

  return (
    <>
      <div className="form-grid">
        <Field label={t("settings.name")}>
          <input
            value={server.name}
            onChange={(event) => onServer((item) => (item.name = event.target.value))}
          />
        </Field>
        {!managed && (
          <Field label={t("settings.type")}>
            <select
              value={server.type}
              onChange={(event) => onServer((item) => (item.type = event.target.value))}
            >
              <option value="socks5">socks5</option>
              <option value="http">http</option>
            </select>
          </Field>
        )}
        <Field label={t("settings.host")}>
          <input
            value={server.host}
            spellCheck={false}
            autoComplete="off"
            placeholder={t("settings.hostHint")}
            onChange={(event) => onServer((item) => (item.host = event.target.value))}
          />
        </Field>
        <Field label={t("settings.port")}>
          <NumberInput
            min={1}
            max={65535}
            value={server.port}
            ariaLabel={t("settings.port")}
            apply={(port) => onServer((item) => (item.port = port))}
          />
        </Field>
        {!managed && (
          <>
            <Field label={t("settings.username")}>
              <input
                value={server.username}
                autoComplete="off"
                onChange={(event) => onServer((item) => (item.username = event.target.value))}
              />
            </Field>
            <Field label={t("settings.password")}>
              <input
                type="password"
                value={server.password}
                autoComplete="off"
                onChange={(event) => onServer((item) => (item.password = event.target.value))}
              />
            </Field>
          </>
        )}
      </div>

      {managed && (
        <>
          <ManagedNodeEditor
            node={managed}
            draft={nodeDraft}
            onNode={(node) => onServer((item) => (item.managed = node))}
            onDraft={(value) => onNodeDraft(value)}
          />
          <div className="form-grid">
            <Field label={t("managed.engine")} hint={t("managed.autoEngine")}>
              <input
                value={config.mihomo_path}
                placeholder={t("managed.autoEngine")}
                spellCheck={false}
                autoComplete="off"
                onChange={(event) => onConfig((c) => (c.mihomo_path = event.target.value))}
              />
            </Field>
          </div>
          <EngineActions
            path={config.mihomo_path}
            onPick={(path) => onConfig((c) => (c.mihomo_path = path))}
          />
        </>
      )}

      {!managed && (
        <>
          <Toggle
            label={t("clash.enable")}
            checked={server.clash !== undefined}
            onChange={(checked) =>
              onServer((item) => {
                if (checked) {
                  item.clash = {...defaultClashController};
                } else {
                  delete item.clash;
                }
              })
            }
          />
          {server.clash && (
            <>
              <div className="form-grid">
                <Field label={t("clash.address")}>
                  <input
                    value={server.clash.address}
                    spellCheck={false}
                    autoComplete="off"
                    onChange={(event) =>
                      onServer((item) => {
                        if (item.clash) item.clash = {...item.clash, address: event.target.value};
                      })
                    }
                  />
                </Field>
                <Field label={t("clash.secret")}>
                  <input
                    type="password"
                    value={server.clash.secret}
                    autoComplete="off"
                    onChange={(event) =>
                      onServer((item) => {
                        if (item.clash) item.clash = {...item.clash, secret: event.target.value};
                      })
                    }
                  />
                </Field>
              </div>
              <p className="muted">{t("clash.settingsHint")}</p>
            </>
          )}
        </>
      )}
    </>
  );
}

/// 内核路径的选择与校验（只在独立节点下出现）。
function EngineActions({path, onPick}: { path: string; onPick: (path: string) => void }) {
  const t = useT();
  const [result, setResult] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);

  return (
    <>
      <div className="row-actions">
        <button
          className="btn btn-small"
          onClick={async () => {
            try {
              const picked = await open({
                multiple: false,
                directory: false,
                title: t("managed.chooseEngine"),
              });
              if (typeof picked === "string") {
                onPick(picked);
                setResult(null);
              }
            } catch (error) {
              setResult(String(error));
            }
          }}
        >
          {t("managed.chooseEngine")}
        </button>
        <button
          className="btn btn-small"
          disabled={checking}
          onClick={async () => {
            setChecking(true);
            try {
              setResult(await api.findMihomo(path));
            } catch (error) {
              setResult(String(error));
            } finally {
              setChecking(false);
            }
          }}
        >
          {t("managed.checkEngine")}
        </button>
      </div>
      {result && <p className="muted" role="status">{result}</p>}
    </>
  );
}
