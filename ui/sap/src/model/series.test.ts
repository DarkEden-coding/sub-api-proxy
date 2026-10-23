import assert from "node:assert/strict";
import test from "node:test";
import { monotonePath, monotoneSlopes } from "./curve.ts";
import { fixture } from "./fixture.ts";
import { blendSeries, chartPaths } from "./series.ts";
import { parseSse } from "./sse.ts";

test("chart points span the box and stay inside the margins", () => {
  const series = fixture().stats.series;
  const c = chartPaths(series, 400, 150);
  assert.equal(c.requestPoints.length, series.length);
  assert.equal(c.requestPoints[0].x, 0);
  assert.equal(c.requestPoints.at(-1)!.x, 400);
  assert.ok([...c.requestPoints, ...c.cachePoints].every((p) => p.y >= 10 && p.y <= 140));
  assert.ok(c.requests.startsWith("M") && c.requests.includes("C"));
  assert.equal(chartPaths([], 400, 150).cacheArea, "");
});

test("monotone slopes are flat at peaks and never overshoot a segment", () => {
  const pts = [0, 1, 2, 3, 4].map((x, i) => ({ x, y: [0, 0, 10, 0.5, 0.5][i] }));
  const m = monotoneSlopes(pts);
  assert.equal(m[2], 0);
  assert.equal(m[0], 0);
  // Sample every cubic segment: it must stay within the range of its two end values.
  const segs = monotonePath(pts).split(" C").slice(1);
  segs.forEach((s, i) => {
    const ys = s.split(" ").map((p) => Number(p.split(",")[1]));
    const lo = Math.min(pts[i].y, pts[i + 1].y);
    const hi = Math.max(pts[i].y, pts[i + 1].y);
    assert.ok(ys.every((y) => y >= lo - 0.05 && y <= hi + 0.05), `segment ${i}: ${s}`);
  });
});

test("blending interpolates values and snaps when the length changes", () => {
  const a = [{ start: "a", requests: 0, cache_read_tokens: 10, input_tokens: 0 }];
  const b = [{ start: "a", requests: 10, cache_read_tokens: 30, input_tokens: 0 }];
  assert.deepEqual(blendSeries(a, b, 0.5)[0], { start: "a", requests: 5, cache_read_tokens: 20, input_tokens: 0 });
  assert.equal(blendSeries(a, [...b, ...b], 0.5).length, 2);
});

test("sse parser keeps partial events and joins data lines", () => {
  const first = parseSse(": hi\n\nevent: snapshot\ndata: {\"a\":\ndata: 1}\n\nevent: ev");
  assert.deepEqual(first.messages, [{ event: "snapshot", data: '{"a":\n1}' }]);
  assert.equal(first.rest, "event: ev");
  const second = parseSse(first.rest + 'ent\r\ndata: x\r\n\r\n');
  assert.deepEqual(second.messages, [{ event: "event", data: "x" }]);
});
