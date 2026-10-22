<script lang="ts">
  import { fetchConflicts, resolveConflict } from "../api/client.ts";
  import type { Conflict, ConflictSide } from "../api/types.ts";
  import { ago, bytes } from "../model/format.ts";
  import { store } from "../store.svelte.ts";

  const snap = $derived(store.snapshot!);
  let conflicts = $state<Conflict[]>([]);
  let message = $state("");

  const load = () => fetchConflicts().then((c) => (conflicts = c.filter((x) => !x.resolved))).catch((e) => (message = e.message));
  $effect(() => {
    void snap.open_conflicts;
    void load();
  });

  async function resolve(c: Conflict, side: ConflictSide) {
    message = "";
    try {
      await resolveConflict(c.id, { keep: side.node });
      await load();
    } catch (e) {
      message = (e as Error).message;
    }
  }
  const show = (v: unknown) => (typeof v === "string" ? v : JSON.stringify(v));
</script>

<div class="page">
  <h2>Nodes</h2>
  <div class="lede">servers that can serve requests, and how closely they follow each other</div>
  <table class="table">
    <thead><tr><th>Node</th><th>Role</th><th>State</th><th>Requests/min</th><th>Streams</th><th>Selection p50</th><th>Sync lag</th><th>Last failover</th><th>RSS</th><th>Telemetry</th></tr></thead>
    <tbody>
      {#each snap.nodes as n (n.id)}
        <tr>
          <td><span class="dot" class:ok={n.reachable && n.state === "serving"} class:warn={n.reachable && n.state !== "serving"} class:bad={!n.reachable}></span>{n.id}{#if n.version}<span class="faint ver">v{n.version}</span>{/if}</td>
          <td>{n.role ?? "unknown"}</td>
          <td>{n.reachable ? n.state ?? "unknown" : "unreachable"}</td>
          <td>{n.requests_per_minute === null ? "—" : Math.round(n.requests_per_minute)}</td>
          <td>{n.open_streams ?? "—"}</td>
          <td>{n.added_latency_p50_ms === null ? "—" : `${Math.round(n.added_latency_p50_ms)} ms`}</td>
          <td>{n.sync_lag_secs === null ? "—" : `${n.sync_lag_secs.toFixed(1)} s`}</td>
          <td>{n.last_failover_at ? ago(n.last_failover_at, snap.now) : "never"}</td>
          <td>{bytes(n.rss_bytes)}</td>
          <td>{n.telemetry_stale ? "stale" : n.telemetry_received_at ? ago(n.telemetry_received_at, snap.now) : "unknown"}</td>
        </tr>
      {/each}
    </tbody>
  </table>

  <h2 class="sec">Sync conflicts</h2>
  <div class="lede">edits made on two nodes while they could not see each other; pick the value to keep</div>
  {#if message}<p class="err">{message}</p>{/if}
  {#each conflicts as c (c.id)}
    <div class="conflict">
      <div class="what"><b>{c.table}</b> <span class="mono">{c.key}</span> · {c.field}<span class="faint"> · found {ago(c.detected_at, snap.now)}</span></div>
      <div class="sides">
        {#each [c.applied, c.other] as side, i}
          <div class="side">
            <div class="faint">{i === 0 ? "currently applied" : "replaced"} · {side.node}</div>
            <div class="val mono">{show(side.value)}</div>
            <button class="btn" onclick={() => resolve(c, side)}>Keep this</button>
          </div>
        {/each}
      </div>
    </div>
  {:else}
    <div class="empty">No open conflicts.</div>
  {/each}
</div>

<style>
  .ver { margin-left: 8px; }
  .sec { margin-top: 36px; font-size: 19px; }
  .conflict { border-top: 1px solid var(--rule); padding: 12px 0; }
  .sides { display: flex; gap: 40px; margin-top: 8px; }
  .side { display: flex; flex-direction: column; gap: 6px; align-items: flex-start; font-size: 12px; }
  .val { color: var(--ink); }
</style>
