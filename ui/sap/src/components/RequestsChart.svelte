<script lang="ts">
  import type { StatsView } from "../api/types.ts";
  import { bytes } from "../model/format.ts";
  import { chartPaths } from "../model/series.ts";

  let { stats }: { stats: StatsView } = $props();
  const paths = $derived(chartPaths(stats.series, 400, 150));
</script>

<div class="panel">
  <h2>Requests &amp; cache reads</h2>
  <div class="lede">last three hours, all accounts</div>
  <div class="legend">
    <span style:color="var(--ink-2)">requests</span>
    <span style:color="var(--claude)">cache-read tokens (scaled)</span>
  </div>
  <svg width="100%" height="150" viewBox="0 0 400 150" preserveAspectRatio="none">
    {#each paths.grid as y}<line x1="0" x2="400" y1={y} y2={y} stroke="#2a2520" stroke-width="1" vector-effect="non-scaling-stroke" />{/each}
    {#if paths.cacheArea}<path d={paths.cacheArea} fill="#cf8a66" fill-opacity=".08" />{/if}
    <path d={paths.cache} stroke="#cf8a66" stroke-width="1.5" fill="none" vector-effect="non-scaling-stroke" />
    <path d={paths.requests} stroke="#c9c0b1" stroke-width="1.5" fill="none" vector-effect="non-scaling-stroke" />
  </svg>
  <div class="stats">
    <div class="stat"><div class="n">{stats.requests_3h.toLocaleString("en-US")}</div><div class="l">requests</div></div>
    <div class="stat"><div class="n">{stats.cache_hit_percent_3h === null ? "—" : `${Math.round(stats.cache_hit_percent_3h)}%`}</div><div class="l">cache hit</div></div>
    <div class="stat"><div class="n">{stats.added_latency_p50_ms === null ? "—" : `${Math.round(stats.added_latency_p50_ms)} ms`}</div><div class="l">p50 added</div></div>
    <div class="stat"><div class="n">{bytes(stats.rss_bytes)}</div><div class="l">rss</div></div>
  </div>
</div>

<style>
  .legend { display: flex; gap: 16px; color: var(--faint); font-size: 11.5px; margin-bottom: 6px; }
  .legend span::before { content: ""; display: inline-block; width: 14px; height: 2px; margin-right: 6px; vertical-align: 3px; background: currentColor; }
  svg { display: block; }
  .stats { display: flex; gap: 34px; margin-top: 14px; }
  .n { font-family: var(--serif); font-size: 26px; font-variant-numeric: tabular-nums; }
  .l { color: var(--faint); font-size: 11px; letter-spacing: .6px; text-transform: uppercase; }
</style>
