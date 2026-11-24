// 出口地与系统时区/语言的一致性校验（风控自检）。
//
// 依据公开风控规则（Timezone Mismatch 是 Certta/Verisoul 等平台的明确信号）：
// 出口 IP 国家与设备时区/语言严重不一致时，代理身份易被识别。

/// 常见代理地区时区 → 预期国家码（粗映射）。
const TZ_TO_CC: Record<string, string> = {
  "Asia/Shanghai": "CN",
  "Asia/Chongqing": "CN",
  "Asia/Harbin": "CN",
  "Asia/Urumqi": "CN",
  "Asia/Tokyo": "JP",
  "Asia/Seoul": "KR",
  "Asia/Singapore": "SG",
  "Asia/Kuala_Lumpur": "MY",
  "Asia/Bangkok": "TH",
  "Asia/Jakarta": "ID",
  "Asia/Manila": "PH",
  "Asia/Ho_Chi_Minh": "VN",
  "Asia/Hong_Kong": "HK",
  "Asia/Taipei": "TW",
  "Asia/Macau": "MO",
  "America/Los_Angeles": "US",
  "America/New_York": "US",
  "America/Chicago": "US",
  "America/Denver": "US",
  "America/Phoenix": "US",
  "America/Anchorage": "US",
  "America/Toronto": "CA",
  "America/Vancouver": "CA",
  "Europe/London": "GB",
  "Europe/Dublin": "IE",
  "Europe/Berlin": "DE",
  "Europe/Paris": "FR",
  "Europe/Amsterdam": "NL",
  "Europe/Madrid": "ES",
  "Europe/Rome": "IT",
  "Europe/Moscow": "RU",
  "Australia/Sydney": "AU",
  "Australia/Melbourne": "AU",
  "Pacific/Auckland": "NZ",
  "America/Sao_Paulo": "BR",
  "America/Mexico_City": "MX",
  "Asia/Kolkata": "IN",
  "Asia/Dubai": "AE",
  "Asia/Jerusalem": "IL",
  "Europe/Istanbul": "TR",
};

/// 系统语言 → 预期国家码（"zh-CN" → CN；无地区时返回 null）。
export function languageToCountryCode(language: string): string | null {
  const match = language.match(/^([a-z]{2,3})(?:-([A-Z]{2}))?$/i);
  if (!match) return null;
  return match[2] ? match[2].toUpperCase() : null;
}

/// 系统时区 → 预期国家码。
export function timezoneToCountryCode(timeZone: string): string | null {
  return TZ_TO_CC[timeZone] ?? null;
}

export interface ConsistencyResult {
  level: "ok" | "warn" | "unknown";
  /// 详细说明（前端 i18n key 之外的动态部分）
  detail: string;
}

/// 一致性校验。
///
/// - 出口国家与「时区推断」或「语言推断」任一匹配 → ok
/// - 都不匹配 → warn（时区/语言与出口地不一致）
/// - 出口国家未知 → unknown
export function checkConsistency(
  exitCountryCode: string,
  timeZone: string,
  language: string
): ConsistencyResult {
  if (!exitCountryCode) {
    return { level: "unknown", detail: "" };
  }
  const fromTz = timezoneToCountryCode(timeZone);
  const fromLang = languageToCountryCode(language);
  const matched =
    (fromTz !== null && fromTz === exitCountryCode) ||
    (fromLang !== null && fromLang === exitCountryCode);
  if (matched) {
    return { level: "ok", detail: "" };
  }
  const hints = [fromTz, fromLang].filter(Boolean).join("/");
  return {
    level: "warn",
    detail: hints ? `(${hints} vs ${exitCountryCode})` : `(${exitCountryCode})`,
  };
}

/// 读取系统时区与语言。
export function systemContext(): { timeZone: string; language: string } {
  let timeZone = "";
  try {
    timeZone = Intl.DateTimeFormat().resolvedOptions().timeZone ?? "";
  } catch {
    timeZone = "";
  }
  const language = navigator.language || "en";
  return { timeZone, language };
}
