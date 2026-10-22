import assert from "node:assert/strict";
import test from "node:test";
import { dayKey, duration, headerClock, planLabel, shortKey, when } from "./format.ts";

test("durations", () => {
  assert.equal(duration(20_000), "<1 m");
  assert.equal(duration(48 * 60_000), "48 m");
  assert.equal(duration(134 * 60_000), "2 h 14 m");
  assert.equal(duration(76 * 3_600_000), "3 d 4 h");
});

test("a reset within a day is relative, later ones are weekday and time", () => {
  const now = "2026-10-22T16:24:00Z";
  assert.equal(when("2026-10-22T17:12:00Z", now, "UTC"), "in 48 m");
  assert.equal(when("2026-10-26T09:00:00Z", now, "UTC"), "Mon 09:00");
  assert.equal(when("2026-10-22T16:00:00Z", now, "UTC"), "now");
  assert.equal(when(null, now), "");
});

test("clock and day labels", () => {
  assert.equal(headerClock("2026-10-22T16:24:00Z", "UTC"), "Thu 22 Oct, 16:24");
  assert.equal(dayKey("2026-10-22T23:59:00Z", "UTC"), "2026-10-22");
});

test("short keys and plan labels", () => {
  assert.equal(shortKey("claude:7fe0a2c1-0d7e-4c1e-9b1a-16beb50f2bbb"), "7fe0·2bbb");
  assert.equal(shortKey("claude:7fe0a2c1-0d7e/6b98dc80"), "agent 6b98");
  assert.equal(planLabel("default_claude_max_20x"), "Max 20×");
  assert.equal(planLabel("plus"), "Plus");
  assert.equal(planLabel(null), "");
});
