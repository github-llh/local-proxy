// 路由规则：默认动作、优先级模式、内网直连、规则表（全字段可编辑）。

import type {AppConfig, RoutingRule} from "../../api";
import {useT} from "../../i18n";
import {Field, ListInput, NumberInput, Toggle} from "../../components/fields";

const emptyRule = (): RoutingRule => ({
  id: crypto.randomUUID(),
  name: "",
  enabled: true,
  action: "proxy",
  priority: 0,
  ip_cidrs_v4: [],
  ip_cidrs_v6: [],
  domains: [],
  ports: [],
  protocols: ["tcp"],
});

function splitCommas(text: string): string[] {
  return text
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean);
}

export default function RoutingCard({
                                      config,
                                      patch,
                                    }: {
  config: AppConfig;
  patch: (edit: (config: AppConfig) => void) => void;
}) {
  const t = useT();
  const rules = config.routing.rules;
  const byPriority = config.routing.priority_mode === "number";

  const updateRule = (id: string, edit: (rule: RoutingRule) => void) =>
    patch((c) => {
      const rule = c.routing.rules.find((item) => item.id === id);
      if (rule) edit(rule);
    });

  const move = (index: number, delta: number) =>
    patch((c) => {
      const target = index + delta;
      if (target < 0 || target >= c.routing.rules.length) return;
      const [rule] = c.routing.rules.splice(index, 1);
      c.routing.rules.splice(target, 0, rule);
    });

  return (
    <section className="card">
      <h2>{t("settings.routing")}</h2>
      <div className="form-grid">
        <Field label={t("settings.defaultAction")}>
          <select
            value={config.routing.default_action}
            onChange={(event) => patch((c) => (c.routing.default_action = event.target.value))}
          >
            <option value="proxy">{t("settings.defaultActionProxy")}</option>
            <option value="direct">{t("settings.defaultActionDirect")}</option>
          </select>
        </Field>
        <Field label={t("settings.priorityMode")} hint={t("settings.priorityHint")}>
          <select
            value={config.routing.priority_mode}
            onChange={(event) => patch((c) => (c.routing.priority_mode = event.target.value))}
          >
            <option value="order">{t("settings.priorityOrder")}</option>
            <option value="number">{t("settings.priorityNumber")}</option>
          </select>
        </Field>
      </div>
      <Toggle
        label={t("settings.autoPrivate")}
        checked={config.routing.use_default_private}
        onChange={(checked) => patch((c) => (c.routing.use_default_private = checked))}
      />
      <Toggle
        label={t("settings.routingEnabled")}
        hint={t("settings.routingEnabledHint")}
        checked={config.routing.enabled}
        onChange={(checked) => patch((c) => (c.routing.enabled = checked))}
      />

      {rules.length === 0 ? (
        <p className="muted">{t("settings.noRules")}</p>
      ) : (
        <div className="rules-scroll">
          <table className="rules-table">
            <thead>
            <tr>
              <th className="col-enabled">{t("settings.enabled")}</th>
              <th className="col-name">{t("settings.ruleName")}</th>
              <th className="col-action">{t("settings.action")}</th>
              <th>{t("settings.domains")}</th>
              <th>{t("settings.cidrV4")}</th>
              <th>{t("settings.cidrV6")}</th>
              <th className="col-port">{t("settings.ports")}</th>
              <th className="col-protocol">{t("settings.protocol")}</th>
              <th className="col-priority">{t("settings.priority")}</th>
              <th className="col-actions"></th>
            </tr>
            </thead>
            <tbody>
            {rules.map((rule, index) => (
              <tr key={rule.id} className={rule.enabled ? "" : "dimmed"}>
                <td>
                  <input
                    type="checkbox"
                    checked={rule.enabled}
                    aria-label={t("settings.enabled")}
                    onChange={(event) =>
                      updateRule(rule.id, (item) => (item.enabled = event.target.checked))
                    }
                  />
                </td>
                <td>
                  <input
                    value={rule.name}
                    aria-label={t("settings.ruleName")}
                    onChange={(event) =>
                      updateRule(rule.id, (item) => (item.name = event.target.value))
                    }
                  />
                </td>
                <td>
                  <select
                    value={rule.action}
                    aria-label={t("settings.action")}
                    onChange={(event) =>
                      updateRule(rule.id, (item) => (item.action = event.target.value))
                    }
                  >
                    <option value="proxy">proxy</option>
                    <option value="direct">direct</option>
                  </select>
                </td>
                <td>
                  <ListInput
                    value={rule.domains.join(", ")}
                    placeholder=".cn, *.example.com"
                    ariaLabel={t("settings.domains")}
                    apply={(text) =>
                      updateRule(rule.id, (item) => (item.domains = splitCommas(text)))
                    }
                  />
                </td>
                <td>
                  <ListInput
                    value={rule.ip_cidrs_v4.join(", ")}
                    placeholder="10.0.0.0/8"
                    ariaLabel={t("settings.cidrV4")}
                    apply={(text) =>
                      updateRule(rule.id, (item) => (item.ip_cidrs_v4 = splitCommas(text)))
                    }
                  />
                </td>
                <td>
                  <ListInput
                    value={rule.ip_cidrs_v6.join(", ")}
                    placeholder="fc00::/7"
                    ariaLabel={t("settings.cidrV6")}
                    apply={(text) =>
                      updateRule(rule.id, (item) => (item.ip_cidrs_v6 = splitCommas(text)))
                    }
                  />
                </td>
                <td>
                  <ListInput
                    value={rule.ports.join(", ")}
                    placeholder="80, 443, 10000-20000"
                    ariaLabel={t("settings.ports")}
                    apply={(text) =>
                      updateRule(rule.id, (item) => (item.ports = splitCommas(text)))
                    }
                  />
                </td>
                <td>
                  <select
                    value={rule.protocols.length === 1 ? rule.protocols[0] : "both"}
                    aria-label={t("settings.protocol")}
                    onChange={(event) =>
                      updateRule(rule.id, (item) => {
                        item.protocols =
                          event.target.value === "both" ? [] : [event.target.value];
                      })
                    }
                  >
                    <option value="both">{t("settings.protocolBoth")}</option>
                    <option value="tcp">TCP</option>
                    <option value="udp">UDP</option>
                  </select>
                </td>
                <td>
                  <NumberInput
                    className="priority-input"
                    value={rule.priority}
                    disabled={!byPriority}
                    ariaLabel={t("settings.priority")}
                    apply={(priority) =>
                      updateRule(rule.id, (item) => (item.priority = priority))
                    }
                  />
                </td>
                <td>
                  <div className="row-buttons">
                    <button
                      className="btn btn-small btn-ghost"
                      aria-label={t("settings.moveUp")}
                      title={t("settings.moveUp")}
                      disabled={byPriority || index === 0}
                      onClick={() => move(index, -1)}
                    >
                      ↑
                    </button>
                    <button
                      className="btn btn-small btn-ghost"
                      aria-label={t("settings.moveDown")}
                      title={t("settings.moveDown")}
                      disabled={byPriority || index === rules.length - 1}
                      onClick={() => move(index, 1)}
                    >
                      ↓
                    </button>
                    <button
                      className="btn btn-small btn-ghost btn-danger-text"
                      aria-label={t("settings.remove")}
                      title={t("settings.remove")}
                      onClick={() =>
                        patch((c) => {
                          c.routing.rules = c.routing.rules.filter(
                            (item) => item.id !== rule.id
                          );
                        })
                      }
                    >
                      ×
                    </button>
                  </div>
                </td>
              </tr>
            ))}
            </tbody>
          </table>
        </div>
      )}
      <div className="row-actions">
        <button
          className="btn btn-small"
          onClick={() => patch((c) => c.routing.rules.push(emptyRule()))}
        >
          {t("settings.addRule")}
        </button>
      </div>
    </section>
  );
}
