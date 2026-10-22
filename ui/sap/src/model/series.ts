import type { SeriesPoint } from "../api/types.ts";

export interface ChartPaths {
  requests: string;
  cache: string;
  cacheArea: string;
  grid: number[];
}

/**
 * Requests and cache-read tokens, each scaled to the chart height (cache is "scaled",
 * as the legend says). The chart is a width×height box with a small top and bottom margin.
 */
export function chartPaths(series: SeriesPoint[], width: number, height: number): ChartPaths {
  const top = 10;
  const bottom = height - 10;
  const maxReq = Math.max(1, ...series.map((p) => p.requests));
  const maxCache = Math.max(1, ...series.map((p) => p.cache_read_tokens));
  const x = (i: number) => (series.length > 1 ? (i / (series.length - 1)) * width : 0);
  const line = (value: (p: SeriesPoint) => number, max: number) =>
    series
      .map((p, i) => `${i ? "L" : "M"}${x(i).toFixed(1)},${(bottom - (value(p) / max) * (bottom - top)).toFixed(1)}`)
      .join(" ");
  const cache = line((p) => p.cache_read_tokens, maxCache);
  return {
    requests: line((p) => p.requests, maxReq),
    cache,
    cacheArea: series.length ? `${cache} L${width},${bottom} L0,${bottom} Z` : "",
    grid: [0, 1, 2, 3].map((i) => top + (i * (bottom - top)) / 3),
  };
}
