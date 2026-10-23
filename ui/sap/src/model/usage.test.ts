import assert from "node:assert/strict";
import test from "node:test";
import type { UsageDay } from "../api/types.ts";
import { arc, fillDays, niceMax, percent, slices } from "./usage.ts";

test("missing days are filled so the time axis is even", () => {
  const day = { date: "2026-10-21", requests: 3 } as UsageDay;
  const rows = fillDays([day], 3, Date.parse("2026-10-23T15:00:00Z"));
  assert.deepEqual(rows.map((r) => r.date), ["2026-10-21", "2026-10-22", "2026-10-23"]);
  assert.equal(rows[0], day);
  assert.ok("empty" in rows[1]);
});

test("axis maxima round up to 1, 2 or 5 steps", () => {
  assert.equal(niceMax(0), 1);
  assert.equal(niceMax(3.2), 5);
  assert.equal(niceMax(17), 20);
  assert.equal(niceMax(64_000_000), 100_000_000);
  assert.equal(niceMax(0.04), 0.05);
});

test("slices cover the circle in proportion; nothing when all zero", () => {
  const s = slices([1, 3]);
  assert.equal(s[0].share, 0.25);
  assert.ok(Math.abs(s[1].a1 - Math.PI * 2) < 1e-9);
  assert.deepEqual(slices([0, 0]), []);
  assert.match(arc(50, 50, 40, 28, 0, Math.PI * 2), /^M50\.00,10\.00 A40,40 0 1 1 /);
});

test("percent labels never round a sliver to zero", () => {
  assert.equal(percent(0.004), "<1%");
  assert.equal(percent(0.256), "26%");
  assert.equal(percent(0), "0%");
});
