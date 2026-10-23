<script lang="ts">
  import type { FlowSnapshot, ThreadView } from "../api/types.ts";
  import { ago, shortKey } from "../model/format.ts";
  import { store } from "../store.svelte.ts";

  let { thread, snapshot }: { thread: ThreadView | null; snapshot: FlowSnapshot } = $props();
  const parent = $derived(thread?.parent ? snapshot.threads.find((t) => t.key === thread.parent) : null);
  const client = $derived(thread && { claude_code: "Claude Code", codex_cli: "Codex CLI", other: "client" }[thread.client]);
</script>

<div class="panel">
  {#if thread}
    <h2>{thread.title ?? shortKey(thread.key)}</h2>
    <div class="lede">
      {client} · {thread.lineage === "root" ? "thread" : thread.lineage}
      {thread.streaming ? "· streaming now" : `· idle, last seen ${ago(thread.last_seen, snapshot.now)}`}
    </div>
    <dl class="kv">
      <dt>thread</dt><dd class="mono">{shortKey(thread.key)}</dd>
      {#if thread.parent}
        <dt>parent</dt>
        <dd>
          {#if parent}<button class="link" onclick={() => (store.selected = parent.key)}>{parent.title ?? shortKey(parent.key)}</button>{/if}
          <span class="mono faint">{shortKey(thread.parent)}</span>
        </dd>
      {/if}
      {#if thread.agent_label}<dt>agent</dt><dd class="mono">{thread.agent_label}</dd>{/if}
      <dt>machine</dt><dd>{thread.machine ?? "unknown"}</dd>
      <dt>account</dt><dd>{store.accountLabel(thread.account)}</dd>
      <dt>routing reason</dt><dd>{thread.reason ?? "not routed yet"}</dd>
      <dt>served by</dt><dd>{thread.serving_node ?? "—"}</dd>
      <dt>prompt cache</dt><dd>{thread.cache_hit_percent === null ? "no data yet" : `${Math.round(thread.cache_hit_percent)}% of input read from cache`}</dd>
      <dt>requests</dt><dd>{thread.requests}</dd>
      <dt>compactions</dt><dd>{thread.compactions || "none"}</dd>
    </dl>
  {:else}
    <h2>No thread selected</h2>
    <div class="lede">Pick an active thread to see its account, usage and lineage.</div>
  {/if}
</div>

<style>
  .link { background: none; border: 0; padding: 0; color: var(--ink); cursor: pointer; text-decoration: underline; text-decoration-color: var(--rule); text-underline-offset: 3px; }
  .link:hover { text-decoration-color: var(--muted); }
</style>
