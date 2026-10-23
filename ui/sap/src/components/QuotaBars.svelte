<script lang="ts">
  import type { Provider } from "../api/types.ts";
  import { level, type WindowRow } from "../model/accounts.ts";

  let { windows, provider }: { windows: WindowRow[]; provider: Provider } = $props();
</script>

<div class="bars {provider}">
  {#each windows as w (w.name)}
    <span>{w.name}</span>
    <div class="bar"><i class={level(w.left)} style:width="{w.left}%"></i></div>
    <span class="pct">{Math.round(w.left)}%</span>
    <span class="reset">{w.reset}</span>
  {:else}
    <span class="reset none">no quota data yet</span>
  {/each}
</div>

<style>
  .bars { display: grid; grid-template-columns: 30px 1fr 34px 96px; gap: 3px 8px; align-items: center; margin-top: 6px; font-size: 11px; color: var(--faint); }
  .bar { height: 4px; background: var(--rule-soft); border-radius: 2px; overflow: hidden; }
  i { display: block; height: 100%; border-radius: 2px; background: var(--tint); }
  i.warn { background: var(--warn); }
  i.bad { background: var(--bad); }
  .pct { text-align: right; color: var(--ink-2); font-variant-numeric: tabular-nums; }
  .reset { white-space: nowrap; }
  .none { grid-column: 1 / -1; font-style: italic; font-family: var(--serif); }
</style>
