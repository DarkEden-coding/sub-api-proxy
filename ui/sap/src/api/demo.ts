// Demo backend: serves the sap-core sample and applies actions to a local copy.
import type {
  AccountHistory, AccountPatch, AccountView, Conflict, EventView, FlowSnapshot, ThreadView,
} from "./types.ts";
import type { Backend, StreamHandlers } from "./http.ts";

let state: Promise<FlowSnapshot> | null = null;
const listeners = new Set<(s: FlowSnapshot) => void>();

const load = () =>
  (state ??= import("../../../../crates/sap-core/tests/fixtures/flow.json").then(
    (m) => structuredClone(m.default) as unknown as FlowSnapshot,
  ));

async function change<T>(fn: (s: FlowSnapshot) => T): Promise<T> {
  const s = await load();
  const out = fn(s);
  for (const l of listeners) l(structuredClone(s));
  return out;
}

function conflictSample(s: FlowSnapshot): Conflict[] {
  const acct = s.accounts[2];
  const side = (node: string, wall: number, value: unknown) => ({ node, hlc: { wall_ms: wall, counter: 0, node }, value });
  return [{
    id: "demo-conflict-1", table: "accounts", key: acct.id, field: "priority",
    applied: side("macbook", 1_792_000_000_000, 2), other: side("power-server", 1_791_999_990_000, 0),
    detected_at: s.events.at(-1)?.at ?? s.now, resolved: false,
  }];
}

const find = (s: FlowSnapshot, id: string): AccountView => {
  const a = s.accounts.find((x) => x.id === id);
  if (!a) throw new Error(`no account ${id}`);
  return a;
};

export const demo: Backend = {
  flow: async () => structuredClone(await load()),
  accounts: async () => structuredClone((await load()).accounts),
  patchAccount: (id, patch: AccountPatch) =>
    change((s) => {
      const a = find(s, id);
      if (patch.label !== undefined) a.label = patch.label;
      if (patch.priority !== undefined) a.priority = patch.priority;
      if (patch.disabled !== undefined) a.state = patch.disabled ? "disabled" : a.active_threads ? "active" : "idle";
      if (patch.capacity_override !== undefined) {
        a.capacity_multiplier = patch.capacity_override ?? 1;
        a.capacity_source = patch.capacity_override === null ? "default" : "override";
      }
      return structuredClone(a);
    }),
  startLogin: async () => ({ url: "about:blank#demo-login", login_id: "demo-login" }),
  history: async (id, hours): Promise<AccountHistory> => {
    const s = await load();
    const a = find(s, id);
    const end = Date.parse(s.now);
    const n = hours * 4;
    const used = (kind: string) => a.windows.find((w) => w.kind === kind)?.used_percent ?? null;
    const five = used("five_hour"), week = used("weekly");
    return {
      account: id,
      samples: Array.from({ length: n }, (_, i) => {
        const f = (i + 1) / n;
        return {
          t: new Date(end - (n - 1 - i) * 900_000).toISOString(),
          five_hour_used_percent: five === null ? null : Math.max(0, five * f * (0.7 + 0.3 * Math.sin(i / 3) ** 2)),
          weekly_used_percent: week === null ? null : week * (0.6 + 0.4 * f),
        };
      }),
    };
  },
  threads: async () => structuredClone((await load()).threads),
  assignThread: (key, req) =>
    change((s): ThreadView => {
      const t = s.threads.find((x) => x.key === key);
      if (!t) throw new Error(`no thread ${key}`);
      t.account = req.account;
      t.reason = "set manually";
      return structuredClone(t);
    }),
  nodes: async () => structuredClone((await load()).nodes),
  conflicts: async () => conflictSample(await load()),
  resolveConflict: async (id, req) => {
    const c = conflictSample(await load())[0];
    return { ...c, id, resolved: true, applied: { ...c.applied, node: req.keep } };
  },
  events: async (limit): Promise<EventView[]> => structuredClone((await load()).events.slice(0, limit)),
};

export function demoStream(h: StreamHandlers): () => void {
  const push = (s: FlowSnapshot) => h.onSnapshot(s);
  listeners.add(push);
  void load().then((s) => {
    h.onOpen?.();
    push(structuredClone(s));
  });
  return () => listeners.delete(push);
}
