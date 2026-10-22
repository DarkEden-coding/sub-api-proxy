<script lang="ts">
  import { onMount } from "svelte";
  import Header from "./components/Header.svelte";
  import Nav from "./components/Nav.svelte";
  import Notice from "./components/Notice.svelte";
  import AccountsPage from "./pages/AccountsPage.svelte";
  import EventsPage from "./pages/EventsPage.svelte";
  import FlowPage from "./pages/FlowPage.svelte";
  import NodesPage from "./pages/NodesPage.svelte";
  import SettingsPage from "./pages/SettingsPage.svelte";
  import ThreadsPage from "./pages/ThreadsPage.svelte";
  import { store } from "./store.svelte.ts";

  onMount(() => {
    store.listenForHash();
    store.start();
  });
</script>

<div class="shell">
  <Header />
  <Nav />
  <Notice />
  {#if store.snapshot}
    {#if store.page === "flow"}<FlowPage />
    {:else if store.page === "accounts"}<AccountsPage />
    {:else if store.page === "threads"}<ThreadsPage />
    {:else if store.page === "nodes"}<NodesPage />
    {:else if store.page === "events"}<EventsPage />
    {/if}
  {/if}
  {#if store.page === "settings"}<SettingsPage />{/if}
</div>

<style>
  .shell { max-width: 1400px; margin: 0 auto; padding: 28px 32px 48px; }
</style>
