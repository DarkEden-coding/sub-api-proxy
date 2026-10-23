import type { SeriesPoint } from "../api/types.ts";
import { lerp, monotonePath, type Pt } from "./curve.ts";

export interface ChartPaths {
  requests: string;
  cache: string;
  cacheArea: string;
  /** Marker positions, one per series point. */
  requestPoints: Pt[];
  cachePoints: Pt[];
  /** Gridlines, top to bottom, with the request count each one stands for. */
  grid: { y: number; label: string }[];
  bottom: number;
}

const TOP = 10;
const GRID_LINES = 4;

/** Whole-number label that stays readable for a small max (0, 1, 2 rather than 0, 0.3, 0.7). */
const tickLabel = (v: number) => (v >= 10 ? String(Math.round(v)) : String(Math.round(v * 10) / 10));

/**
 * Requests and cache-read tokens, each scaled to the chart height (cache is "scaled",
 * as the legend says), drawn as monotone curves so they never dip below zero or above the max.
 * The gridline labels are request counts.
 */
export function chartPaths(series: SeriesPoint[], width: number, height: number): ChartPaths {
  const bottom = height - 10;
  const maxReq = Math.max(1, ...series.map((p) => p.requests));
  const maxCache = Math.max(1, ...series.map((p) => p.cache_read_tokens));
  const x = (i: number) => (series.length > 1 ? (i / (series.length - 1)) * width : 0);
  const points = (value: (p: SeriesPoint) => number, max: number): Pt[] =>
    series.map((p, i) => ({ x: x(i), y: bottom - (value(p) / max) * (bottom - TOP) }));
  const requestPoints = points((p) => p.requests, maxReq);
  const cachePoints = points((p) => p.cache_read_tokens, maxCache);
  const cache = monotonePath(cachePoints);
  return {
    requests: monotonePath(requestPoints),
    cache,
    cacheArea: series.length ? `${cache} L${x(series.length - 1).toFixed(1)},${bottom} L0,${bottom} Z` : "",
    requestPoints,
    cachePoints,
    grid: Array.from({ length: GRID_LINES }, (_, i) => ({
      y: TOP + (i * (bottom - TOP)) / (GRID_LINES - 1),
      label: tickLabel(maxReq * (1 - i / (GRID_LINES - 1))),
    })),
    bottom,
  };
}

/** Series partway from one to the other, for tweening; a different length snaps to the target. */
export function blendSeries(from: SeriesPoint[], to: SeriesPoint[], t: number): SeriesPoint[] {
  if (from.length !== to.length) return to;
  return to.map((p, i) => ({
    ...p,
    requests: lerp(from[i].requests, p.requests, t),
    cache_read_tokens: lerp(from[i].cache_read_tokens, p.cache_read_tokens, t),
  }));
}
