<script lang="ts">
  import type { ThreadView } from "../api/types.ts";
  import { shortKey } from "../model/format.ts";

  let { thread, depth, selected, accountName, machineTag, onselect }: {
    thread: ThreadView;
    depth: number;
    selected: boolean;
    accountName: string;
    /** Set when the thread ran on a different machine than the group it is shown in. */
    machineTag: string | null;
    onselect: () => void;
  } = $props();

  const provider = $derived(thread.key.startsWith("codex:") ? "codex" : "claude");
</script>

<div
  class="thread {provider}"
  class:child={depth > 0}
  class:sel={selected}
  style:--depth={depth}
  data-thread={thread.key}
  role="button"
  tabindex="0"
  onclick={onselect}
  onkeydown={(e) => (e.key === "Enter" || e.key === " ") && onselect()}
>
  <div class="title">
    {#if thread.lineage !== "root"}<span class="kind">{thread.lineage}</span>{/if}{thread.title ?? shortKey(thread.key)}
  </div>
  <div class="acct">{accountName}</div>
  <div class="tid">
    {shortKey(thread.key)}{#if machineTag} · {machineTag}{/if}{#if thread.streaming}<span class="live">● streaming</span>{/if}
  </div>
  <div class="why">
    {thread.reason ?? "not routed yet"}{#if thread.cache_hit_percent !== null} · cache {Math.round(thread.cache_hit_percent)}%{/if}
  </div>
</div>

<style>
  .thread { position: relative; display: grid; grid-template-columns: 1fr auto; gap: 2px 10px; padding: 7px 8px 7px 38px; margin-left: calc(var(--depth) * 22px); border-radius: 6px; cursor: pointer; }
  .thread:hover { background: var(--panel); }
  .thread:focus-visible { outline: 1px solid var(--muted); }
  .thread.sel { background: var(--panel-2); box-shadow: inset 2px 0 0 var(--tint); }
  .title { color: var(--ink); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .tid { font-family: var(--mono); font-size: 11px; color: var(--faint); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .acct { text-align: right; font-size: 12px; color: var(--ink-2); white-space: nowrap; max-width: 150px; overflow: hidden; text-overflow: ellipsis; }
  .why { text-align: right; font-size: 11px; color: var(--faint); font-style: italic; font-family: var(--serif); white-space: nowrap; max-width: 190px; overflow: hidden; text-overflow: ellipsis; }
  .child::before { content: ""; position: absolute; left: 22px; top: -4px; bottom: 50%; width: 10px; border-left: 1px solid var(--rule); border-bottom: 1px solid var(--rule); border-bottom-left-radius: 6px; }
  .kind { font-size: 10px; letter-spacing: .8px; text-transform: uppercase; color: var(--muted); border: 1px solid var(--rule); border-radius: 3px; padding: 0 4px; margin-right: 6px; vertical-align: 1px; }
  .live { color: var(--ok); margin-left: 8px; }
</style>
