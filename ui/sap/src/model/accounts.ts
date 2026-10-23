import type { AccountState, AccountView, WindowView } from "../api/types.ts";
import { remaining, when, windowLimit, windowName } from "./format.ts";

export type Tone = "live" | "idle" | "blocked";

export interface WindowRow {
  name: string;
  /** Percent of the window still available. */
  left: number;
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
  left: remaining(w.used_percent),
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

/** Bar colour class for a remaining percentage (same cut-offs as 90% and 70% used). */
export const level = (left: number): "ok" | "warn" | "bad" => (left <= 10 ? "bad" : left <= 30 ? "warn" : "ok");
