import assert from "node:assert/strict";
import test from "node:test";
import { cacheShare, compact, dayKey, duration, headerClock, remaining, shortKey, usd, when } from "./format.ts";

test("remaining percent counts down from used and stays within 0-100", () => {
  assert.equal(remaining(0), 100);
  assert.equal(remaining(38), 62);
  assert.equal(remaining(100), 0);
  assert.equal(remaining(130), 0);
  assert.equal(remaining(-5), 100);
});

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

test("short keys", () => {
  assert.equal(shortKey("claude:7fe0a2c1-0d7e-4c1e-9b1a-16beb50f2bbb"), "7fe0·2bbb");
  assert.equal(shortKey("claude:7fe0a2c1-0d7e/6b98dc80"), "agent 6b98");
});

test("usage dollars and token counts", () => {
  assert.equal(usd(12.345), "$12.35");
  assert.equal(usd(1234.5), "$1,235");
  assert.equal(usd(0.004), "<$0.01");
  assert.equal(usd(0), "$0.00");
  assert.equal(usd(null), "—");
  assert.equal(compact(950), "950");
  assert.equal(compact(1234), "1.2K");
  assert.equal(compact(5_600_000), "5.6M");
  assert.equal(compact(2_000_000_000), "2B");
  assert.equal(cacheShare({ input_tokens: 100, cache_read_tokens: 900, cache_write_tokens: 0 }), 90);
  assert.equal(cacheShare({ input_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0 }), 0);
});
