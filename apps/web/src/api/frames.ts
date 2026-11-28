import { API_BASE } from "./http";

/** The address of a socket at `path` under the API, on the page's host: `wss:` when the page is `https:`. */
export function socketUrl(location: Pick<Location, "protocol" | "host">, path: string): string {
  const scheme = location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${location.host}${API_BASE}${path}`;
}

/** A JSON text frame with a string `op`, or undefined for anything else. */
export function parseOpFrame<T extends { op: string }>(data: string): T | undefined {
  let value: unknown;
  try {
    value = JSON.parse(data);
  } catch {
    return undefined;
  }
  if (typeof value !== "object" || value === null || !("op" in value)) return undefined;
  if (typeof value.op !== "string") return undefined;
  // Shapes beyond `op` are the contract's business; unknown ops are dropped by the caller.
  return value as T;
}
