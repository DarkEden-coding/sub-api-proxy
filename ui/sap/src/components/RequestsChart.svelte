<script lang="ts">
  import { untrack } from "svelte";
  import type { StatsView } from "../api/types.ts";
  import { easeOutCubic } from "../model/curve.ts";
  import { bytes, clock } from "../model/format.ts";
  import { blendSeries, chartPaths } from "../model/series.ts";

  let { stats }: { stats: StatsView } = $props();

  const HEIGHT = 150;
  const TWEEN_MS = 600;
  const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");

  let width = $state(400);
  // The series being drawn; it eases toward stats.series when new data arrives.
  let shown = $state.raw(untrack(() => stats.series));
  let hover = $state<number | null>(null);

  $effect(() => {
    const to = stats.series;
    const from = untrack(() => shown);
    if (from === to) return;
    if (reduceMotion.matches || from.length !== to.length) {
      shown = to;
      return;
    }
    const began = performance.now();
    let frame = requestAnimationFrame(function step(now) {
      const t = Math.min(1, (now - began) / TWEEN_MS);
      shown = t < 1 ? blendSeries(from, to, easeOutCubic(t)) : to;
      if (t < 1) frame = requestAnimationFrame(step);
    });
    return () => cancelAnimationFrame(frame);
  });

  const paths = $derived(chartPaths(shown, width, HEIGHT));
  const hit = $derived(hover === null ? null : stats.series[hover] ?? null);

  function move(e: PointerEvent) {
    const n = stats.series.length;
    if (!n) return;
    const box = (e.currentTarget as Element).getBoundingClientRect();
    const frac = (e.clientX - box.left) / box.width;
    hover = Math.min(n - 1, Math.max(0, Math.round(frac * (n - 1))));
  }
</script>

<div class="panel">
  <h2>Requests &amp; cache reads</h2>
  <div class="lede">last three hours, all accounts</div>
  <div class="legend">
    <span style:color="var(--ink-2)">requests</span>
    <span style:color="var(--claude)">cache-read tokens (scaled)</span>
  </div>
  <div class="plot" bind:clientWidth={width}>
    <svg {width} height={HEIGHT} viewBox="0 0 {width} {HEIGHT}" role="img" aria-label="Requests and cache-read tokens over the last three hours" onpointermove={move} onpointerleave={() => (hover = null)}>
      <defs>
        <linearGradient id="cache-fill" x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stop-color="#cf8a66" stop-opacity=".26" />
          <stop offset="1" stop-color="#cf8a66" stop-opacity="0" />
        </linearGradient>
      </defs>
      {#each paths.grid as g}
        <line x1="0" x2={width} y1={g.y} y2={g.y} stroke="#2a2520" stroke-width="1" />
        <text x="0" y={g.y - 3} class="tick">{g.label}</text>
      {/each}
      {#if paths.cacheArea}<path d={paths.cacheArea} fill="url(#cache-fill)" />{/if}
      <path d={paths.cache} stroke="#cf8a66" stroke-width="1.5" fill="none" stroke-linejoin="round" stroke-linecap="round" />
      <path d={paths.requests} stroke="#c9c0b1" stroke-width="1.5" fill="none" stroke-linejoin="round" stroke-linecap="round" />
      {#if hover !== null && paths.requestPoints[hover]}
        {@const r = paths.requestPoints[hover]}
        {@const c = paths.cachePoints[hover]}
        <line x1={r.x} x2={r.x} y1="0" y2={paths.bottom} stroke="#37312a" stroke-width="1" />
        <circle cx={c.x} cy={c.y} r="3" fill="#171513" stroke="#cf8a66" stroke-width="1.5" />
        <circle cx={r.x} cy={r.y} r="3" fill="#171513" stroke="#c9c0b1" stroke-width="1.5" />
      {/if}
    </svg>
    {#if hit && hover !== null && paths.requestPoints[hover]}
      {@const x = paths.requestPoints[hover].x}
      <div class="tip" class:left={x > width / 2} style:left="{x}px">
        <div class="when">{clock(hit.start)}</div>
        <div><span class="k">requests</span> {hit.requests.toLocaleString("en-US")}</div>
        <div><span class="k claude-text">cache read</span> {hit.cache_read_tokens.toLocaleString("en-US")}</div>
      </div>
    {/if}
  </div>
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
  .plot { position: relative; }
  svg { display: block; touch-action: none; }
  .tick { fill: var(--faint); font-size: 10px; font-variant-numeric: tabular-nums; pointer-events: none; }
  .tip { position: absolute; top: 4px; transform: translateX(10px); pointer-events: none; background: var(--panel-2); border: 1px solid var(--rule); border-radius: 5px; padding: 6px 9px; font-size: 11.5px; color: var(--ink-2); white-space: nowrap; font-variant-numeric: tabular-nums; }
  .tip.left { transform: translateX(calc(-100% - 10px)); }
  .when { font-family: var(--serif); font-style: italic; color: var(--muted); margin-bottom: 2px; }
  .k { color: var(--faint); margin-right: 4px; }
  .claude-text { color: var(--claude); }
  .stats { display: flex; gap: 34px; margin-top: 14px; }
  .n { font-family: var(--serif); font-size: 26px; font-variant-numeric: tabular-nums; }
  .l { color: var(--faint); font-size: 11px; letter-spacing: .6px; text-transform: uppercase; }
</style>
