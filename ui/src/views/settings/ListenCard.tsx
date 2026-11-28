// 监听与日志：地址、SOCKS5 / HTTP CONNECT / 控制面端口、日志级别。

import type {AppConfig} from "../../api";
import {useT} from "../../i18n";
import {Field, NumberInput} from "../../components/fields";

export default function ListenCard({
                                     config,
                                     patch,
                                   }: {
  config: AppConfig;
  patch: (edit: (config: AppConfig) => void) => void;
}) {
  const t = useT();
  const port = (key: "socks5_port" | "http_port" | "control_port", label: string) => (
    <Field label={label}>
      <NumberInput
        min={1}
        max={65535}
        value={config[key]}
        ariaLabel={label}
        apply={(value) =>
          patch((c) => {
            c[key] = value;
          })
        }
      />
    </Field>
  );
  return (
    <section className="card">
      <h2>{t("settings.listen")}</h2>
      <div className="form-grid">
        <Field label={t("settings.listenHost")} hint={t("settings.listenHostHint")}>
          <input
            value={config.listen_host}
            spellCheck={false}
            autoComplete="off"
            onChange={(event) => patch((c) => (c.listen_host = event.target.value))}
          />
        </Field>
        {port("socks5_port", t("settings.socks5Port"))}
        {port("http_port", t("settings.httpPort"))}
        {port("control_port", t("settings.controlPort"))}
        <Field label={t("settings.logLevel")}>
          <select
            value={config.log_level}
            onChange={(event) => patch((c) => (c.log_level = event.target.value))}
          >
            <option value="error">error</option>
            <option value="warn">warn</option>
            <option value="info">info</option>
            <option value="debug">debug</option>
          </select>
        </Field>
      </div>
      <p className="muted">{t("settings.controlPortHint")}</p>
    </section>
  );
}
