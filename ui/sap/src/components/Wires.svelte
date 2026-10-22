<script lang="ts">
  import type { ClientKind, FlowSnapshot, ThreadView } from "../api/types.ts";
  import { linkKey, servingNode, threadCounts, flatten, type ClientGroup } from "../model/threads.ts";

  let { host, snapshot, groups, collapsed, selected }: {
    host: HTMLElement | undefined;
    snapshot: FlowSnapshot;
    groups: ClientGroup[];
    collapsed: Record<string, boolean>;
    selected: ThreadView | null;
  } = $props();

  interface Wire { d: string; color: string; width: number; opacity: number; dashed?: boolean }
  interface Dot { x: number; y: number; color: string }
  let wires = $state.raw<Wire[]>([]);
  let dots = $state.raw<Dot[]>([]);

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
    const circles: Dot[] = [];
    const perNode = new Map<string, number>();

    // Threads (or a collapsed group of them) to the server that serves them.
    const anchors: { el: Element | undefined; threads: ThreadView[]; client: ClientKind }[] = [];
    for (const g of groups) {
      if (collapsed[g.id]) {
        anchors.push({ el: unitEls.get(g.id), threads: g.machines.flatMap((m) => m.roots.flatMap(flatten)), client: g.id });
        continue;
      }
      for (const m of g.machines) {
        const all = m.roots.flatMap(flatten);
        if (collapsed[m.id]) anchors.push({ el: unitEls.get(m.id), threads: all, client: g.id });
        else for (const t of all) anchors.push({ el: threadEls.get(t.key), threads: [t], client: g.id });
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
        out.push({
          d: curve(from, to),
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
        out.push({ d: curve(from, to), color: providerColor(acct.provider), width: (sel ? 1.5 : 1) + c * 1.2, opacity: sel ? 0.9 : 0.3 });
      }
      if (!served) {
        const server = serverEls.get(activeNode);
        if (server) out.push({ d: curve(pt(server, "r"), to), color: "var(--faint)", width: 1, opacity: 0.45, dashed: true });
      }
      circles.push({ x: to.x, y: to.y, color: served ? providerColor(acct.provider) : "var(--faint)" });
    }

    // Servers that carry nothing: a short dashed stub shows they are standing by.
    for (const n of nodes) {
      const server = serverEls.get(n.id);
      if (!server || perNode.has(n.id)) continue;
      const l = pt(server, "l");
      out.push({ d: `M${l.x - 40},${l.y} L${l.x},${l.y}`, color: "var(--faint)", width: 1, opacity: 0.6, dashed: true });
    }
    wires = out;
    dots = circles;
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
</script>

<svg aria-hidden="true">
  {#each wires as w}
    <path d={w.d} fill="none" stroke={w.color} stroke-width={w.width} stroke-opacity={w.opacity} stroke-dasharray={w.dashed ? "2 4" : undefined} />
  {/each}
  {#each dots as d}<circle cx={d.x} cy={d.y} r="2.5" fill={d.color} />{/each}
</svg>

<style>
  svg { position: absolute; inset: 0; width: 100%; height: 100%; pointer-events: none; overflow: visible; }
</style>
