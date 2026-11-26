import type { ApiError } from "../gen/ApiError";
import type { CreateSessionRequest } from "../gen/CreateSessionRequest";
import type { Health } from "../gen/Health";
import type { SendMessageRequest } from "../gen/SendMessageRequest";
import type { Session } from "../gen/Session";
import type { SessionDetail } from "../gen/SessionDetail";

/** Mount point of every route in `crates/calin-proto/src/api.rs`. */
export const API_BASE = "/api/v1";

/**
 * A failed request. `status` is 0 when no response arrived. `fromServer` is
 * true only when the body was an `ApiError`, so `message` is the server's own.
 */
export class HttpError extends Error {
  readonly status: number;
  readonly fromServer: boolean;

  constructor(status: number, message: string, fromServer: boolean) {
    super(message);
    this.name = "HttpError";
    this.status = status;
    this.fromServer = fromServer;
  }

  /** A proxy or network failure rather than an answer from Calin. */
  get unreachable(): boolean {
    return !this.fromServer && (this.status === 0 || this.status >= 500);
  }
}

export const UNREACHABLE_MESSAGE = "Cannot reach the server";

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

export interface Api {
  health(): Promise<Health>;
  listSessions(): Promise<Session[]>;
  createSession(body: CreateSessionRequest): Promise<SessionDetail>;
  getSession(id: string): Promise<SessionDetail>;
  sendMessage(id: string, body: SendMessageRequest): Promise<void>;
  stopSession(id: string): Promise<void>;
}

function isApiError(value: unknown): value is ApiError {
  return (
    typeof value === "object" &&
    value !== null &&
    "error" in value &&
    typeof value.error === "string"
  );
}

async function failure(response: Response): Promise<HttpError> {
  try {
    const body: unknown = await response.json();
    if (isApiError(body)) return new HttpError(response.status, body.error, true);
  } catch {
    // Not JSON: the answer came from something in front of Calin.
  }
  const message = response.status >= 500 ? UNREACHABLE_MESSAGE : `HTTP ${response.status}`;
  return new HttpError(response.status, message, false);
}

export function createApi(
  fetchFn: FetchLike = (input, init) => fetch(input, init),
  base: string = API_BASE,
): Api {
  async function send(method: "GET" | "POST", path: string, body?: unknown): Promise<Response> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const init: RequestInit = { method, headers };
    if (body !== undefined) {
      headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    let response: Response;
    try {
      response = await fetchFn(base + path, init);
    } catch {
      throw new HttpError(0, UNREACHABLE_MESSAGE, false);
    }
    if (!response.ok) throw await failure(response);
    return response;
  }

  async function json<T>(method: "GET" | "POST", path: string, body?: unknown): Promise<T> {
    const response = await send(method, path, body);
    try {
      // The generated types are the contract; the server is trusted to honor it.
      return (await response.json()) as T;
    } catch {
      throw new HttpError(response.status, "Unexpected response from the server", false);
    }
  }

  async function empty(method: "POST", path: string, body?: unknown): Promise<void> {
    await send(method, path, body);
  }

  const session = (id: string) => `/sessions/${encodeURIComponent(id)}`;

  return {
    health: () => json<Health>("GET", "/health"),
    listSessions: () => json<Session[]>("GET", "/sessions"),
    createSession: (body) => json<SessionDetail>("POST", "/sessions", body),
    getSession: (id) => json<SessionDetail>("GET", session(id)),
    sendMessage: (id, body) => empty("POST", `${session(id)}/messages`, body),
    stopSession: (id) => empty("POST", `${session(id)}/stop`),
  };
}
