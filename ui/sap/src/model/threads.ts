import type { AccountId, ClientKind, NodeId, NodeView, ThreadKey, ThreadView } from "../api/types.ts";

export interface ThreadNode {
  thread: ThreadView;
  children: ThreadNode[];
}

export interface MachineGroup {
  id: string;
  name: string;
  roots: ThreadNode[];
  count: number;
  streaming: number;
}

export interface ClientGroup {
  id: ClientKind;
  name: string;
  glyph: string;
  machines: MachineGroup[];
  count: number;
  streaming: number;
}

const CLIENTS: { id: ClientKind; name: string; glyph: string }[] = [
  { id: "claude_code", name: "Claude Code", glyph: "✳" },
  { id: "codex_cli", name: "Codex CLI", glyph: "›_" },
  { id: "other", name: "Other clients", glyph: "·" },
];

const byAge = (a: ThreadView, b: ThreadView) =>
  Date.parse(a.first_seen) - Date.parse(b.first_seen) || (a.key < b.key ? -1 : a.key > b.key ? 1 : 0);

export function flatten(node: ThreadNode): ThreadView[] {
  return [node.thread, ...node.children.flatMap(flatten)];
}

/**
 * Client, then machine, then the parent/child tree. A child is nested under its parent even
 * when it ran on another machine; a thread whose parent is not in the list (or loops) is a root.
 */
export function groupThreads(threads: ThreadView[]): ClientGroup[] {
  const byKey = new Map(threads.map((t) => [t.key, t]));
  // A parent counts only when it is in the list and the chain above it does not loop back.
  const hasParent = (t: ThreadView): boolean => {
    if (!t.parent || !byKey.has(t.parent)) return false;
    const seen = new Set<ThreadKey>([t.key]);
    for (let up = byKey.get(t.parent); up; up = up.parent ? byKey.get(up.parent) : undefined) {
      if (seen.has(up.key)) return false;
      seen.add(up.key);
    }
    return true;
  };
  const nodes = new Map<ThreadKey, ThreadNode>(threads.map((t) => [t.key, { thread: t, children: [] }]));
  const roots: ThreadNode[] = [];
  for (const t of [...threads].sort(byAge)) {
    const node = nodes.get(t.key)!;
    (hasParent(t) ? nodes.get(t.parent!)!.children : roots).push(node);
  }

  const groups: ClientGroup[] = [];
  for (const c of CLIENTS) {
    const ofClient = roots.filter((n) => n.thread.client === c.id);
    if (!ofClient.length) continue;
    const machineNames = [...new Set(ofClient.map((n) => n.thread.machine ?? ""))].sort((a, b) =>
      a === "" ? 1 : b === "" ? -1 : a.localeCompare(b),
    );
    const machines = machineNames.map((m): MachineGroup => {
      const mr = ofClient.filter((n) => (n.thread.machine ?? "") === m);
      const all = mr.flatMap(flatten);
      return {
        id: `${c.id}|${m}`,
        name: m || "unknown machine",
        roots: mr,
        count: all.length,
        streaming: all.filter((t) => t.streaming).length,
      };
    });
    groups.push({
      ...c,
      machines,
      count: machines.reduce((s, m) => s + m.count, 0),
      streaming: machines.reduce((s, m) => s + m.streaming, 0),
    });
  }
  return groups;
}

/** The node a thread is served by, falling back to the node that produced the snapshot. */
export function servingNode(t: ThreadView, nodes: NodeView[], snapshotNode: NodeId): NodeId | null {
  if (t.serving_node && nodes.some((n) => n.id === t.serving_node)) return t.serving_node;
  if (nodes.some((n) => n.id === snapshotNode)) return snapshotNode;
  return nodes[0]?.id ?? null;
}

/** Threads per (node, account), for line thickness between the middle and right columns. */
export function threadCounts(threads: ThreadView[], nodes: NodeView[], snapshotNode: NodeId) {
  const counts = new Map<string, number>();
  for (const t of threads) {
    const node = servingNode(t, nodes, snapshotNode);
    if (!node || !t.account) continue;
    const k = linkKey(node, t.account);
    counts.set(k, (counts.get(k) ?? 0) + 1);
  }
  return counts;
}

export const linkKey = (node: NodeId, account: AccountId) => `${node}\u0000${account}`;

/** Case-insensitive match of a search string against what the Threads page shows. */
export function matchesThread(t: ThreadView, q: string, accountLabel: string): boolean {
  const needle = q.trim().toLowerCase();
  if (!needle) return true;
  return [t.title, t.key, t.machine, t.agent_label, t.reason, t.lineage, accountLabel]
    .some((s) => s?.toLowerCase().includes(needle));
}
