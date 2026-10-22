import assert from "node:assert/strict";
import test from "node:test";
import { planName } from "./plans.ts";

test("Claude plans", () => {
  assert.equal(planName("pro"), "Pro");
  assert.equal(planName("default_claude_max_5x"), "Max 5×");
  assert.equal(planName("default_claude_max_20x"), "Max 20×");
  assert.equal(planName("claude_team"), "Team");
  assert.equal(planName("enterprise"), "Enterprise");
});

test("ChatGPT plans", () => {
  for (const [raw, shown] of [["free", "Free"], ["plus", "Plus"], ["pro", "Pro"], ["team", "Team"], ["business", "Business"], ["enterprise", "Enterprise"]]) {
    assert.equal(planName(raw), shown);
  }
  assert.equal(planName("PLUS"), "Plus");
});

test("an unknown plan shows its raw value in title case", () => {
  assert.equal(planName("prolite"), "Prolite");
  assert.equal(planName("default_raven_max"), "Default Raven Max");
  assert.equal(planName("edu"), "Edu");
});

test("no plan is empty", () => {
  assert.equal(planName(null), "");
  assert.equal(planName("  "), "");
});
