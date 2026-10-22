<script lang="ts">
  import type { EventView } from "../api/types.ts";
  import { clock } from "../model/format.ts";

  let { events, limit = 0 }: { events: EventView[]; limit?: number } = $props();
  const shown = $derived(limit ? events.slice(0, limit) : events);
</script>

<ul>
  {#each shown as e (e.id)}
    <li><time datetime={e.at}>{clock(e.at)}</time><div>{e.message}<span class="kind">{e.kind.replace("_", " ")} · {e.node}</span></div></li>
  {:else}
    <li class="empty">Nothing yet.</li>
  {/each}
</ul>

<style>
  ul { list-style: none; margin: 0; padding: 0; }
  li { display: grid; grid-template-columns: 44px 1fr; gap: 10px; padding: 7px 0; border-top: 1px solid var(--rule-soft); font-size: 12.5px; color: var(--ink-2); }
  time { color: var(--faint); font-family: var(--mono); font-size: 11px; padding-top: 1px; }
  .kind { display: block; color: var(--faint); font-size: 11px; margin-top: 1px; }
</style>
