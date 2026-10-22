import assert from "node:assert/strict";
import test from "node:test";
import { fixture } from "./fixture.ts";
import { chartPaths } from "./series.ts";
import { parseSse } from "./sse.ts";

test("chart paths span the box and stay inside the margins", () => {
  const series = fixture().stats.series;
  const c = chartPaths(series, 400, 150);
  const nums = [...c.requests.matchAll(/[ML]([\d.]+),([\d.]+)/g)];
  assert.equal(nums.length, series.length);
  assert.equal(nums[0][1], "0.0");
  assert.equal(nums.at(-1)![1], "400.0");
  assert.ok(nums.every((m) => Number(m[2]) >= 10 && Number(m[2]) <= 140));
  assert.equal(chartPaths([], 400, 150).cacheArea, "");
});

test("sse parser keeps partial events and joins data lines", () => {
  const first = parseSse(": hi\n\nevent: snapshot\ndata: {\"a\":\ndata: 1}\n\nevent: ev");
  assert.deepEqual(first.messages, [{ event: "snapshot", data: '{"a":\n1}' }]);
  assert.equal(first.rest, "event: ev");
  const second = parseSse(first.rest + 'ent\r\ndata: x\r\n\r\n');
  assert.deepEqual(second.messages, [{ event: "event", data: "x" }]);
});
