<script lang="ts">
  import { store } from "../store.svelte.ts";
  import AccountRow from "./AccountRow.svelte";

  const providers = [
    { id: "claude", name: "Claude" },
    { id: "codex", name: "OpenAI" },
  ] as const;
</script>

<div class="col">
  <h2>Accounts</h2>
  <div class="lede">weights follow weekly quota left ÷ time to reset · accounts within 36 h of reset take every new thread · greyed accounts take none</div>
  {#each providers as p (p.id)}
    {@const list = store.accountsOf(p.id)}
    {#if list.length}
      <div class="provblock">
        <div class="prov"><span>{p.name}</span><span>weight</span></div>
        {#each list as a (a.id)}<AccountRow account={a} now={store.snapshot!.now} />{/each}
      </div>
    {/if}
  {/each}
</div>

<style>
  .provblock { margin-bottom: 16px; }
  .prov { font-size: 11px; letter-spacing: 1.2px; text-transform: uppercase; color: var(--muted); margin: 4px 0 6px; display: flex; justify-content: space-between; }
</style>
