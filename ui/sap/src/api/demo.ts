// Demo backend: serves the sap-core sample and applies actions to a local copy.
import type {
  AccountHistory, AccountPatch, AccountView, Conflict, EventView, FlowSnapshot, ThreadView, UsageHistory,
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
  refreshAccounts: async () => ({ accounts: [] }),
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
  usage: async (days): Promise<UsageHistory> => usageSample(days, (await load()).now),
};

/** A small, plausible history so the Usage tab can be seen in `?demo=1`. */
function usageSample(days: number, now: string): UsageHistory {
  const sum = (requests: number, input: number, read: number, write: number, output: number, rate: [number, number, number, number]) => {
    const per = (n: number, r: number) => (n * r) / 1e6;
    const byType = { input: per(input, rate[0]), cache_read: per(read, rate[2]), cache_write: per(write, rate[3]), output: per(output, rate[1]) };
    const cost = byType.input + byType.cache_read + byType.cache_write + byType.output;
    const without = per(input + read + write, rate[0]) + per(output, rate[1]);
    return {
      requests, failed: 0, input_tokens: input, output_tokens: output, cache_read_tokens: read, cache_write_tokens: write,
      cost_usd: cost, cost_by_type_usd: byType, cost_without_cache_usd: without, savings_usd: without - cost, unpriced_requests: 0,
    };
  };
  const sonnet: [number, number, number, number] = [2, 10, 0.2, 2.5];
  const end = Date.parse(now);
  const span = Math.min(days, 21);
  const dayList = Array.from({ length: span }, (_, i) => {
    const t = new Date(end - (span - 1 - i) * 86_400_000);
    const k = 0.5 + Math.abs(Math.sin(i * 1.7)) * (i % 7 === 5 ? 0.15 : 1);
    return { date: t.toISOString().slice(0, 10), ...sum(Math.round(60 * k), Math.round(40_000 * k), Math.round(7_000_000 * k), Math.round(350_000 * k), Math.round(120_000 * k), sonnet) };
  }).filter((_, i) => i % 9 !== 4);
  const total = dayList.reduce((a, d) => sum(a.requests + d.requests, a.input_tokens + d.input_tokens, a.cache_read_tokens + d.cache_read_tokens, a.cache_write_tokens + d.cache_write_tokens, a.output_tokens + d.output_tokens, sonnet), sum(0, 0, 0, 0, 0, sonnet));
  return {
    prices: { source: "built-in", updated_at: null },
    statistics_enabled: true,
    totals: total,
    days: dayList,
    models: [
      { model: "claude-sonnet-5-5", provider: "claude", priced: true, ...total },
      { model: "claude-haiku-5-5", provider: "claude", priced: true, ...sum(130, 22_000, 1_400_000, 90_000, 41_000, [0.1, 0.5, 0.01, 0.125]) },
      { model: "gpt-5-codex", provider: "codex", priced: true, ...sum(48, 410_000, 2_100_000, 0, 96_000, [1.25, 10, 0.125, 1.25]) },
    ],
    recent: Array.from({ length: 5 }, (_, i) => ({
      at: end - i * 420_000, request_id: `demo-${i}`, model: "claude-sonnet-5-5", provider: "claude", failed: false,
      input_tokens: 4, output_tokens: 300 + i * 40, cache_read_tokens: 180_000, cache_write_tokens: 500, cost_usd: 0.04, savings_usd: 0.33,
    })),
  };
}

export function demoStream(h: StreamHandlers): () => void {
  const push = (s: FlowSnapshot) => h.onSnapshot(s);
  listeners.add(push);
  void load().then((s) => {
    h.onOpen?.();
    push(structuredClone(s));
  });
  return () => listeners.delete(push);
}
