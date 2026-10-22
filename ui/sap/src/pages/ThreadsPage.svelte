<script lang="ts">
  import { assignThread } from "../api/client.ts";
  import { ago, shortKey } from "../model/format.ts";
  import { matchesThread } from "../model/threads.ts";
  import { store } from "../store.svelte.ts";

  const snap = $derived(store.snapshot!);
  let query = $state("");
  let message = $state("");
  const shown = $derived(snap.threads.filter((t) => matchesThread(t, query, store.accountLabel(t.account))));

  async function reassign(key: string, account: string) {
    message = "";
    try {
      await assignThread(key, { account });
    } catch (e) {
      message = (e as Error).message;
    }
  }
</script>

<div class="page">
  <div class="head">
    <div><h2>Threads</h2><div class="lede">{snap.threads.length} active · reassigning moves the thread's next request to the chosen account</div></div>
    <input class="field" type="search" placeholder="Search title, machine, account…" bind:value={query} />
  </div>
  {#if message}<p class="err">{message}</p>{/if}
  <table class="table">
    <thead><tr><th>Thread</th><th>Machine</th><th>Routing reason</th><th>Cache</th><th>Last seen</th><th>Account</th></tr></thead>
    <tbody>
      {#each shown as t (t.key)}
        <tr class={t.key.startsWith("codex:") ? "codex" : "claude"}>
          <td>
            <span class="tick">{t.streaming ? "●" : "○"}</span>
            {#if t.lineage !== "root"}<span class="kind">{t.lineage}</span>{/if}{t.title ?? shortKey(t.key)}
            <div class="mono faint">{shortKey(t.key)}</div>
          </td>
          <td>{t.machine ?? "—"}</td>
          <td class="why">{t.reason ?? "—"}</td>
          <td>{t.cache_hit_percent === null ? "—" : `${Math.round(t.cache_hit_percent)}%`}</td>
          <td>{ago(t.last_seen, snap.now)}</td>
          <td>
            <select class="field" value={t.account ?? ""} onchange={(e) => reassign(t.key, e.currentTarget.value)}>
              {#if !t.account}<option value="" disabled>unassigned</option>{/if}
              {#each store.accountsOf(t.key.startsWith("codex:") ? "codex" : "claude") as a (a.id)}
                <option value={a.id}>{a.label}</option>
              {/each}
            </select>
          </td>
        </tr>
      {:else}
        <tr><td colspan="6" class="empty">No matching threads.</td></tr>
      {/each}
    </tbody>
  </table>
</div>

<style>
  .head { display: flex; justify-content: space-between; align-items: flex-end; margin-bottom: 10px; }
  .head .field { width: 260px; }
  .tick { color: var(--tint); margin-right: 6px; font-size: 10px; }
  .kind { font-size: 10px; letter-spacing: .8px; text-transform: uppercase; color: var(--muted); border: 1px solid var(--rule); border-radius: 3px; padding: 0 4px; margin-right: 6px; }
  .why { color: var(--faint); font-family: var(--serif); font-style: italic; }
</style>
