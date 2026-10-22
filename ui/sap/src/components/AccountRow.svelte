<script lang="ts">
  import type { AccountView } from "../api/types.ts";
  import { presentAccount } from "../model/accounts.ts";
  import { planName } from "../model/plans.ts";
  import QuotaBars from "./QuotaBars.svelte";

  let { account, now }: { account: AccountView; now: string } = $props();
  const p = $derived(presentAccount(account, now));
</script>

<div class="acct-row {account.provider}" class:greyed={p.greyed} class:blocked={p.tone === "blocked"} data-account={account.id}>
  <div class="top">
    <span class="aname">{account.label}</span>
    <span class="plan">{planName(account.plan)}</span>
    <span class="weight">{p.tone === "blocked" ? "—" : Math.round(account.weight_percent)}<small>{p.tone === "blocked" ? "" : "%"}</small></span>
  </div>
  {#if p.flag}<div class="flag" class:stale={p.tone !== "blocked"}>{p.flag}</div>{:else if p.tone === "idle"}<div class="flag idle">idle · no active threads</div>{/if}
  <QuotaBars windows={p.windows} provider={account.provider} />
</div>

<style>
  .acct-row { position: relative; padding: 9px 4px 10px 14px; border-top: 1px solid var(--rule-soft); }
  .top { display: flex; align-items: baseline; gap: 8px; }
  .aname { font-family: var(--serif); font-size: 15px; }
  .plan { color: var(--muted); font-size: 11.5px; }
  .weight { margin-left: auto; font-family: var(--serif); font-size: 16px; font-variant-numeric: tabular-nums; }
  .weight small { color: var(--faint); font-family: var(--sans); font-size: 10.5px; margin-left: 3px; }
  .flag { font-size: 11px; color: var(--warn); font-style: italic; font-family: var(--serif); }
  .flag.idle, .flag.stale { color: var(--faint); }
  .greyed { filter: grayscale(1); opacity: .62; }
  .blocked { opacity: .5; }
  .blocked .aname { color: var(--ink-2); }
</style>
