// DNS 与 UDP：解析模式、UDP 模式与降级、IPv6 策略、允许代理的端口。

import type {AppConfig} from "../../api";
import {useT} from "../../i18n";
import {CommaTextField, Field} from "../../components/fields";

export default function DnsUdpCard({
                                     config,
                                     patch,
                                   }: {
  config: AppConfig;
  patch: (edit: (config: AppConfig) => void) => void;
}) {
  const t = useT();
  return (
    <section className="card">
      <h2>{t("settings.dnsUdp")}</h2>
      <div className="form-grid">
        <Field label={t("settings.dnsMode")} hint={t("settings.dnsHint")}>
          <select
            value={config.dns_mode}
            onChange={(event) => patch((c) => (c.dns_mode = event.target.value))}
          >
            <option value="proxy">{t("settings.dnsProxy")}</option>
            <option value="direct">{t("settings.dnsDirect")}</option>
          </select>
        </Field>
        <Field label={t("settings.udpMode")} hint={t("settings.udpHint")}>
          <select
            value={config.proxy_rules.udp_mode}
            onChange={(event) => patch((c) => (c.proxy_rules.udp_mode = event.target.value))}
          >
            <option value="auto">auto</option>
            <option value="proxy">proxy</option>
            <option value="direct">direct</option>
            <option value="block">block</option>
          </select>
        </Field>
        <Field label={t("settings.udpFallback")}>
          <select
            value={config.proxy_rules.udp_fallback}
            onChange={(event) => patch((c) => (c.proxy_rules.udp_fallback = event.target.value))}
          >
            <option value="block">{t("settings.udpFallbackBlock")}</option>
            <option value="direct">{t("settings.udpFallbackDirect")}</option>
          </select>
        </Field>
        <Field label={t("settings.ipv6")}>
          <select
            value={config.proxy_rules.ipv6_mode}
            onChange={(event) => patch((c) => (c.proxy_rules.ipv6_mode = event.target.value))}
          >
            <option value="proxy">proxy</option>
            <option value="direct">direct</option>
            <option value="block">block</option>
          </select>
        </Field>
      </div>
      <CommaTextField
        label={t("settings.allowedPorts")}
        hint={t("settings.allowedPortsHint")}
        placeholder="80, 443"
        value={config.proxy_rules.allowed_ports.join(", ")}
        apply={(text) => {
          const ports = text
            .split(",")
            .map((part) => Number(part.trim()))
            .filter((port) => Number.isInteger(port) && port > 0 && port <= 65535);
          // 全是无效输入时保留原值：空白名单等于放行所有端口，不能靠误输入触发
          if (ports.length === 0 && text.trim() !== "") return;
          patch((c) => (c.proxy_rules.allowed_ports = ports));
        }}
      />
    </section>
  );
}
