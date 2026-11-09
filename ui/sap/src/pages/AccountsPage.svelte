<script lang="ts">
  import { refreshAccounts, startLogin } from "../api/client.ts";
  import type { AccountView, Provider } from "../api/types.ts";
  import AccountDetail from "../components/AccountDetail.svelte";
  import QuotaBars from "../components/QuotaBars.svelte";
  import { presentAccount } from "../model/accounts.ts";
  import { presentResets } from "../model/resets.ts";
  import { planName } from "../model/plans.ts";
  import { store } from "../store.svelte.ts";
  import { browserLogin, DEFAULT_HELPER, isLoopback } from "../model/oauth.ts";

  const snap = $derived(store.snapshot!);
  let open = $state<string | null>(null);
  let message = $state("");
  let provider = $state<Provider>("claude");
  let refreshing = $state(false);
  const localDefault = isLoopback(location.hostname) && location.port === "8317" ? location.origin : DEFAULT_HELPER;
  let helper = $state(localStorage.getItem("sap-oauth-helper") ?? localDefault);
  let signingIn = $state(false);

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

  async function signIn(selected: Provider, account: string | null = null) {
    if (signingIn) return;
    message = "";
    signingIn = true;
    try {
      await browserLogin(() => startLogin({ provider: selected, account, callback_relay: true }), helper);
      localStorage.setItem("sap-oauth-helper", helper);
      message = "Finish signing in in the new tab. The account will appear here when it is ready.";
    } catch (e) {
      message = (e as Error).message;
    } finally {
      signingIn = false;
    }
  }
</script>

<div class="page accounts-page">
  <div class="head">
    <div><h2>Accounts</h2><div class="lede">subscriptions the proxy can route to</div></div>
    <div class="add">
      <button class="btn" onclick={refresh} disabled={refreshing}>{refreshing ? "Refreshing…" : "Refresh quotas"}</button>
      <select class="field" bind:value={provider}><option value="claude">Claude</option><option value="codex">OpenAI</option></select>
      <button class="btn primary" disabled={signingIn} onclick={() => signIn(provider)}>{signingIn ? "Opening sign-in…" : "Add account"}</button>
    </div>
  </div>
  {#if message}<p class="err">{message}</p>{/if}
  <label class="helper">Local sign-in helper
    <input class="field" type="url" bind:value={helper} placeholder={DEFAULT_HELPER} />
  </label>
  <div class="table-scroll"><table class="table">
    <thead><tr><th>Account</th><th>Plan</th><th>State</th><th>Weight</th><th>Priority</th><th>Resets</th><th style="width:360px">Quota</th></tr></thead>
    <tbody>
      {#each snap.accounts as a (a.id)}
        {@const p = presentAccount(a, snap.now)}
        {@const r = presentResets(a.resets, snap.now)}
        <tr class="row {a.provider}" class:greyed={p.greyed} onclick={() => (open = open === a.id ? null : a.id)}>
          <td><span class="name">{a.label}</span><div class="faint">{a.email ?? a.id}</div></td>
          <td>{planName(a.plan)}</td>
          <td>{a.state.replace("_", " ")}{#if p.flag}<div class="flag">{p.flag}</div>{/if}</td>
          <td>{p.tone === "blocked" ? "—" : `${Math.round(a.weight_percent)}%`}</td>
          <td>{a.priority}</td>
          <td class:usable={r?.usable}>{r?.count ?? "—"}</td>
          <td><QuotaBars windows={p.windows} provider={a.provider} /></td>
        </tr>
        {#if open === a.id}
          <tr class="open"><td colspan="7"><AccountDetail account={a} now={snap.now} onerror={(m) => (message = m)} onnotice={(m) => (message = m)} onlogin={(account: AccountView) => signIn(account.provider, account.id)} /></td></tr>
        {/if}
      {/each}
    </tbody>
  </table></div>
</div>

<style>
  .accounts-page { max-width: calc(100vw - 64px); }
  .head { display: flex; justify-content: space-between; align-items: flex-end; flex-wrap: wrap; gap: 12px; margin-bottom: 10px; }
  .add { display: flex; flex-wrap: wrap; gap: 8px; }
  .table-scroll { overflow-x: auto; }
  .helper { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-bottom: 12px; color: var(--muted); }
  .helper input { width: 240px; }
  .row { cursor: pointer; }
  .row:hover { background: var(--panel); }
  .row.greyed { opacity: .6; }
  .name { font-family: var(--serif); font-size: 15px; }
  .usable { color: var(--tint); font-weight: 600; }
  .flag { color: var(--warn); font-family: var(--serif); font-style: italic; font-size: 11.5px; }
  .open td { background: var(--panel); padding-left: 14px; }
</style>
