import assert from "node:assert/strict";
import test from "node:test";
import { bezierPoint, dotsDue, LEG_MS, MAX_DOTS, placeDot, SPAWN_MS, uniqueRoutes, type Route } from "./dots.ts";

const line = { x1: 0, y1: 0, x2: 100, y2: 40 };
const route: Route = { from: "a", to: "b", color: "red" };

test("bezier starts and ends on the wire and crosses its middle", () => {
  assert.deepEqual(bezierPoint(line, 0), { x: 0, y: 0 });
  assert.deepEqual(bezierPoint(line, 1), { x: 100, y: 40 });
  assert.deepEqual(bezierPoint(line, 0.5), { x: 50, y: 20 });
});

test("a route emits at once, then once per interval, and is forgotten when it stops", () => {
  const next = new Map<string, number>();
  assert.equal(dotsDue(next, [route], 1000, 0).length, 1);
  assert.equal(dotsDue(next, [route], 1000 + SPAWN_MS - 1, 1).length, 0);
  assert.equal(dotsDue(next, [route], 1000 + SPAWN_MS, 1).length, 1);
  dotsDue(next, [], 5000, 0);
  assert.equal(next.size, 0);
});

test("the cap bounds concurrent dots and duplicate routes collapse", () => {
  const many = Array.from({ length: MAX_DOTS + 5 }, (_, i) => ({ ...route, from: `w${i}` }));
  assert.equal(dotsDue(new Map(), many, 0, 0).length, MAX_DOTS);
  assert.equal(dotsDue(new Map(), many, 0, MAX_DOTS).length, 0);
  assert.equal(uniqueRoutes([route, { ...route }, { ...route, to: "c" }]).length, 2);
});

test("a dot crosses both wires, follows the current geometry, and ends", () => {
  const moved = { x1: 0, y1: 100, x2: 100, y2: 100 };
  const wires: Record<string, typeof line> = { a: line, b: moved };
  const dot = { route, born: 0 };
  const at = (t: number) => placeDot(dot, t, (k) => wires[k]);
  assert.deepEqual(at(LEG_MS * 0.5)?.y, bezierPoint(line, 0.5).y);
  assert.deepEqual(at(LEG_MS * 1.5)?.y, 100);
  assert.equal(at(LEG_MS * 2), null);
  assert.equal(placeDot(dot, LEG_MS * 1.5, () => undefined), null);
});
