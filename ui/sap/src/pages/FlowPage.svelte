<script lang="ts">
  import AccountColumn from "../components/AccountColumn.svelte";
  import EventList from "../components/EventList.svelte";
  import RequestsChart from "../components/RequestsChart.svelte";
  import ServerColumn from "../components/ServerColumn.svelte";
  import ThreadColumn from "../components/ThreadColumn.svelte";
  import ThreadDetail from "../components/ThreadDetail.svelte";
  import Wires from "../components/Wires.svelte";
  import { groupThreads } from "../model/threads.ts";
  import { store } from "../store.svelte.ts";

  const snap = $derived(store.snapshot!);
  const groups = $derived(groupThreads(snap.threads));
  let host = $state<HTMLElement>();
</script>

<section class="flow" bind:this={host}>
  <Wires {host} snapshot={snap} {groups} collapsed={store.collapsed} selected={store.selectedThread} />
  <ThreadColumn {groups} />
  <div></div>
  <ServerColumn />
  <div></div>
  <AccountColumn />
</section>

<section class="lower">
  <ThreadDetail thread={store.selectedThread} snapshot={snap} />
  <RequestsChart stats={snap.stats} />
  <div class="panel">
    <h2>Recent events</h2>
    <div class="lede">routing, failover and sync</div>
    <EventList events={store.events} limit={6} />
  </div>
</section>

<style>
  .flow { position: relative; display: grid; grid-template-columns: 420px 1fr 230px 1fr 380px; align-items: start; }
  .lower { display: grid; grid-template-columns: 400px 1fr 380px; gap: 28px; margin-top: 34px; padding-top: 24px; border-top: 1px solid var(--rule); }
</style>
