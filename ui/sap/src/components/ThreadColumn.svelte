<script lang="ts">
  import type { ThreadView } from "../api/types.ts";
  import { flatten, type ClientGroup, type ThreadNode } from "../model/threads.ts";
  import { store } from "../store.svelte.ts";
  import ThreadRow from "./ThreadRow.svelte";

  let { groups }: { groups: ClientGroup[] } = $props();

  interface Row { thread: ThreadView; depth: number }
  const rows = (node: ThreadNode, depth = 0): Row[] => [
    { thread: node.thread, depth },
    ...node.children.flatMap((c) => rows(c, depth + 1)),
  ];
  const selectedKey = $derived(store.chosenThread?.key);
</script>

<div class="col">
  <h2>Threads</h2>
  <div class="lede">active only · by client, then machine · children nested under their parent</div>
  {#each groups as g (g.id)}
    {@const open = !store.collapsed[g.id]}
    <div class="group" class:open>
      <div class="ghead" data-unit={g.id} role="button" tabindex="0" onclick={() => store.toggle(g.id)} onkeydown={(e) => e.key === "Enter" && store.toggle(g.id)}>
        <span class="chev">▶</span>
        <span class="glyph {g.id}">{g.glyph}</span>
        <span class="gname">{g.name}</span>
        <span class="gmeta">{g.count} threads · {g.streaming} streaming</span>
      </div>
      {#if open}
        {#each g.machines as m (m.id)}
          {@const mopen = !store.collapsed[m.id]}
          <div class="machine" class:open={mopen}>
            <div class="mhead" data-unit={m.id} role="button" tabindex="0" onclick={() => store.toggle(m.id)} onkeydown={(e) => e.key === "Enter" && store.toggle(m.id)}>
              <span class="chev">▶</span>
              <span class="mname">{m.name}</span>
              <span class="gmeta">{m.count} · {m.streaming} streaming</span>
            </div>
            {#if mopen}
              {#each m.roots as root (root.thread.key)}
                {#each rows(root) as r (r.thread.key)}
                  <ThreadRow
                    thread={r.thread}
                    depth={r.depth}
                    selected={r.thread.key === selectedKey}
                    accountName={store.accountLabel(r.thread.account)}
                    machineTag={(r.thread.machine ?? "unknown machine") !== m.name ? (r.thread.machine ?? "unknown machine") : null}
                    onselect={() => (store.selected = r.thread.key)}
                  />
                {/each}
              {/each}
            {/if}
          </div>
        {/each}
      {/if}
    </div>
  {:else}
    <div class="empty">No active threads.</div>
  {/each}
</div>

<style>
  .group { border-top: 1px solid var(--rule); }
  .group:last-child { border-bottom: 1px solid var(--rule); }
  .ghead, .mhead { display: flex; align-items: center; gap: 10px; cursor: pointer; user-select: none; }
  .ghead { padding: 11px 4px; }
  .mhead { padding: 7px 4px 7px 18px; border-top: 1px solid var(--rule-soft); }
  .chev { color: var(--faint); width: 10px; font-size: 10px; transition: transform .15s; }
  .open > .ghead .chev, .machine.open > .mhead .chev { transform: rotate(90deg); }
  .glyph { width: 18px; height: 18px; border-radius: 4px; display: grid; place-items: center; font-size: 11px; font-weight: 600; color: var(--bg); background: var(--muted); }
  .glyph.claude_code { background: var(--claude); }
  .glyph.codex_cli { background: var(--openai); }
  .gname { font-family: var(--serif); font-size: 16px; }
  .mname { font-family: var(--mono); font-size: 12px; color: var(--ink-2); }
  .gmeta { margin-left: auto; color: var(--muted); font-size: 12px; }
  .machine { padding-bottom: 4px; }
</style>
