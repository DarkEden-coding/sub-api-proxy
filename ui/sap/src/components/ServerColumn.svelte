<script lang="ts">
  import { ago, bytes } from "../model/format.ts";
  import { store } from "../store.svelte.ts";

  const snap = $derived(store.snapshot!);
  const standby = (n: (typeof snap.nodes)[number]) => n.role === "fallback" && n.requests_per_minute === 0;
</script>

<div class="col">
  <h2>Servers</h2>
  <div class="lede">Tailscale · local router on each client</div>
  <div class="servers">
    {#each snap.nodes as n (n.id)}
      <div class="server" class:primary={n.role === "primary"} class:standby={standby(n)} class:down={!n.reachable} data-node={n.id}>
        <div class="sname"><span class="dot" class:ok={n.reachable && n.state === "serving"} class:warn={n.reachable && n.state !== "serving"} class:bad={!n.reachable}></span>{n.id}</div>
        <div class="role">{n.role} · {n.reachable ? n.state : "unreachable"}{n.version ? ` · v${n.version}` : ""}</div>
        <dl>
          <dt>requests/min</dt><dd>{Math.round(n.requests_per_minute)}</dd>
          <dt>open streams</dt><dd>{n.open_streams}</dd>
          {#if n.added_latency_p50_ms !== null}<dt>added p50</dt><dd>{Math.round(n.added_latency_p50_ms)} ms</dd>{/if}
          {#if n.sync_lag_secs !== null && n.id !== snap.node}<dt>sync lag</dt><dd>{n.sync_lag_secs.toFixed(1)} s</dd>{/if}
          {#if n.last_failover_at}<dt>last failover</dt><dd>{ago(n.last_failover_at, snap.now)}</dd>{/if}
          <dt>rss</dt><dd>{bytes(n.rss_bytes)}</dd>
        </dl>
      </div>
    {/each}
  </div>
</div>

<style>
  .col { text-align: left; }
  h2, .lede { text-align: center; }
  .servers { display: flex; flex-direction: column; gap: 18px; padding-top: 38px; }
  .server { border: 1px solid var(--rule); border-radius: 8px; padding: 14px 16px; background: var(--panel); }
  .server.primary { border-color: #4a4136; }
  .server.standby { opacity: .78; }
  .server.down { opacity: .5; filter: grayscale(1); }
  .sname { font-family: var(--serif); font-size: 17px; }
  .role { color: var(--muted); font-size: 11.5px; margin: 2px 0 10px; }
  dl { display: grid; grid-template-columns: auto 1fr; gap: 4px 12px; margin: 0; font-size: 12px; }
  dt { color: var(--faint); }
  dd { margin: 0; text-align: right; color: var(--ink-2); font-variant-numeric: tabular-nums; }
</style>
