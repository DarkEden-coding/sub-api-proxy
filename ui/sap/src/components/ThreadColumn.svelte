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
    <div class="idle">
      <svg viewBox="0 0 120 64" aria-hidden="true">
        <g fill="none" stroke="var(--rule)" stroke-width="1.4" stroke-dasharray="2 4" stroke-linecap="round">
          <path d="M12 10C46 10 54 32 92 32" /><path d="M12 32H92" /><path d="M12 54C46 54 54 32 92 32" />
        </g>
        <g fill="var(--faint)"><circle cx="12" cy="10" r="3" /><circle cx="12" cy="32" r="3" /><circle cx="12" cy="54" r="3" /></g>
        <circle class="pulse" cx="92" cy="32" r="7" fill="none" stroke="var(--muted)" stroke-width="1.2" />
        <circle cx="92" cy="32" r="5" fill="var(--faint)" />
      </svg>
      <div class="title">Waiting for a session</div>
      <p>Claude Code and Codex threads appear here as soon as they send a request, and leave a little after they go quiet.</p>
    </div>
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
  .idle { margin: 18px 16px 0 0; padding: 28px 24px; border: 1px dashed var(--rule); border-radius: 10px; text-align: center; }
  .idle svg { width: 120px; height: 64px; overflow: visible; }
  .idle .title { margin-top: 12px; font-family: var(--serif); font-size: 17px; color: var(--ink-2); }
  .idle p { margin: 6px auto 0; max-width: 300px; color: var(--muted); font-size: 12.5px; line-height: 1.5; }
  .pulse { transform-origin: 92px 32px; animation: pulse 2.8s ease-out infinite; }
  @keyframes pulse { from { transform: scale(1); opacity: .7; } to { transform: scale(2.2); opacity: 0; } }
  @media (prefers-reduced-motion: reduce) { .pulse { animation: none; opacity: 0; } }
</style>
