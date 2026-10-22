import type { EventView } from "../api/types.ts";

export const EVENT_CAP = 500;

/**
 * Merges `incoming` into `current` (both newest first): one entry per id, an event already held
 * wins, newest first by time (arrival order breaks ties), at most `cap`. Returns `current`
 * itself when nothing new arrived.
 */
export function mergeEvents(current: EventView[], incoming: EventView[], cap = EVENT_CAP): EventView[] {
  const seen = new Set(current.map((e) => e.id));
  const fresh = incoming.filter((e) => !seen.has(e.id) && seen.add(e.id));
  if (!fresh.length) return current;
  return [...fresh, ...current].sort((a, b) => Date.parse(b.at) - Date.parse(a.at)).slice(0, cap);
}
