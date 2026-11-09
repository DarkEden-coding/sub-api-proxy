<script lang="ts">
  import { fetchHistory, patchAccount, redeemReset } from "../api/client.ts";
  import type { AccountView, HistoryPoint } from "../api/types.ts";
  import { planName } from "../model/plans.ts";
  import { presentResets } from "../model/resets.ts";
  import History from "./History.svelte";

  let { account, now, onerror, onnotice, onlogin }: {
    account: AccountView;
    now: string;
    onerror: (message: string) => void;
    onnotice: (message: string) => void;
    onlogin: (account: AccountView) => void;
  } = $props();

  let override = $state("");
  let history = $state<HistoryPoint[]>([]);
  let redeeming = $state(false);
  const resets = $derived(presentResets(account.resets, now));

  $effect(() => {
    const id = account.id;
    // A new quota observation adds a history sample, so refetch when one arrives.
    void account.windows.map((w) => w.observed_at).join();
    void fetchHistory(id, 24).then((h) => (history = h.samples)).catch(() => (history = []));
  });

  async function run(job: Promise<unknown>) {
    try {
      await job;
    } catch (e) {
      onerror((e as Error).message);
    }
  }

  async function redeem() {
    if (!confirm(`Use a reset on ${account.label}? This clears its usage limits and cannot be undone.`)) return;
    redeeming = true;
    try {
      onnotice((await redeemReset(account.id)).message);
    } catch (e) {
      onerror((e as Error).message);
    } finally {
      redeeming = false;
    }
  }
</script>

<div class="detail">
  <div>
    <dl class="kv">
      <dt>email</dt><dd>{account.email ?? "—"}</dd>
      <dt>plan</dt><dd>{planName(account.plan) || "unknown"}</dd>
      <dt>capacity</dt><dd>{account.capacity_multiplier}× <span class="faint">({account.capacity_source})</span></dd>
      <dt>active threads</dt><dd>{account.active_threads}</dd>
    </dl>
    <div class="resets">
      <h3>Resets</h3>
      {#if resets}
        {#each resets.rows as r, i (i)}
          <div class="reset-row"><span>{r.label}</span><span class="faint">{[r.left, r.expires].filter(Boolean).join(" · ")}</span></div>
        {:else}
          {#if !resets.note}<div class="faint">None available.</div>{/if}
        {/each}
        {#if resets.note}<div class="note">{resets.note}</div>{/if}
        <button class="btn" disabled={!resets.usable || redeeming} onclick={redeem}>{redeeming ? "Using reset…" : "Use a reset"}</button>
      {:else}
        <div class="faint">Refresh quotas to check.</div>
      {/if}
    </div>
  </div>
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
      <button class="btn" onclick={() => onlogin(account)}>Sign in again</button>
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
  .resets { margin-top: 16px; display: flex; flex-direction: column; align-items: flex-start; gap: 4px; font-size: 12px; }
  .reset-row { display: flex; justify-content: space-between; gap: 12px; width: 100%; }
  .note { color: var(--warn); font-family: var(--serif); font-style: italic; }
  .resets .btn { margin-top: 6px; }
</style>
