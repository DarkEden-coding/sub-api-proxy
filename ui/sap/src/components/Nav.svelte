<script lang="ts">
  import { PAGES, store } from "../store.svelte.ts";
</script>

<nav>
  {#each PAGES as page}
    <a href="#/{page}" class:on={store.page === page}>
      {page}{#if page === "nodes" && store.snapshot?.open_conflicts}<span class="badge">{store.snapshot.open_conflicts}</span>{/if}
    </a>
  {/each}
</nav>

<style>
  nav { display: flex; gap: 26px; padding: 12px 0 0; margin-bottom: 26px; }
  a { color: var(--muted); text-decoration: none; font-size: 11.5px; letter-spacing: 1.2px; text-transform: uppercase; padding-bottom: 8px; border-bottom: 1px solid transparent; }
  a:hover { color: var(--ink-2); }
  a.on { color: var(--ink); border-bottom-color: var(--claude); }
  .badge { margin-left: 6px; color: var(--bg); background: var(--warn); border-radius: 8px; padding: 0 5px; font-size: 10px; letter-spacing: 0; }
</style>
