import { AUTH_LOGIN_PATH } from "../api/http";

/**
 * Where the Sign in button sends the browser. `next` is the in-app location
 * (path and query) the server returns to afterwards; anything that could read
 * as another origin (`//host`, `/\host`) becomes `/` (remote-access spec, section 2).
 */
export function signInUrl(location: Pick<Location, "pathname" | "search">): string {
  const path = location.pathname;
  const safe = path.startsWith("/") && !path.startsWith("//") && !path.startsWith("/\\");
  const next = safe ? path + location.search : "/";
  return `${AUTH_LOGIN_PATH}?next=${encodeURIComponent(next)}`;
}

export const TOKEN_NAME_MAX = 80;

/** Why a device token name cannot be sent, or `undefined` when it can (auth.rs: 1 to 80 characters). */
export function tokenNameProblem(name: string): string | undefined {
  // Characters, not UTF-16 units: an emoji counts once.
  const length = Array.from(name.trim()).length;
  if (length === 0) return "Give the token a name.";
  if (length > TOKEN_NAME_MAX) return `Keep the name to ${TOKEN_NAME_MAX} characters.`;
  return undefined;
}

const RAW_UA_MAX = 60;

// Order matters: Edge and Opera also say Chrome, Chrome also says Safari.
const BROWSERS: ReadonlyArray<readonly [RegExp, string]> = [
  [/\bEdg(?:e|A|iOS)?\//, "Edge"],
  [/\b(?:OPR|Opera)\//, "Opera"],
  [/\bSamsungBrowser\//, "Samsung Internet"],
  [/\b(?:Firefox|FxiOS)\//, "Firefox"],
  [/\b(?:HeadlessChrome|Chrome|CriOS|Chromium)\//, "Chrome"],
  [/\bVersion\/[\d.]+.*\bSafari\//, "Safari"],
];

const SYSTEMS: ReadonlyArray<readonly [RegExp, string]> = [
  [/\biPad\b/, "iPadOS"],
  [/\b(?:iPhone|iPod)\b/, "iOS"],
  [/\bAndroid\b/, "Android"],
  [/\bCrOS\b/, "ChromeOS"],
  [/\bWindows\b/, "Windows"],
  [/\bMac OS X\b|\bMacintosh\b/, "macOS"],
  [/\bLinux\b/, "Linux"],
];

const firstMatch = (ua: string, table: ReadonlyArray<readonly [RegExp, string]>) =>
  table.find(([pattern]) => pattern.test(ua))?.[1];

/**
 * "Firefox on Linux", "Safari on iOS"; the raw string, cut short, when the
 * browser is not one of the common ones.
 */
export function describeUserAgent(ua: string | null): string {
  const raw = (ua ?? "").trim();
  if (raw === "") return "Unknown browser";
  const browser = firstMatch(raw, BROWSERS);
  const system = firstMatch(raw, SYSTEMS);
  if (browser && system) return `${browser} on ${system}`;
  if (browser) return browser;
  return raw.length > RAW_UA_MAX ? `${raw.slice(0, RAW_UA_MAX - 1)}…` : raw;
}
