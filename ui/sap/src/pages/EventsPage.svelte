<script lang="ts">
  import { fetchEvents } from "../api/client.ts";
  import EventList from "../components/EventList.svelte";
  import { store } from "../store.svelte.ts";

  let message = $state("");

  // The stream appends from here on; this only loads what happened before.
  $effect(() => {
    fetchEvents(200).then((e) => store.addEvents(e), (e) => (message = e.message));
  });
</script>

<div class="page narrow">
  <h2>Events</h2>
  <div class="lede">routing, failover, compaction and sync, newest first</div>
  {#if message}<p class="err">{message}</p>{/if}
  <EventList events={store.events} />
</div>

<style>
  .narrow { max-width: 820px; }
</style>
