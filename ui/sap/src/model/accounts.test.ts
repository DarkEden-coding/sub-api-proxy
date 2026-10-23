import assert from "node:assert/strict";
import test from "node:test";
import type { AccountView } from "../api/types.ts";
import { level, presentAccount } from "./accounts.ts";
import { fixture } from "./fixture.ts";

const snap = fixture();
const acct = (id: string): AccountView => snap.accounts.find((a) => a.id === id)!;
const present = (a: AccountView) => presentAccount(a, snap.now, "UTC");

test("an account with active threads is not greyed", () => {
  const p = present(acct("claude-max-work"));
  assert.equal(p.greyed, false);
  assert.equal(p.tone, "live");
  assert.deepEqual(p.windows.map((w) => [w.name, w.left, w.reset]), [["5 h", 62, "in 2 h 14 m"], ["week", 59, "Mon 09:24"]]);
});

test("quota bars warn as the remaining share runs low", () => {
  assert.equal(level(100), "ok");
  assert.equal(level(31), "ok");
  assert.equal(level(30), "warn");
  assert.equal(level(11), "warn");
  assert.equal(level(10), "bad");
  assert.equal(level(0), "bad");
});

test("an exhausted window has 0% left", () => {
  const p = present(acct("codex-plus-alt"));
  assert.equal(p.windows[0].left, 0);
});

test("an idle account is greyed and keeps its reset times", () => {
  const p = present(acct("claude-pro-personal"));
  assert.equal(p.greyed, true);
  assert.equal(p.tone, "idle");
  assert.equal(p.windows[0].reset, "in 48 m");
});

test("an exhausted account says which limit and when it returns", () => {
  const p = present(acct("codex-plus-alt"));
  assert.equal(p.tone, "blocked");
  assert.equal(p.flag, "5-hour limit and weekly limit reached · resets Sun 08:24");
  assert.equal(p.windows[0].reset, "");
});

test("cooling, disabled and login-needed explain themselves", () => {
  const base = acct("claude-pro-alt");
  const state = (over: Partial<AccountView>) => present({ ...base, ...over });
  assert.equal(state({ state: "cooling", available_at: "2026-10-22T16:27:00Z" }).flag, "cooling down · available in 3 m");
  assert.equal(state({ state: "cooling", available_at: null }).flag, "cooling down");
  assert.equal(state({ state: "disabled" }).flag, "disabled");
  assert.equal(state({ state: "login_needed", login_needed_on: ["macbook"] }).flag, "sign in again on macbook");
  assert.equal(state({ state: "disabled" }).greyed, true);
});
