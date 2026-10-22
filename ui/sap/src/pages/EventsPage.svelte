<script lang="ts">
  import { fetchEvents } from "../api/client.ts";
  import type { EventView } from "../api/types.ts";
  import EventList from "../components/EventList.svelte";
  import { store } from "../store.svelte.ts";

  let events = $state<EventView[]>([]);
  let message = $state("");
  const newest = $derived(store.snapshot?.events[0]?.id);

  $effect(() => {
    void newest;
    fetchEvents(200).then((e) => (events = e), (e) => (message = e.message));
  });
</script>

<div class="page narrow">
  <h2>Events</h2>
  <div class="lede">routing, failover, compaction and sync, newest first</div>
  {#if message}<p class="err">{message}</p>{/if}
  <EventList {events} />
</div>

<style>
  .narrow { max-width: 820px; }
</style>
