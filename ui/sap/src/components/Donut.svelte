<script lang="ts">
  // A ring of shares with its legend as a small table: colour key, name, value, share.
  // Hovering a slice or a legend row shows that part in the middle.
  import { arc, percent, slices } from "../model/usage.ts";

  interface Part {
    label: string;
    value: number;
    color: string;
  }
  let { parts, format, caption }: { parts: Part[]; format: (n: number) => string; caption: string } = $props();

  const SIZE = 140;
  const R = SIZE / 2;
  const INNER = R - 19;
  let hover = $state<number | null>(null);

  const total = $derived(parts.reduce((a, p) => a + p.value, 0));
  const arcs = $derived(slices(parts.map((p) => p.value)));
  const focus = $derived(hover === null ? null : parts[hover]);
</script>

<div class="donut">
  <svg width={SIZE} height={SIZE} viewBox="0 0 {SIZE} {SIZE}" role="img" aria-label={`${caption}: ${parts.map((p) => `${p.label} ${format(p.value)}`).join(", ")}`} onpointerleave={() => (hover = null)}>
    {#if arcs.length}
      {#each arcs as a, i}
        {#if a.share > 0}
          <path
            d={arc(R, R, hover === i ? R : R - 3, INNER, a.a0, a.a1)}
            fill={parts[i].color}
            class="slice"
            class:dim={hover !== null && hover !== i}
            role="presentation"
            onpointerenter={() => (hover = i)}
          />
        {/if}
      {/each}
    {:else}
      <circle cx={R} cy={R} r={(R - 3 + INNER) / 2} fill="none" stroke="var(--rule)" stroke-width={R - 3 - INNER} />
    {/if}
    <text x={R} y={R - 4} text-anchor="middle" class="big">{focus ? percent(total ? focus.value / total : 0) : format(total)}</text>
    <text x={R} y={R + 15} text-anchor="middle" class="small">{focus ? focus.label : caption}</text>
  </svg>
  <table>
    <tbody>
      {#each parts as p, i}
        <tr class:on={hover === i} onpointerenter={() => (hover = i)} onpointerleave={() => (hover = null)}>
          <td><i style:background={p.color}></i>{p.label}</td>
          <td class="n">{format(p.value)}</td>
          <td class="n faint">{percent(total ? p.value / total : 0)}</td>
        </tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .donut { display: flex; align-items: center; gap: 22px; }
  svg { flex: none; overflow: visible; }
  .slice { stroke: var(--panel); stroke-width: 2; transition: opacity 0.15s; }
  .slice.dim { opacity: 0.35; }
  .big { fill: var(--ink); font-family: var(--serif); font-size: 19px; }
  .small { fill: var(--faint); font-size: 11px; }
  table { border-collapse: collapse; flex: 1; font-size: 12.5px; }
  td { padding: 5px 0; color: var(--ink-2); }
  tr.on td { color: var(--ink); }
  td i { display: inline-block; width: 9px; height: 9px; border-radius: 2px; margin-right: 9px; vertical-align: -0.5px; }
  .n { text-align: right; font-variant-numeric: tabular-nums; padding-left: 14px; }
</style>
