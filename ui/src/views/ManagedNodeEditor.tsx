import {type ManagedNode} from "../api";
import {useT} from "../i18n";

const protocolFields: Record<string, { label: string; fields: string[] }> = {
  vless: {label: "VLESS", fields: ["uuid", "flow"]},
  vmess: {label: "VMess", fields: ["uuid", "cipher"]},
  ss: {label: "Shadowsocks", fields: ["cipher", "password"]},
  ssr: {label: "ShadowsocksR", fields: ["cipher", "password", "protocol", "obfs"]},
  trojan: {label: "Trojan", fields: ["password", "sni"]},
  hysteria2: {label: "Hysteria2", fields: ["password", "sni", "obfs", "obfs-password"]},
  tuic: {label: "TUIC", fields: ["uuid", "password", "token", "sni"]},
  anytls: {label: "AnyTLS", fields: ["password", "sni"]},
  wireguard: {label: "WireGuard", fields: ["private-key", "public-key", "ip", "ipv6"]},
  snell: {label: "Snell", fields: ["psk"]},
  ssh: {label: "SSH", fields: ["username", "password"]},
  http: {label: "HTTP / HTTPS", fields: ["username", "password"]},
  socks5: {label: "SOCKS5 / TLS", fields: ["username", "password"]},
};

export function advancedNodeOptions(node: ManagedNode): Record<string, unknown> {
  const fields = protocolFields[node.protocol]?.fields ?? [];
  return Object.fromEntries(Object.entries(node.options).filter(([key]) => !fields.includes(key)));
}

export function applyAdvancedOptions(node: ManagedNode, draft: string): ManagedNode {
  const extra: unknown = JSON.parse(draft || "{}");
  if (!extra || typeof extra !== "object" || Array.isArray(extra)) {
    throw new Error("JSON object required");
  }
  const fields = protocolFields[node.protocol]?.fields ?? [];
  const basic = Object.fromEntries(Object.entries(node.options).filter(([key]) => fields.includes(key)));
  return {...node, options: {...basic, ...extra}};
}

export default function ManagedNodeEditor({node, draft, onNode, onDraft}: {
  node: ManagedNode;
  draft: string | undefined;
  onNode: (node: ManagedNode) => void;
  onDraft: (value?: string) => void;
}) {
  const t = useT();
  const fields = protocolFields[node.protocol]?.fields ?? [];

  function editField(field: string, value: string) {
    const options = {...node.options};
    if (value) {
      options[field] = value;
    } else {
      delete options[field];
    }
    onNode({...node, options});
  }

  return (
    <>
      <div className="form-grid">
        <label>
          {t("settings.type")}
          <select value={node.protocol} onChange={(event) => {
            onNode({protocol: event.target.value, options: {}});
            onDraft();
          }}>
            {Object.entries(protocolFields).map(([key, {label}]) =>
              <option key={key} value={key}>{label}</option>
            )}
          </select>
        </label>
        {fields.map((field) =>
          <label key={field}>
            {field.toUpperCase() === "UUID" ? "UUID" : field}
            <input
              type={["password", "obfs-password", "private-key", "psk", "token", "uuid"].includes(field) ? "password" : "text"}
              value={String(node.options[field] ?? "")}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => editField(field, event.target.value)}
            />
          </label>
        )}
      </div>
      <details>
        <summary>{t("managed.advanced")}</summary>
        <p className="muted">{t("managed.advancedHint")}</p>
        <textarea
          aria-label={t("managed.advanced")}
          className="node-options"
          rows={5}
          autoComplete="off"
          spellCheck={false}
          value={draft ?? JSON.stringify(advancedNodeOptions(node), null, 2)}
          onChange={(event) => onDraft(event.target.value)}
        />
      </details>
    </>
  );
}
