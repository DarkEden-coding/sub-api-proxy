import assert from "node:assert/strict";
import test from "node:test";
import type { ResetsView } from "../api/types.ts";
import { fixture } from "./fixture.ts";
import { presentResets } from "./resets.ts";

const snap = fixture();
const resets = (id: string) => snap.accounts.find((a) => a.id === id)!.resets;

test("a grant that waits for a limit says so and cannot be used", () => {
  const p = presentResets(resets("claude-max-work"), snap.now, "UTC")!;
  assert.equal(p.count, "2");
  assert.equal(p.usable, false);
  assert.equal(p.note, "Usable once a usage limit is reached");
  assert.deepEqual(p.rows, [{ label: "Weekly limit reset", left: "2 of 2 left", expires: "expires 5 Nov" }]);
});

test("a single-use credit on an exhausted account can be used", () => {
  const p = presentResets(resets("codex-plus-alt"), snap.now, "UTC")!;
  assert.equal(p.usable, true);
  assert.equal(p.note, null);
  assert.deepEqual(p.rows, [{ label: "1 reset", left: "", expires: "expires 30 Oct" }]);
});

test("unread resets show nothing and a failed first check shows the failure", () => {
  assert.equal(presentResets(undefined, snap.now), null);
  const failed: ResetsView = { checked_at: snap.now, available: 0, blocked: null, credits: [], error: "The provider answered HTTP 403" };
  const p = presentResets(failed, snap.now)!;
  assert.equal(p.count, "—");
  assert.equal(p.usable, false);
  assert.equal(p.note, "Check failed: The provider answered HTTP 403");
});
