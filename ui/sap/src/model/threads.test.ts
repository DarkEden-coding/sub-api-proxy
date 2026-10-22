import assert from "node:assert/strict";
import test from "node:test";
import type { ThreadView } from "../api/types.ts";
import { fixture } from "./fixture.ts";
import { flatten, groupThreads, matchesThread, servingNode, threadCounts } from "./threads.ts";

const base: ThreadView = {
  key: "claude:a", client: "claude_code", machine: "m1", title: "t", lineage: "root", parent: null,
  agent_label: null, account: "acct", reason: null, serving_node: null, streaming: false,
  cache_hit_percent: null, compactions: 0, requests: 1,
  first_seen: "2026-01-01T00:00:00Z", last_seen: "2026-01-01T00:00:00Z",
};
const th = (over: Partial<ThreadView>): ThreadView => ({ ...base, ...over });

test("groups by client, then machine, with the tree inside", () => {
  const groups = groupThreads(fixture().threads);
  assert.deepEqual(groups.map((g) => [g.id, g.count]), [["claude_code", 6], ["codex_cli", 4]]);
  const claude = groups[0];
  assert.deepEqual(claude.machines.map((m) => [m.name, m.count]), [["macbook-air", 4], ["scythe-desktop-1", 2]]);
  const root = claude.machines[0].roots.find((r) => r.thread.title === "Rust fork: peer sync module")!;
  assert.deepEqual(root.children.map((c) => c.thread.lineage).sort(), ["fork", "subagent", "subagent"]);
});

test("a child on another machine nests under its parent's machine", () => {
  const codex = groupThreads(fixture().threads)[1];
  const macbook = codex.machines.find((m) => m.name === "macbook-air")!;
  const root = macbook.roots[0];
  assert.ok(flatten(root).some((t) => t.machine === "scythe-desktop-1" && t.lineage === "fork"));
  assert.equal(codex.machines.find((m) => m.name === "scythe-desktop-1")!.roots.length, 1);
});

test("orphans and parent loops become roots; missing machine is its own group", () => {
  const groups = groupThreads([
    th({ key: "claude:a", parent: "claude:b" }),
    th({ key: "claude:b", parent: "claude:a" }),
    th({ key: "claude:c", parent: "claude:gone", machine: null }),
  ]);
  const machines = groups[0].machines;
  assert.deepEqual(machines.map((m) => m.name), ["m1", "unknown machine"]);
  assert.equal(machines[0].roots.length, 2);
  assert.equal(machines[1].roots.length, 1);
});

test("counts streaming threads and counts per node and account", () => {
  const snap = fixture();
  const groups = groupThreads(snap.threads);
  assert.equal(groups.reduce((s, g) => s + g.streaming, 0), snap.threads.filter((t) => t.streaming).length);
  const counts = threadCounts(snap.threads, snap.nodes, snap.node);
  assert.equal([...counts.values()].reduce((a, b) => a + b, 0), snap.threads.length);
});

test("serving node falls back to the snapshot node", () => {
  const snap = fixture();
  assert.equal(servingNode(th({ serving_node: "ghost" }), snap.nodes, snap.node), snap.node);
  assert.equal(servingNode(th({ serving_node: "macbook" }), snap.nodes, snap.node), "macbook");
});

test("search matches title, machine and account", () => {
  assert.ok(matchesThread(th({ title: "Quota Refactor" }), "refactor", ""));
  assert.ok(matchesThread(th({}), "m1", ""));
  assert.ok(matchesThread(th({}), "claude max", "Claude Max · work"));
  assert.ok(!matchesThread(th({}), "zzz", "x"));
});
