<script lang="ts">
  import { isDemo, managementKey, setManagementKey } from "../api/client.ts";
  import { store } from "../store.svelte.ts";

  let key = $state(managementKey());
  const save = () => {
    setManagementKey(key.trim());
    store.start();
  };
</script>

<div class="page narrow">
  <h2>Settings</h2>
  <div class="lede">this browser only</div>
  <h3>Management key</h3>
  <p class="faint">Sent as a bearer token with every request to <span class="mono">/sap/api</span>. It stays in this browser's local storage.</p>
  <form onsubmit={(e) => { e.preventDefault(); save(); }}>
    <input class="field" type="password" autocomplete="off" placeholder="management key" bind:value={key} />
    <button class="btn primary" type="submit">Save</button>
    <button class="btn" type="button" onclick={() => { key = ""; save(); }}>Forget</button>
  </form>
  <p class="status">
    Connection: <b>{store.status}</b>{#if store.error} <span class="err">· {store.error}</span>{/if}
    {#if isDemo()}<br /><span class="faint">Demo mode uses the sample data from sap-core and changes nothing on a server.</span>{/if}
  </p>
</div>

<style>
  .narrow { max-width: 620px; }
  h3 { font-size: 16px; margin-top: 18px; }
  form { display: flex; gap: 8px; }
  form .field { flex: 1; }
  .status { margin-top: 24px; color: var(--ink-2); }
</style>
