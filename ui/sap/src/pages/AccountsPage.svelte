<script lang="ts">
  import { refreshAccounts, startLogin } from "../api/client.ts";
  import type { Provider } from "../api/types.ts";
  import AccountDetail from "../components/AccountDetail.svelte";
  import QuotaBars from "../components/QuotaBars.svelte";
  import { presentAccount } from "../model/accounts.ts";
  import { planName } from "../model/plans.ts";
  import { store } from "../store.svelte.ts";

  const snap = $derived(store.snapshot!);
  let open = $state<string | null>(null);
  let message = $state("");
  let provider = $state<Provider>("claude");
  let refreshing = $state(false);

  async function refresh() {
    refreshing = true;
    message = "";
    try {
      const result = await refreshAccounts();
      const failures = result.accounts.filter((a) => a.last_poll_error).length;
      message = failures ? `Quota refresh failed for ${failures} account(s). Check login status and try again.` : "Quota refresh complete.";
    } catch (e) {
      message = (e as Error).message;
    } finally {
      refreshing = false;
    }
  }

  async function addAccount() {
    message = "";
    try {
      const login = await startLogin({ provider, account: null });
      window.open(login.url, "_blank", "noopener");
      message = `Finish signing in in the new tab (login ${login.login_id}); the account appears here when it is done.`;
    } catch (e) {
      message = (e as Error).message;
    }
  }
</script>

<div class="page">
  <div class="head">
    <div><h2>Accounts</h2><div class="lede">subscriptions the proxy can route to</div></div>
    <div class="add">
      <button class="btn" onclick={refresh} disabled={refreshing}>{refreshing ? "Refreshing…" : "Refresh quotas"}</button>
      <select class="field" bind:value={provider}><option value="claude">Claude</option><option value="codex">OpenAI</option></select>
      <button class="btn primary" onclick={addAccount}>Add account</button>
    </div>
  </div>
  {#if message}<p class="err">{message}</p>{/if}
  <table class="table">
    <thead><tr><th>Account</th><th>Plan</th><th>State</th><th>Weight</th><th>Priority</th><th style="width:360px">Quota</th></tr></thead>
    <tbody>
      {#each snap.accounts as a (a.id)}
        {@const p = presentAccount(a, snap.now)}
        <tr class="row {a.provider}" class:greyed={p.greyed} onclick={() => (open = open === a.id ? null : a.id)}>
          <td><span class="name">{a.label}</span><div class="faint">{a.email ?? a.id}</div></td>
          <td>{planName(a.plan)}</td>
          <td>{a.state.replace("_", " ")}{#if p.flag}<div class="flag">{p.flag}</div>{/if}</td>
          <td>{p.tone === "blocked" ? "—" : `${Math.round(a.weight_percent)}%`}</td>
          <td>{a.priority}</td>
          <td><QuotaBars windows={p.windows} provider={a.provider} /></td>
        </tr>
        {#if open === a.id}
          <tr class="open"><td colspan="6"><AccountDetail account={a} onerror={(m) => (message = m)} /></td></tr>
        {/if}
      {/each}
    </tbody>
  </table>
</div>

<style>
  .head { display: flex; justify-content: space-between; align-items: flex-end; margin-bottom: 10px; }
  .add { display: flex; gap: 8px; }
  .row { cursor: pointer; }
  .row:hover { background: var(--panel); }
  .row.greyed { opacity: .6; }
  .name { font-family: var(--serif); font-size: 15px; }
  .flag { color: var(--warn); font-family: var(--serif); font-style: italic; font-size: 11.5px; }
  .open td { background: var(--panel); padding-left: 14px; }
</style>
