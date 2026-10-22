<script lang="ts">
  import { fetchHistory, patchAccount, startLogin } from "../api/client.ts";
  import type { AccountView, HistoryPoint } from "../api/types.ts";
  import { planLabel } from "../model/format.ts";
  import History from "./History.svelte";

  let { account, onerror }: { account: AccountView; onerror: (message: string) => void } = $props();

  let override = $state("");
  let history = $state<HistoryPoint[]>([]);

  $effect(() => {
    const id = account.id;
    void fetchHistory(id, 24).then((h) => (history = h.samples)).catch(() => (history = []));
  });

  async function run(job: Promise<unknown>) {
    try {
      await job;
    } catch (e) {
      onerror((e as Error).message);
    }
  }

  const relogin = () =>
    run(startLogin({ provider: account.provider, account: account.id }).then((s) => window.open(s.url, "_blank", "noopener")));
</script>

<div class="detail">
  <dl class="kv">
    <dt>email</dt><dd>{account.email ?? "—"}</dd>
    <dt>plan</dt><dd>{planLabel(account.plan) || "unknown"}</dd>
    <dt>capacity</dt><dd>{account.capacity_multiplier}× <span class="faint">({account.capacity_source})</span></dd>
    <dt>active threads</dt><dd>{account.active_threads}</dd>
  </dl>
  <div class="edit">
    <label>Label <input class="field label-input" value={account.label} onchange={(e) => run(patchAccount(account.id, { label: e.currentTarget.value }))} /></label>
    <label>Priority <input class="field num" type="number" value={account.priority} onchange={(e) => run(patchAccount(account.id, { priority: Number(e.currentTarget.value) }))} /></label>
    <label>
      Capacity override
      <input class="field num" type="number" min="0" step="0.1" placeholder={account.capacity_source === "override" ? String(account.capacity_multiplier) : "auto"} bind:value={override} />
      <button class="btn" disabled={override === ""} onclick={() => run(patchAccount(account.id, { capacity_override: Number(override) }))}>Set</button>
      <button class="btn" disabled={account.capacity_source !== "override"} onclick={() => { override = ""; void run(patchAccount(account.id, { capacity_override: null })); }}>Clear</button>
    </label>
    <div class="actions">
      <button class="btn" onclick={() => run(patchAccount(account.id, { disabled: account.state !== "disabled" }))}>
        {account.state === "disabled" ? "Enable" : "Disable"}
      </button>
      <button class="btn" onclick={relogin}>Sign in again</button>
    </div>
  </div>
  <div class="hist"><h3>Last 24 hours</h3>{#if history.length}<History samples={history} />{:else}<div class="empty">No samples.</div>{/if}</div>
</div>

<style>
  .detail { display: grid; grid-template-columns: 300px 1fr 320px; gap: 28px; padding: 14px 0 16px; }
  .edit { display: flex; flex-direction: column; gap: 9px; font-size: 12px; color: var(--faint); }
  label { display: flex; align-items: center; gap: 8px; }
  
  label { min-width: 0; }
  .label-input { flex: 1; }
  .num { width: 72px; }
  .actions { display: flex; gap: 8px; margin-top: 4px; }
  h3 { font-size: 15px; margin-bottom: 8px; }
</style>
