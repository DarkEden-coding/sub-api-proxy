/** Pure logic for the dots that travel from a streaming thread, over the server, to its account. */

export interface Line { x1: number; y1: number; x2: number; y2: number }
export interface Point { x: number; y: number }

/** A trip over two wires, named by wire key: thread to server, then server to account. */
export interface Route { from: string; to: string; color: string }
export interface Dot { route: Route; born: number }
export interface Placed { x: number; y: number; color: string; opacity: number }

/** Time to cross one wire. */
export const LEG_MS = 650;
export const SPAWN_MS = 520;
export const MAX_DOTS = 36;

/** A point on the wire's curve, `t` in [0, 1]. Same control points as the drawn path in Wires.svelte. */
export function bezierPoint(g: Line, t: number): Point {
  const mx = (g.x1 + g.x2) / 2;
  const u = 1 - t;
  const a = u * u * u, b = 3 * u * u * t, c = 3 * u * t * t, d = t * t * t;
  return { x: a * g.x1 + (b + c) * mx + d * g.x2, y: (a + b) * g.y1 + (c + d) * g.y2 };
}

/** One route per distinct pair of wires, so a collapsed group does not emit a dot per thread. */
export function uniqueRoutes(routes: Route[]): Route[] {
  return [...new Map(routes.map((r) => [routeId(r), r])).values()];
}

const routeId = (r: Route) => `${r.from}\u0000${r.to}`;

/**
 * Routes that should emit a dot now. `nextAt` holds when each route may emit next;
 * it is updated here, and routes that stopped streaming are forgotten.
 */
export function dotsDue(nextAt: Map<string, number>, routes: Route[], now: number, live: number): Route[] {
  const ids = new Set(routes.map(routeId));
  for (const id of nextAt.keys()) if (!ids.has(id)) nextAt.delete(id);
  const due: Route[] = [];
  for (const r of routes) {
    const id = routeId(r);
    if ((nextAt.get(id) ?? 0) > now) continue;
    if (live + due.length >= MAX_DOTS) break;
    nextAt.set(id, now + SPAWN_MS);
    due.push(r);
  }
  return due;
}

/**
 * Where a dot is at `now` on the wires as they are currently drawn, or null once it arrived
 * or a wire it needs is gone. Dots fade in and out over the first and last stretch.
 */
export function placeDot(dot: Dot, now: number, wire: (key: string) => Line | undefined): Placed | null {
  const age = (now - dot.born) / LEG_MS;
  if (age < 0 || age >= 2) return null;
  const leg = age < 1 ? dot.route.from : dot.route.to;
  const line = wire(leg);
  if (!line) return null;
  const p = bezierPoint(line, age < 1 ? age : age - 1);
  const opacity = Math.min(1, age / 0.2, (2 - age) / 0.2);
  return { ...p, color: dot.route.color, opacity };
}
