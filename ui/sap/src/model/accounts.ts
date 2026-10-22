import type { AccountState, AccountView, WindowView } from "../api/types.ts";
import { when, windowLimit, windowName } from "./format.ts";

export type Tone = "live" | "idle" | "blocked";

export interface WindowRow {
  name: string;
  used: number;
  /** "in 48 m", "Mon 09:00" or "". */
  reset: string;
}

export interface AccountPresentation {
  tone: Tone;
  /** Drawn greyed out. Everything but accounts with active threads. */
  greyed: boolean;
  /** Why it cannot take new threads, with the time it can again. */
  flag: string | null;
  windows: WindowRow[];
}

const TONES: Record<AccountState, Tone> = {
  active: "live",
  idle: "idle",
  cooling: "blocked",
  exhausted: "blocked",
  disabled: "blocked",
  login_needed: "blocked",
};

function flagFor(a: AccountView, now: string, tz?: string): string | null {
  const back = when(a.available_at, now, tz);
  switch (a.state) {
    case "exhausted": {
      const full = a.windows.filter((w) => w.used_percent >= 100).map((w) => windowLimit(w.kind));
      const what = full.length ? `${full.join(" and ")} reached` : "limit reached";
      return back ? `${what} · resets ${back}` : what;
    }
    case "cooling":
      return back ? `cooling down · available ${back}` : "cooling down";
    case "disabled":
      return "disabled";
    case "login_needed":
      return a.login_needed_on.length ? `sign in again on ${a.login_needed_on.join(", ")}` : "sign in again";
    default:
      return a.stale ? "quota data may be stale" : null;
  }
}

const rowOf = (w: WindowView, now: string, tz?: string): WindowRow => ({
  name: windowName(w.kind),
  used: w.used_percent,
  reset: when(w.reset_at, now, tz),
});

export function presentAccount(a: AccountView, now: string, tz?: string): AccountPresentation {
  const tone = TONES[a.state];
  return {
    tone,
    greyed: tone !== "live",
    flag: flagFor(a, now, tz),
    windows: a.windows.map((w) => rowOf(w, now, tz)),
  };
}

/** Bar colour class for a used percentage. */
export const level = (used: number): "ok" | "warn" | "bad" => (used >= 90 ? "bad" : used >= 70 ? "warn" : "ok");
