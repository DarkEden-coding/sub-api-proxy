// The dashboard's data client. A real server by default; the sap-core sample in demo mode
// (`?demo=1`, or the API being unreachable under `vite dev`).
import type {
  AccountPatch, AssignRequest, FlowSnapshot, LoginRequest, ResolveRequest,
} from "./types.ts";
import { demo, demoStream } from "./demo.ts";
import { http, openStream, type Backend, type StreamHandlers } from "./http.ts";

export { managementKey, RequestError, setManagementKey } from "./http.ts";

let demoMode = new URLSearchParams(location.search).get("demo") === "1";
let backend: Backend = demoMode ? demo : http;

export const isDemo = () => demoMode;

export function useDemo() {
  demoMode = true;
  backend = demo;
}

export const fetchFlow = (): Promise<FlowSnapshot> => backend.flow();

/** Live snapshots over SSE `GET /sap/api/stream`; returns the unsubscribe function. */
export const subscribe = (handlers: StreamHandlers): (() => void) =>
  demoMode ? demoStream(handlers) : openStream(handlers);

export const fetchAccounts = () => backend.accounts();
export const refreshAccounts = () => backend.refreshAccounts();
export const patchAccount = (id: string, patch: AccountPatch) => backend.patchAccount(id, patch);
export const startLogin = (req: LoginRequest) => backend.startLogin(req);
export const fetchHistory = (id: string, hours = 24) => backend.history(id, hours);
export const fetchThreads = (activeOnly = true) => backend.threads(activeOnly);
export const assignThread = (key: string, req: AssignRequest) => backend.assignThread(key, req);
export const fetchNodes = () => backend.nodes();
export const fetchConflicts = () => backend.conflicts();
export const resolveConflict = (id: string, req: ResolveRequest) => backend.resolveConflict(id, req);
export const fetchEvents = (limit = 100) => backend.events(limit);
export const fetchUsage = (days = 30) => backend.usage(days);
