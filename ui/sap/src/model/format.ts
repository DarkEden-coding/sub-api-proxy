import type { ThreadKey, WindowKind } from "../api/types.ts";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** Percent of a limit still available, from the API's used percent. Clamped to 0-100. */
export const remaining = (usedPercent: number): number => Math.min(100, Math.max(0, 100 - usedPercent));

/** "2 h 14 m", "48 m", "3 d 4 h", "<1 m". */
export function duration(ms: number): string {
  const m = Math.round(ms / MINUTE);
  if (m < 1) return "<1 m";
  if (m < 60) return `${m} m`;
  if (ms < DAY) return `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, "0")} m`;
  const d = Math.floor(ms / DAY);
  return `${d} d ${Math.round((ms - d * DAY) / HOUR)} h`;
}

export function ago(iso: string, now: string): string {
  const ms = Date.parse(now) - Date.parse(iso);
  return ms < MINUTE ? "just now" : `${duration(ms)} ago`;
}

function parts(iso: string, tz: string | undefined, opts: Intl.DateTimeFormatOptions): Record<string, string> {
  const f = new Intl.DateTimeFormat("en-GB", { timeZone: tz, hourCycle: "h23", ...opts });
  return Object.fromEntries(f.formatToParts(new Date(iso)).map((p) => [p.type, p.value]));
}

/** "Mon 09:00". */
export function weekdayTime(iso: string, tz?: string): string {
  const p = parts(iso, tz, { weekday: "short", hour: "2-digit", minute: "2-digit" });
  return `${p.weekday} ${p.hour}:${p.minute}`;
}

/** "16:24". */
export function clock(iso: string, tz?: string): string {
  const p = parts(iso, tz, { hour: "2-digit", minute: "2-digit" });
  return `${p.hour}:${p.minute}`;
}

/** "Thu 22 Oct, 16:24". */
export function headerClock(iso: string, tz?: string): string {
  const p = parts(iso, tz, { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
  return `${p.weekday} ${p.day} ${p.month}, ${p.hour}:${p.minute}`;
}

/** A calendar day key in the given zone, for "today" comparisons. */
export function dayKey(iso: string, tz?: string): string {
  const p = parts(iso, tz, { year: "numeric", month: "2-digit", day: "2-digit" });
  return `${p.year}-${p.month}-${p.day}`;
}

/** "in 48 m" within a day, otherwise "Mon 09:00". Empty when no time is known. */
export function when(iso: string | null, now: string, tz?: string): string {
  if (!iso) return "";
  const ms = Date.parse(iso) - Date.parse(now);
  if (ms <= 0) return "now";
  return ms < DAY ? `in ${duration(ms)}` : weekdayTime(iso, tz);
}

export function bytes(n: number | null): string {
  if (n === null) return "—";
  return n >= 1 << 30 ? `${(n / (1 << 30)).toFixed(1)} GB` : `${Math.round(n / (1 << 20))} MB`;
}

export function windowName(kind: WindowKind): string {
  if (kind === "five_hour") return "5 h";
  if (kind === "weekly") return "week";
  return kind.other;
}

/** Long form for sentences: "weekly limit". */
export function windowLimit(kind: WindowKind): string {
  if (kind === "five_hour") return "5-hour limit";
  if (kind === "weekly") return "weekly limit";
  return `${kind.other} limit`;
}

/** Short display id: "7fe0·2bbb", or "agent 6b98" for a Claude subagent key. */
export function shortKey(key: ThreadKey): string {
  const id = key.slice(key.indexOf(":") + 1);
  const slash = id.indexOf("/");
  if (slash >= 0) return `agent ${id.slice(slash + 1, slash + 5)}`;
  return id.length > 9 ? `${id.slice(0, 4)}·${id.slice(-4)}` : id;
}

/** Dollars: cents below $100, whole dollars above, "<$0.01" for a trace, "—" when unpriced. */
export function usd(n: number | null): string {
  if (n === null || !Number.isFinite(n)) return "—";
  if (n !== 0 && Math.abs(n) < 0.01) return "<$0.01";
  const places = Math.abs(n) >= 100 ? 0 : 2;
  return n.toLocaleString("en-US", { style: "currency", currency: "USD", minimumFractionDigits: places, maximumFractionDigits: places });
}

/** Token counts: 1,234 -> "1.2K", 5,600,000 -> "5.6M". */
export function compact(n: number): string {
  for (const [size, suffix] of [[1e9, "B"], [1e6, "M"], [1e3, "K"]] as const) {
    if (Math.abs(n) >= size) return `${(n / size).toFixed(Math.abs(n) >= size * 100 ? 0 : 1).replace(/\.0$/, "")}${suffix}`;
  }
  return String(Math.round(n));
}

/** Whole percent of input-side tokens that came from the cache. */
export function cacheShare(t: { input_tokens: number; cache_read_tokens: number; cache_write_tokens: number }): number {
  const all = t.input_tokens + t.cache_read_tokens + t.cache_write_tokens;
  return all ? Math.round((t.cache_read_tokens / all) * 100) : 0;
}
