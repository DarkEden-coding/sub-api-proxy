<script lang="ts">
  import { dayKey, headerClock } from "../model/format.ts";
  import { store } from "../store.svelte.ts";

  const snap = $derived(store.snapshot);
  const serving = $derived.by(() => {
    if (!snap) return null;
    const busiest = [...snap.nodes].sort((a, b) => (b.open_streams ?? -1) - (a.open_streams ?? -1))[0];
    return snap.nodes.find((n) => n.id === snap.node && n.state === "serving") ?? busiest;
  });
  const lag = $derived(Math.max(0, ...(snap?.nodes.map((n) => n.sync_lag_secs ?? 0) ?? [])));
  const failovers = $derived(
    snap ? snap.events.filter((e) => e.kind === "failover" && dayKey(e.at) === dayKey(snap.now)).length : 0,
  );
  const tone = $derived(store.status === "live" || store.status === "demo" ? "ok" : store.status === "connecting" ? "warn" : "bad");
</script>

<header>
  <div class="brand">
    <h1>Sub-Api-Proxy</h1>
    <span class="sub">subscription routing for coding agents</span>
  </div>
  <div class="status">
    {#if store.status === "demo"}<span class="demo">demo data</span>{/if}
    {#if serving}
      <span><span class="dot {tone}"></span>serving from <b>{serving.id}</b></span>
      <span>sync lag <b>{lag.toFixed(1)} s</b></span>
      <span>failovers today <b>{failovers}</b></span>
    {:else}
      <span><span class="dot {tone}"></span>{store.status}</span>
    {/if}
    {#if snap}<span><b>{headerClock(snap.now)}</b></span>{/if}
  </div>
</header>

<style>
  header { display: flex; align-items: flex-end; justify-content: space-between; padding-bottom: 18px; border-bottom: 1px solid var(--rule); }
  .brand { display: flex; align-items: baseline; gap: 16px; }
  h1 { font-size: 30px; letter-spacing: -0.2px; }
  .sub { color: var(--muted); font-family: var(--serif); font-style: italic; font-size: 15px; }
  .status { display: flex; gap: 22px; color: var(--muted); font-size: 12px; }
  .status b { color: var(--ink-2); font-weight: 500; }
  .demo { color: var(--warn); font-style: italic; font-family: var(--serif); }
</style>
