<script lang="ts">
  // Stacked columns, one per day, on one value axis. Each column's whole band is its
  // hover and focus target; the tooltip lists every series for that day.
  import { niceMax } from "../model/usage.ts";

  interface Series {
    label: string;
    color: string;
  }
  interface Column {
    label: string;
    /** One value per series, bottom to top. */
    values: number[];
  }
  let {
    series,
    columns,
    format,
    axisFormat = format,
    height = 190,
    total = (v: number[]) => v.reduce((a, b) => a + b, 0),
    totalLabel = "Total",
  }: {
    series: Series[];
    columns: Column[];
    format: (n: number) => string;
    /** Tick labels; defaults to `format`. */
    axisFormat?: (n: number) => string;
    height?: number;
    total?: (values: number[]) => number;
    totalLabel?: string;
  } = $props();

  const AXIS = 52;
  const TOP = 8;
  const BOTTOM = 22;
  const GAP = 2;
  let width = $state(600);
  let hover = $state<number | null>(null);

  const max = $derived(niceMax(Math.max(0, ...columns.map((c) => c.values.reduce((a, b) => a + b, 0)))));
  const plot = $derived(Math.max(1, width - AXIS));
  const band = $derived(plot / Math.max(1, columns.length));
  const bar = $derived(Math.max(3, Math.min(24, band * 0.62)));
  const base = $derived(height - BOTTOM);
  const scale = $derived((v: number) => (v / max) * (base - TOP));
  const ticks = $derived([0, 0.5, 1].map((f) => ({ v: max * f, y: base - f * (base - TOP) })));
  // Label every nth day so labels never collide.
  const every = $derived(Math.max(1, Math.ceil(46 / band)));

  /** Segments bottom-up; slivers under 1px are skipped and the top one gets rounded corners. */
  function stack(values: number[], x: number) {
    let y = base;
    const out: { d: string; color: string }[] = [];
    const drawn = values.map((v, i) => ({ h: scale(v), color: series[i].color })).filter((s) => s.h >= 1);
    drawn.forEach((s, i) => {
      const last = i === drawn.length - 1;
      const h = Math.max(1, s.h - (last ? 0 : GAP));
      const top = y - h;
      const r = last ? Math.min(4, h, bar / 2) : 0;
      out.push({
        d: `M${x},${y} V${top + r} Q${x},${top} ${x + r},${top} H${x + bar - r} Q${x + bar},${top} ${x + bar},${top + r} V${y} Z`,
        color: s.color,
      });
      y -= s.h;
    });
    return out;
  }
  const hit = $derived(hover === null ? null : columns[hover]);
  const tipX = $derived(hover === null ? 0 : AXIS + hover * band + band / 2);
</script>

<div class="chart" bind:clientWidth={width}>
  <svg {width} {height} viewBox="0 0 {width} {height}" role="group" aria-label="Daily columns" onpointerleave={() => (hover = null)}>
    {#each ticks as t}
      <line x1={AXIS} x2={width} y1={t.y} y2={t.y} class="grid" />
      <text x={AXIS - 8} y={t.y + 3.5} class="tick" text-anchor="end">{axisFormat(t.v)}</text>
    {/each}
    {#each columns as c, i}
      {@const x = AXIS + i * band + (band - bar) / 2}
      {#if hover === i}<rect x={AXIS + i * band} y={TOP} width={band} height={base - TOP} class="lift" />{/if}
      {#each stack(c.values, x) as s}<path d={s.d} fill={s.color} />{/each}
      {#if i % every === (columns.length - 1) % every}
        <text x={AXIS + i * band + band / 2} y={height - 6} class="tick" text-anchor="middle">{c.label}</text>
      {/if}
      <!-- Focusable so keyboard readers get the same tooltip as the pointer. -->
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <rect
        x={AXIS + i * band}
        y={0}
        width={band}
        height={base}
        fill="transparent"
        tabindex="0"
        role="img"
        aria-label={`${c.label}: ${series.map((s, j) => `${s.label} ${format(c.values[j])}`).join(", ")}`}
        onpointerenter={() => (hover = i)}
        onfocus={() => (hover = i)}
        onblur={() => (hover = null)}
      />
    {/each}
  </svg>
  {#if hit && hover !== null}
    <div class="tip" class:left={tipX > width * 0.65} style:left="{tipX}px">
      <div class="when">{hit.label}</div>
      {#if hit.values.some((v) => v > 0)}
        {#each [...series.keys()].reverse() as j}
          <div class="row"><i style:background={series[j].color}></i><b>{format(hit.values[j])}</b><span>{series[j].label}</span></div>
        {/each}
        <div class="row sum"><i></i><b>{format(total(hit.values))}</b><span>{totalLabel}</span></div>
      {:else}
        <div>No requests</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .chart { position: relative; }
  svg { display: block; overflow: visible; }
  .grid { stroke: var(--rule-soft); stroke-width: 1; }
  .tick { fill: var(--faint); font-size: 10.5px; font-variant-numeric: tabular-nums; pointer-events: none; }
  .lift { fill: var(--panel-2); }
  rect[tabindex]:focus { outline: none; }
  rect[tabindex]:focus-visible { outline: 1px solid var(--muted); }
  .tip { position: absolute; top: 0; transform: translateX(14px); pointer-events: none; background: var(--panel-2); border: 1px solid var(--rule); border-radius: 6px; padding: 8px 11px; font-size: 12px; color: var(--muted); white-space: nowrap; z-index: 2; box-shadow: 0 6px 18px rgb(0 0 0 / 0.35); }
  .tip.left { transform: translateX(calc(-100% - 14px)); }
  .when { font-family: var(--serif); font-style: italic; margin-bottom: 4px; }
  .row { display: grid; grid-template-columns: 14px auto auto; gap: 8px; align-items: center; line-height: 1.6; }
  .row i { width: 12px; height: 2px; border-radius: 1px; }
  .row b { color: var(--ink); font-weight: 500; font-variant-numeric: tabular-nums; text-align: right; }
  .sum { border-top: 1px solid var(--rule); margin-top: 3px; padding-top: 3px; }
</style>
