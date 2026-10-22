import assert from "node:assert/strict";
import test from "node:test";
import type { EventView } from "../api/types.ts";
import { mergeEvents } from "./events.ts";

const ev = (id: string, minute: number): EventView => ({
  id, at: new Date(Date.UTC(2026, 9, 22, 12, minute)).toISOString(), kind: "thread_assigned",
  message: id, node: "macbook", thread: null, account: null,
});
const ids = (list: EventView[]) => list.map((e) => e.id);

test("a new event goes to the top", () => {
  assert.deepEqual(ids(mergeEvents([ev("a", 1)], [ev("b", 2)])), ["b", "a"]);
});

test("a repeated id is ignored, including within the batch", () => {
  const current = [ev("b", 2), ev("a", 1)];
  assert.equal(mergeEvents(current, [ev("a", 1), ev("b", 2)]), current);
  assert.deepEqual(ids(mergeEvents(current, [ev("c", 3), ev("c", 3)])), ["c", "b", "a"]);
});

test("older fetched events slot in by time", () => {
  assert.deepEqual(ids(mergeEvents([ev("c", 3), ev("a", 1)], [ev("b", 2)])), ["c", "b", "a"]);
});

test("the list is capped, dropping the oldest", () => {
  const current = Array.from({ length: 500 }, (_, i) => ev(`e${i}`, 500 - i));
  const merged = mergeEvents(current, [ev("new", 501)]);
  assert.equal(merged.length, 500);
  assert.equal(merged[0].id, "new");
  assert.equal(merged.at(-1)?.id, "e498");
});
