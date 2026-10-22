<script lang="ts">
  import { store } from "../store.svelte.ts";
</script>

{#if store.status === "unauthorized"}
  <p class="notice">The server refused the management key. <a href="#/settings">Enter it in Settings.</a></p>
{:else if store.status === "offline"}
  <p class="notice">Cannot reach the server{store.error ? ` (${store.error})` : ""}. Showing the last data; retrying.</p>
{:else if store.status === "connecting" && !store.snapshot}
  <p class="notice">Connecting…</p>
{/if}

<style>
  .notice { border: 1px solid var(--rule); border-left: 2px solid var(--warn); background: var(--panel); padding: 10px 14px; margin: 0 0 20px; color: var(--ink-2); font-family: var(--serif); }
</style>
