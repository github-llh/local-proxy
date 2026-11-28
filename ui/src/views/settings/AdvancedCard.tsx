// 高级：开机自启、界面语言、出口 IP 探测端点与超时。

import type {AppConfig} from "../../api";
import {languages, useT} from "../../i18n";
import {Field, LinesField, NumberInput, Toggle} from "../../components/fields";

export default function AdvancedCard({
                                       config,
                                       patch,
                                     }: {
  config: AppConfig;
  patch: (edit: (config: AppConfig) => void) => void;
}) {
  const t = useT();
  return (
    <section className="card">
      <h2>{t("settings.advanced")}</h2>
      <div className="form-grid">
        <Field label={t("settings.language")}>
          <select
            value={config.ui.language}
            onChange={(event) => patch((c) => (c.ui.language = event.target.value))}
          >
            {languages.map((language) => (
              <option key={language.value} value={language.value}>
                {language.label}
              </option>
            ))}
          </select>
        </Field>
      </div>
      <Toggle
        label={t("settings.launchAtLogin")}
        checked={config.ui.launch_at_login}
        onChange={(checked) => patch((c) => (c.ui.launch_at_login = checked))}
      />
      <Toggle
        label={t("settings.exitIpProbe")}
        checked={config.ui.exit_ip_probe}
        onChange={(checked) => patch((c) => (c.ui.exit_ip_probe = checked))}
      />
      <div className="form-grid">
        <LinesField
          label={t("settings.probeEndpointsHint")}
          rows={2}
          value={config.probe.endpoints.join("\n")}
          apply={(text) =>
            patch((c) => {
              c.probe.endpoints = text
                .split("\n")
                .map((line) => line.trim())
                .filter(Boolean);
            })
          }
        />
        <Field label={t("settings.probeTimeout")}>
          <NumberInput
            min={100}
            max={60000}
            step={500}
            value={config.probe.timeout_ms}
            ariaLabel={t("settings.probeTimeout")}
            apply={(timeout) => patch((c) => (c.probe.timeout_ms = timeout))}
          />
        </Field>
      </div>
      <p className="muted">{t("settings.probeHint")}</p>
    </section>
  );
}
