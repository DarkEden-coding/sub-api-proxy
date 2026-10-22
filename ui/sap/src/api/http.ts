import type {
  AccountHistory, AccountPatch, AccountView, ApiError, AssignRequest, Conflict, EventView,
  FlowSnapshot, LoginRequest, LoginStart, NodeView, ResolveRequest, ThreadView,
} from "./types.ts";
import { parseSse } from "../model/sse.ts";

const KEY_STORAGE = "sap-management-key";
const API = "/sap/api";

export const managementKey = () => localStorage.getItem(KEY_STORAGE) ?? "";
export function setManagementKey(key: string) {
  if (key) localStorage.setItem(KEY_STORAGE, key);
  else localStorage.removeItem(KEY_STORAGE);
}

/** A failed request. `status` 0 means the server could not be reached. */
export class RequestError extends Error {
  constructor(readonly status: number, message: string) {
    super(message);
  }
}

function headers(json: boolean): Headers {
  const h = new Headers();
  const key = managementKey();
  if (key) h.set("Authorization", `Bearer ${key}`);
  if (json) h.set("Content-Type", "application/json");
  return h;
}

async function failure(res: Response): Promise<RequestError> {
  const body = (await res.json().catch(() => null)) as Partial<ApiError> | null;
  return new RequestError(res.status, body?.message ?? `${res.status} ${res.statusText}`);
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch(API + path, {
      method,
      headers: headers(body !== undefined),
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch (e) {
    throw new RequestError(0, e instanceof Error ? e.message : "unreachable");
  }
  if (!res.ok) throw await failure(res);
  return res.json() as Promise<T>;
}

export interface StreamHandlers {
  onSnapshot(s: FlowSnapshot): void;
  onEvent?(e: EventView): void;
  /** Called with the failure each time the stream drops, before it reconnects. */
  onError?(e: RequestError): void;
  onOpen?(): void;
}

/**
 * `GET /sap/api/stream` read with fetch, because EventSource cannot send the bearer header.
 * Reconnects with backoff until the returned function is called.
 */
export function openStream(h: StreamHandlers): () => void {
  const abort = new AbortController();
  void (async () => {
    let delay = 1000;
    while (!abort.signal.aborted) {
      try {
        const res = await fetch(`${API}/stream`, { headers: headers(false), signal: abort.signal });
        if (!res.ok || !res.body) throw await failure(res);
        h.onOpen?.();
        delay = 1000;
        const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
        let buffer = "";
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          const parsed = parseSse(buffer + value);
          buffer = parsed.rest;
          for (const m of parsed.messages) {
            if (m.event === "snapshot") h.onSnapshot(JSON.parse(m.data));
            else if (m.event === "event") h.onEvent?.(JSON.parse(m.data));
          }
        }
        throw new RequestError(0, "stream closed");
      } catch (e) {
        if (abort.signal.aborted) return;
        h.onError?.(e instanceof RequestError ? e : new RequestError(0, (e as Error).message));
        if (e instanceof RequestError && (e.status === 401 || e.status === 403)) return;
        await new Promise((r) => setTimeout(r, delay));
        delay = Math.min(delay * 2, 15_000);
      }
    }
  })();
  return () => abort.abort();
}

const enc = encodeURIComponent;

export const http = {
  flow: () => request<FlowSnapshot>("GET", "/flow"),
  accounts: () => request<AccountView[]>("GET", "/accounts"),
  refreshAccounts: () => request<{ accounts: { last_poll_error: string | null }[] }>("POST", "/accounts/refresh"),
  patchAccount: (id: string, patch: AccountPatch) => request<AccountView>("PATCH", `/accounts/${enc(id)}`, patch),
  startLogin: (req: LoginRequest) => request<LoginStart>("POST", "/accounts/login", req),
  history: (id: string, hours: number) => request<AccountHistory>("GET", `/accounts/${enc(id)}/history?hours=${hours}`),
  threads: (activeOnly: boolean) => request<ThreadView[]>("GET", `/threads?active=${activeOnly}`),
  assignThread: (key: string, req: AssignRequest) => request<ThreadView>("POST", `/threads/${enc(key)}/assign`, req),
  nodes: () => request<NodeView[]>("GET", "/nodes"),
  conflicts: () => request<Conflict[]>("GET", "/conflicts"),
  resolveConflict: (id: string, req: ResolveRequest) => request<Conflict>("POST", `/conflicts/${enc(id)}/resolve`, req),
  events: (limit: number) => request<EventView[]>("GET", `/events?limit=${limit}`),
};

export type Backend = typeof http;
