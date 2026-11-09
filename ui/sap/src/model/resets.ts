import type { ResetsView } from "../api/types.ts";
import { dayMonth, when } from "./format.ts";

const WEEK = 6 * 24 * 3_600_000;

export interface ResetRow {
  label: string;
  /** "2 of 3 left", or "" for single-use credits. */
  left: string;
  /** "expires in 3 h", "expires Mon 09:00", "expires 5 Nov" or "". */
  expires: string;
}

export interface ResetsPresentation {
  /** Resets left, or "—" when none were ever read. */
  count: string;
  /** One can be used now. */
  usable: boolean;
  /** Why none can be used, or why the last check failed. */
  note: string | null;
  rows: ResetRow[];
}

function expiry(iso: string | null, now: string, tz?: string): string {
  if (!iso) return "";
  const later = Date.parse(iso) - Date.parse(now) > WEEK;
  return `expires ${later ? dayMonth(iso, tz) : when(iso, now, tz)}`;
}

/** Null when the account's resets have not been read yet. */
export function presentResets(r: ResetsView | undefined, now: string, tz?: string): ResetsPresentation | null {
  if (!r) return null;
  const read = r.error === null || r.credits.length > 0 || r.available > 0;
  return {
    count: read ? String(r.available) : "—",
    usable: r.available > 0 && r.blocked === null && r.error === null,
    note: r.error ? `Check failed: ${r.error}` : r.available > 0 ? r.blocked : null,
    rows: r.credits.map((c) => ({
      label: c.label ?? "1 reset",
      left: c.total === null ? "" : `${c.left} of ${c.total} left`,
      expires: expiry(c.expires_at, now, tz),
    })),
  };
}
