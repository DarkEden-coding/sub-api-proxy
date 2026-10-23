export interface Pt {
  x: number;
  y: number;
}

/**
 * Fritsch-Carlson slopes for a monotone cubic through the points (x strictly increasing).
 * Where the data changes direction the slope is zero, so the curve never leaves the range
 * of its neighbouring values.
 */
export function monotoneSlopes(pts: Pt[]): number[] {
  const n = pts.length;
  if (n < 2) return pts.map(() => 0);
  const dx = pts.slice(1).map((p, i) => p.x - pts[i].x);
  const secant = pts.slice(1).map((p, i) => (p.y - pts[i].y) / dx[i]);
  const m = pts.map((_, i) => {
    if (i === 0) return secant[0];
    if (i === n - 1) return secant[n - 2];
    return secant[i - 1] * secant[i] <= 0 ? 0 : (secant[i - 1] + secant[i]) / 2;
  });
  secant.forEach((s, i) => {
    if (s === 0) {
      m[i] = 0;
      m[i + 1] = 0;
      return;
    }
    const a = m[i] / s;
    const b = m[i + 1] / s;
    const h = Math.hypot(a, b);
    if (h > 3) {
      m[i] = (3 * a * s) / h;
      m[i + 1] = (3 * b * s) / h;
    }
  });
  return m;
}

const fmt = (n: number) => n.toFixed(1);

/** SVG path "M…C…" through the points as a monotone cubic; a lone point yields a bare "M". */
export function monotonePath(pts: Pt[]): string {
  if (!pts.length) return "";
  const m = monotoneSlopes(pts);
  let d = `M${fmt(pts[0].x)},${fmt(pts[0].y)}`;
  for (let i = 1; i < pts.length; i++) {
    const a = pts[i - 1];
    const b = pts[i];
    const w = (b.x - a.x) / 3;
    d += ` C${fmt(a.x + w)},${fmt(a.y + m[i - 1] * w)} ${fmt(b.x - w)},${fmt(b.y - m[i] * w)} ${fmt(b.x)},${fmt(b.y)}`;
  }
  return d;
}

export const lerp = (a: number, b: number, t: number) => a + (b - a) * t;
export const easeOutCubic = (t: number) => 1 - (1 - t) ** 3;
