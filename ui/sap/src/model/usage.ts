// Shapes for the Usage page charts. Pure, so they are tested without a browser.
import type { UsageDay, UsageSum } from "../api/types.ts";

const DAY = 86_400_000;

/** The token types, in stacking order, with colours checked for colour-blind separation on the panel surface. */
export const TOKEN_TYPES = [
  { key: "input", label: "Input", color: "#5d8fd8" },
  { key: "cache_read", label: "Cache read", color: "#2fa37e" },
  { key: "cache_write", label: "Cache write", color: "#9085e9" },
  { key: "output", label: "Output", color: "#d9693a" },
] as const;

export const SPENT_COLOR = "#c9c0b1";
export const SAVED_COLOR = TOKEN_TYPES[1].color;

export const tokenCounts = (s: UsageSum): number[] => [s.input_tokens, s.cache_read_tokens, s.cache_write_tokens, s.output_tokens];

/**
 * One row per UTC day of the last `days` days ending on `now`, oldest first. The server only
 * reports days with traffic; the rest are zero so the time axis stays even.
 */
export function fillDays(rows: UsageDay[], days: number, now: number): (UsageDay | { date: string; empty: true })[] {
  const byDate = new Map(rows.map((r) => [r.date, r]));
  const today = Math.floor(now / DAY) * DAY;
  return Array.from({ length: days }, (_, i) => {
    const date = new Date(today - (days - 1 - i) * DAY).toISOString().slice(0, 10);
    return byDate.get(date) ?? { date, empty: true as const };
  });
}

/** A round axis maximum at or above `max`: 1, 2 or 5 times a power of ten. */
export function niceMax(max: number): number {
  if (max <= 0) return 1;
  const p = 10 ** Math.floor(Math.log10(max));
  return ([1, 2, 5, 10].find((m) => m * p >= max) ?? 10) * p;
}

/** A ring segment from angle `a0` to `a1` (radians, 0 at 12 o'clock, clockwise). */
export function arc(cx: number, cy: number, r: number, inner: number, a0: number, a1: number): string {
  // A full circle cannot be one arc; stop just short of it.
  const end = a1 - a0 >= Math.PI * 2 ? a0 + Math.PI * 2 - 1e-4 : a1;
  const pt = (rad: number, a: number) => `${(cx + rad * Math.sin(a)).toFixed(2)},${(cy - rad * Math.cos(a)).toFixed(2)}`;
  const large = end - a0 > Math.PI ? 1 : 0;
  return `M${pt(r, a0)} A${r},${r} 0 ${large} 1 ${pt(r, end)} L${pt(inner, end)} A${inner},${inner} 0 ${large} 0 ${pt(inner, a0)} Z`;
}

/** Start and end angles for each value's share of the whole; empty when everything is zero. */
export function slices(values: number[]): { a0: number; a1: number; share: number }[] {
  const total = values.reduce((a, b) => a + b, 0);
  if (total <= 0) return [];
  let at = 0;
  return values.map((v) => {
    const share = v / total;
    const a0 = at;
    at += share * Math.PI * 2;
    return { a0, a1: at, share };
  });
}

/** "12%", "<1%" for a sliver, "0%" for nothing. */
export const percent = (share: number): string => (share <= 0 ? "0%" : share < 0.01 ? "<1%" : `${Math.round(share * 100)}%`);
