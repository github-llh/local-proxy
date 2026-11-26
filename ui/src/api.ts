// Tauri 命令层封装：前端唯一的数据入口。

import {invoke} from "@tauri-apps/api/core";
import {open as openDialog} from "@tauri-apps/plugin-dialog";

export interface CoreStatus {
  running: boolean;
  pid: number | null;
  uptime_secs: number;
  listen_host: string;
  socks5_port: number;
  http_port: number;
  active_upstream: string;
  active_upstream_id: string;
  upstream_type: string;
  upstreams: UpstreamSummary[];
}

export interface UpstreamSummary {
  id: string;
  name: string;
  endpoint: string;
  server_type: string;
  clash_enabled: boolean;
}

export interface ClashController {
  address: string;
  secret: string;
}

export interface ClashState {
  mode: string;
  groups: { name: string; selected: string; choices: string[] }[];
}

export interface AppEntry {
  id: string;
  name: string;
  path: string;
  enabled: boolean;
  extra_args: string[];
  note: string;
  last_launched_at: number | null;
}

export interface SessionInfo {
  entry_id: string;
  entry_name: string;
  pid: number;
  uptime_secs: number;
}

export interface UpstreamServer {
  id: string;
  name: string;
  type: string;
  host: string;
  port: number;
  username: string;
  password: string;
  clash?: ClashController;
}

export interface RoutingRule {
  name: string;
  enabled: boolean;
  action: string;
  priority: number;
  ip_cidrs_v4: string[];
  ip_cidrs_v6: string[];
  domains: string[];
  ports: string[];
  protocols: string[];
}

export interface ExitIpInfo {
  ip: string;
  country_code: string;
  country: string;
  city: string;
  isp: string;
}

export interface ConnectionRecord {
  ts_ms: number;
  protocol: string;
  host: string;
  port: number;
  action: string;
  rule: string;
  result: string;
  error: string;
}

export interface AppConfig {
  listen_host: string;
  socks5_port: number;
  http_port: number;
  control_port: number;
  control_token: string;
  log_level: string;
  upstream: { active: string; servers: UpstreamServer[] };
  routing: {
    enabled: boolean;
    priority_mode: string;
    default_action: string;
    use_default_private: boolean;
    rules: RoutingRule[];
  };
  dns_mode: string;
  proxy_rules: {
    allowed_ports: number[];
    dns_mode: string;
    ipv6_mode: string;
    udp_mode: string;
    udp_fallback: string;
    routing: unknown;
  };
  apps: AppEntry[];
  ui: {
    language: string;
    launch_at_login: boolean;
    keep_core_alive: boolean;
    exit_ip_probe: boolean;
  };
  probe: {
    endpoints: string[];
    timeout_ms: number;
  };
}

export const api = {
  getStatus: () => invoke<CoreStatus>("get_status"),
  startCore: () => invoke<number>("start_core"),
  stopCore: () => invoke<void>("stop_core"),
  listApps: () => invoke<AppEntry[]>("list_apps"),
  addApp: (path: string) => invoke<AppEntry>("add_app", {path}),
  removeApp: (id: string) => invoke<void>("remove_app", {id}),
  launchApp: (id: string) => invoke<SessionInfo>("launch_app", {id}),
  stopApp: (id: string) => invoke<void>("stop_app", {id}),
  listSessions: () => invoke<SessionInfo[]>("list_sessions"),
  setAppEnabled: (id: string, enabled: boolean) =>
    invoke<void>("set_app_enabled", {id, enabled}),
  testUpstream: (id: string, draft?: UpstreamServer) => invoke<number>("test_upstream", {id, draft}),
  probeExitIp: () => invoke<ExitIpInfo>("probe_exit_ip"),
  listRecentConnections: (limit?: number) =>
    invoke<ConnectionRecord[]>("list_recent_connections", {limit}),
  getConfig: () => invoke<AppConfig>("get_config"),
  saveConfig: (config: AppConfig, previousActive: string) =>
    invoke<AppConfig>("save_config", {config, previousActive}),
  selectUpstream: (id: string) => invoke<void>("select_upstream", {id}),
  getClashState: (upstreamId: string) => invoke<ClashState>("get_clash_state", {upstreamId}),
  selectClashProxy: (upstreamId: string, group: string, choice: string) =>
    invoke<void>("select_clash_proxy", {upstreamId, group, choice}),
};

// 选择应用文件（.app / .exe / 可执行文件）。
export async function pickAppFile(): Promise<string | null> {
  const result = await openDialog({
    multiple: false,
    directory: false,
    title: "选择要代理的应用",
  });
  return typeof result === "string" ? result : null;
}
