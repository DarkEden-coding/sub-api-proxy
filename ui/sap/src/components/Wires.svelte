<script lang="ts">
  import type { ClientKind, FlowSnapshot, ThreadView } from "../api/types.ts";
  import { dotsDue, LEG_MS, placeDot, uniqueRoutes, type Dot, type Placed, type Route } from "../model/dots.ts";
  import { linkKey, servingNode, threadCounts, flatten, type ClientGroup } from "../model/threads.ts";

  let { host, snapshot, groups, collapsed, selected }: {
    host: HTMLElement | undefined;
    snapshot: FlowSnapshot;
    groups: ClientGroup[];
    collapsed: Record<string, boolean>;
    selected: ThreadView | null;
  } = $props();

  /** The animated numbers of a wire. */
  interface Geometry { x1: number; y1: number; x2: number; y2: number; width: number; opacity: number }
  /** `dest` names what the wire ends at, so a wire that appears or leaves can slide to or from a sibling. */
  interface Wire extends Geometry { key: string; dest: string; color: string; dashed?: boolean }
  interface Shown { wire: Wire; from: Geometry; now: Geometry; dying: boolean }
  interface Circle { x: number; y: number; color: string }
  let shown = $state.raw<Shown[]>([]);
  let dots = $state.raw<Circle[]>([]);

  const DURATION = 260;
  const geometryKeys = ["x1", "y1", "x2", "y2", "width", "opacity"] as const;
  const ease = (p: number) => 1 - (1 - p) ** 3;
  const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");
  const entries = new Map<string, Shown>();
  let started = 0;
  let frame = 0;

  const geometry = (w: Geometry): Geometry => ({ x1: w.x1, y1: w.y1, x2: w.x2, y2: w.y2, width: w.width, opacity: w.opacity });
  /** The wire with the same destination whose start is nearest, for entering and leaving wires to move toward. */
  const sibling = (w: Wire, pool: Iterable<{ wire: Wire; now: Geometry }>) => {
    let best: Geometry | undefined;
    for (const o of pool) {
      if (o.wire.dest !== w.dest || o.wire.key === w.key) continue;
      if (!best || Math.abs(o.now.y1 - w.y1) < Math.abs(best.y1 - w.y1)) best = o.now;
    }
    return best;
  };

  /** Moves every wire from where it is drawn now to its new target. */
  function retarget(next: Wire[]) {
    const keys = new Set(next.map((w) => w.key));
    const before = [...entries.values()];
    for (const e of before) if (!keys.has(e.wire.key)) e.dying = true;
    for (const w of next) {
      const old = entries.get(w.key);
      const near = old ? undefined : sibling(w, before.filter((e) => !e.dying));
      const from = old ? { ...old.now } : { ...(near ?? w), width: w.width, opacity: 0 };
      entries.set(w.key, { wire: w, from, now: { ...from }, dying: false });
    }
    for (const e of before) {
      if (!e.dying) continue;
      const near = sibling(e.wire, next.map((wire) => ({ wire, now: wire })));
      e.from = { ...e.now };
      e.wire = { ...e.wire, ...(near ?? geometry(e.wire)), opacity: 0 };
    }
    started = performance.now();
    cancelAnimationFrame(frame);
    step(started);
  }

  function step(time: number) {
    const raw = reduceMotion.matches ? 1 : Math.min(1, Math.max(0, (time - started) / DURATION));
    const p = ease(raw);
    for (const [key, e] of entries) {
      if (raw === 1 && e.dying) {
        entries.delete(key);
        continue;
      }
      for (const k of geometryKeys) e.now[k] = e.from[k] + (e.wire[k] - e.from[k]) * p;
    }
    shown = [...entries.values()].map((e) => ({ ...e, now: { ...e.now } }));
    if (raw < 1) frame = requestAnimationFrame(step);
  }

  // Dots travel thread -> server -> account along the wires as currently drawn, while a thread streams.
  let routes: Route[] = [];
  let flight: Dot[] = [];
  let flightFrame = 0;
  const nextAt = new Map<string, number>();
  let flying = $state.raw<Placed[]>([]);

  const wake = () => {
    if (!flightFrame && !document.hidden && !reduceMotion.matches) flightFrame = requestAnimationFrame(fly);
  };
  const stopFlight = () => {
    cancelAnimationFrame(flightFrame);
    flightFrame = 0;
    flight = [];
    flying = [];
    nextAt.clear();
  };
  function fly(time: number) {
    flightFrame = 0;
    if (document.hidden || reduceMotion.matches) return stopFlight();
    const wire = (key: string) => entries.get(key)?.now;
    flight = flight.filter((d) => time - d.born < 2 * LEG_MS);
    for (const route of dotsDue(nextAt, routes, time, flight.length)) flight.push({ route, born: time });
    flying = flight.flatMap((d) => placeDot(d, time, wire) ?? []);
    if (flight.length || routes.length) flightFrame = requestAnimationFrame(fly);
  }

  const path = (g: Geometry) => curve({ x: g.x1, y: g.y1 }, { x: g.x2, y: g.y2 });

  const providerColor = (p: string) => (p === "claude" ? "var(--claude)" : p === "codex" ? "var(--openai)" : "var(--muted)");
  const clientColor: Record<ClientKind, string> = { claude_code: "var(--claude)", codex_cli: "var(--openai)", other: "var(--muted)" };
  const inOffset: Record<ClientKind, number> = { claude_code: -12, codex_cli: 12, other: 0 };

  const curve = (a: { x: number; y: number }, b: { x: number; y: number }) => {
    const mx = (a.x + b.x) / 2;
    return `M${a.x},${a.y} C${mx},${a.y} ${mx},${b.y} ${b.x},${b.y}`;
  };

  function draw() {
    if (!host) return;
    const box = host.getBoundingClientRect();
    const pt = (el: Element, side: "l" | "r") => {
      const r = el.getBoundingClientRect();
      return { x: (side === "r" ? r.right : r.left) - box.left, y: r.top + r.height / 2 - box.top };
    };
    const byAttr = (attr: string) =>
      new Map([...host.querySelectorAll(`[data-${attr}]`)].map((el) => [(el as HTMLElement).dataset[attr]!, el]));
    const serverEls = byAttr("node");
    const accountEls = byAttr("account");
    const threadEls = byAttr("thread");
    const unitEls = byAttr("unit");
    const { nodes, accounts, threads, node: self } = snapshot;

    const out: Wire[] = [];
    const wire = (key: string, dest: string, a: { x: number; y: number }, b: { x: number; y: number }, rest: { color: string; dashed?: boolean; width: number; opacity: number }) =>
      out.push({ key, dest, x1: a.x, y1: a.y, x2: b.x, y2: b.y, ...rest });
    const circles: Circle[] = [];
    const streaming: Route[] = [];
    const perNode = new Map<string, number>();

    // Threads (or a collapsed group of them) to the server that serves them.
    const anchors: { id: string; el: Element | undefined; threads: ThreadView[]; client: ClientKind }[] = [];
    for (const g of groups) {
      if (collapsed[g.id]) {
        anchors.push({ id: g.id, el: unitEls.get(g.id), threads: g.machines.flatMap((m) => m.roots.flatMap(flatten)), client: g.id });
        continue;
      }
      for (const m of g.machines) {
        const all = m.roots.flatMap(flatten);
        if (collapsed[m.id]) anchors.push({ id: m.id, el: unitEls.get(m.id), threads: all, client: g.id });
        else for (const t of all) anchors.push({ id: t.key, el: threadEls.get(t.key), threads: [t], client: g.id });
      }
    }
    for (const a of anchors) {
      if (!a.el) continue;
      const from = pt(a.el, "r");
      from.x += 8;
      const targets = new Map<string, ThreadView[]>();
      for (const t of a.threads) {
        const n = servingNode(t, nodes, self);
        if (n) targets.set(n, [...(targets.get(n) ?? []), t]);
        if (n) perNode.set(n, (perNode.get(n) ?? 0) + 1);
      }
      for (const [nodeId, ts] of targets) {
        const server = serverEls.get(nodeId);
        if (!server) continue;
        const to = pt(server, "l");
        to.y += inOffset[a.client];
        const sel = ts.length === 1 && ts[0].key === selected?.key;
        for (const t of ts) {
          if (t.streaming && t.account) streaming.push({ from: `${a.id}>${nodeId}`, to: `${nodeId}:${t.account}`, color: clientColor[a.client] });
        }
        wire(`${a.id}>${nodeId}`, nodeId, from, to, {
          color: clientColor[a.client],
          width: ts.length > 1 ? 1 + ts.length : sel ? 2.4 : 1.4,
          opacity: sel ? 0.95 : ts.length > 1 ? 0.35 : 0.32,
        });
      }
    }

    // Servers to accounts; thickness follows thread count, idle accounts get a dashed stub.
    const counts = threadCounts(threads, nodes, self);
    const activeNode = [...perNode.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] ?? self;
    const selNode = selected ? servingNode(selected, nodes, self) : null;
    for (const acct of accounts) {
      const el = accountEls.get(acct.id);
      if (!el) continue;
      const to = pt(el, "l");
      to.x -= 6;
      let served = false;
      for (const n of nodes) {
        const server = serverEls.get(n.id);
        const c = counts.get(linkKey(n.id, acct.id)) ?? 0;
        if (!server || !c) continue;
        served = true;
        const from = pt(server, "r");
        from.y += acct.provider === "claude" ? -12 : 12;
        const sel = selected?.account === acct.id && selNode === n.id;
        wire(`${n.id}:${acct.id}`, acct.id, from, to, { color: providerColor(acct.provider), width: (sel ? 1.5 : 1) + c * 1.2, opacity: sel ? 0.9 : 0.3 });
      }
      if (!served) {
        const server = serverEls.get(activeNode);
        if (server) wire(`idle:${acct.id}`, acct.id, pt(server, "r"), to, { color: "var(--faint)", width: 1, opacity: 0.45, dashed: true });
      }
      circles.push({ x: to.x, y: to.y, color: served ? providerColor(acct.provider) : "var(--faint)" });
    }
    retarget(out);
    dots = circles;
    routes = uniqueRoutes(streaming);
    wake();
  }

  $effect(() => {
    // Redraw after the DOM settles whenever any input changes.
    void [snapshot, groups, collapsed, selected, host];
    const frame = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(frame);
  });

  $effect(() => {
    if (!host) return;
    const ro = new ResizeObserver(draw);
    ro.observe(host);
    return () => ro.disconnect();
  });

  $effect(() => () => cancelAnimationFrame(frame));

  $effect(() => {
    const onVisibility = () => (document.hidden ? stopFlight() : wake());
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      stopFlight();
    };
  });
</script>

<svg aria-hidden="true">
  {#each shown as { wire: w, now } (w.key)}
    <path d={path(now)} fill="none" stroke={w.color} stroke-width={now.width} stroke-opacity={now.opacity} stroke-dasharray={w.dashed ? "2 4" : undefined} />
  {/each}
  {#each dots as d}<circle cx={d.x} cy={d.y} r="2.5" fill={d.color} />{/each}
  {#each flying as d}<circle cx={d.x} cy={d.y} r="2" fill={d.color} opacity={d.opacity} />{/each}
</svg>

<style>
  svg { position: absolute; inset: 0; width: 100%; height: 100%; pointer-events: none; overflow: visible; }
</style>
