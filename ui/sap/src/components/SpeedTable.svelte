<script lang="ts">
  import type { UsageSpeed, UsageSpeedFigures } from "../api/types.ts";
  import { latency, rate } from "../model/usage.ts";
  import { store } from "../store.svelte.ts";

  let { rows }: { rows: UsageSpeed[] } = $props();

  /** Expanded rows, by `provider|account`. Everything starts collapsed. */
  let open = $state<Record<string, boolean>>({});

  const key = (r: UsageSpeed) => `${r.provider}|${r.account}`;
  const label = (account: string) => (account ? store.accountLabel(account) : "unknown account");
</script>

{#snippet figures(f: UsageSpeedFigures)}
  <td class="n">{f.requests.toLocaleString("en-US")}</td>
  <td class="n">{latency(f.ttft_ms?.p50)}</td>
  <td class="n faint">{latency(f.ttft_ms?.p90)}</td>
  <td class="n">{rate(f.tps?.p50)}</td>
  <td class="n faint">{rate(f.tps?.p90)}</td>
{/snippet}

<table class="table speed">
  <thead>
    <tr>
      <th>Account</th><th class="n">Requests</th>
      <th class="n">TTFT p50</th><th class="n">TTFT p90</th>
      <th class="n">Speed p50</th><th class="n">Speed p90</th>
    </tr>
  </thead>
  <tbody>
    {#each rows as r (key(r))}
      {@const expanded = open[key(r)] ?? false}
      <tr class="group">
        <td>
          <button class="toggle" aria-expanded={expanded} onclick={() => (open[key(r)] = !expanded)}>
            <span class="chev" class:down={expanded}>›</span>
            <span>{label(r.account)}</span>
            <span class="faint prov">{r.provider}</span>
          </button>
        </td>
        {@render figures(r)}
      </tr>
      {#if expanded}
        {#each r.models as m (m.model)}
          <tr class="model">
            <td><span class="mono">{m.model}</span></td>
            {@render figures(m)}
          </tr>
        {/each}
      {/if}
    {/each}
  </tbody>
</table>

<style>
  .speed td { padding-top: 11px; padding-bottom: 11px; }
  .speed .n { text-align: right; font-variant-numeric: tabular-nums; }
  .toggle { all: unset; cursor: pointer; display: inline-flex; align-items: baseline; gap: 8px; }
  .toggle:focus-visible { outline: 1px solid var(--muted); outline-offset: 3px; border-radius: 3px; }
  .chev { display: inline-block; width: 10px; color: var(--faint); transition: transform 0.12s; }
  .chev.down { transform: rotate(90deg); }
  .prov { font-size: 11.5px; }
  .model td { background: var(--panel-2); }
  .model td:first-child { padding-left: 18px; }
</style>
