<script lang="ts">
  import { Res, every } from "./store.svelte";
  import { api, missing, text } from "./api";
  import { cacheShare, compact, usd, type Data } from "./core";

  // Token mix and API-equivalent spend from the server's durable history: what this traffic
  // would have cost at pay-per-token prices, whatever plan actually paid for it.
  const ranges = [7, 30, 90];
  let days = $state(30);
  const history = new Res<Data>(() => api(`/observability/usage/history?days=${days}`));
  // Runs on mount and again when the period changes.
  $effect(() => {
    void days;
    history.load(true);
  });
  $effect(() => every(30_000, () => history.load(true)));

  const t = $derived(history.data?.totals as Data | undefined);
  const allTokens = $derived(t ? t.input_tokens + t.output_tokens + t.cache_read_tokens + t.cache_write_tokens : 0);
  const mix = $derived(
    t
      ? [
          { name: "Input", n: t.input_tokens, cls: "m-in" },
          { name: "Cache read", n: t.cache_read_tokens, cls: "m-read" },
          { name: "Cache write", n: t.cache_write_tokens, cls: "m-write" },
          { name: "Output", n: t.output_tokens, cls: "m-out" },
        ]
      : [],
  );
  const day = (d: string) => new Date(`${d}T00:00:00Z`).toLocaleDateString([], { timeZone: "UTC", month: "short", day: "numeric" });
  const time = (at: number) => new Date(at).toLocaleString([], { hourCycle: "h23", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
  const tokens = (r: Data) => r.input_tokens + r.output_tokens + r.cache_read_tokens + r.cache_write_tokens;
  const spend = (r: Data) => (r.unpriced_requests && r.unpriced_requests === r.requests ? "—" : usd(r.cost_usd));
</script>

<section class="section">
  <div class="section-head">
    <h2>Spend and tokens</h2>
    <div class="seg" role="group" aria-label="Period">
      {#each ranges as d}<button aria-pressed={days === d} onclick={() => (days = d)}>{d} days</button>{/each}
    </div>
  </div>
  {#if history.error}
    <p class="note">
      <span class="lamp {missing(history.error) ? 'off' : 'bad'}"></span>{missing(history.error)
        ? "This server does not keep usage history."
        : text(history.error)}
    </p>
  {:else if !t}
    <p class="muted">Loading…</p>
  {:else}
    {#if !history.data?.statistics_enabled}
      <p class="note">
        <span class="lamp warn"></span>Usage statistics are off, so nothing new is recorded. Set
        <code>observability.usage.usage-statistics-enabled</code> to <code>true</code>.
      </p>
    {/if}
    <div class="window display live-readings">
      <div class="reading">
        <span class="legend">Tokens</span><strong>{compact(allTokens)}</strong>
        <span class="legend">{compact(t.input_tokens)} in · {compact(t.output_tokens)} out · {compact(t.cache_read_tokens)} cached</span>
      </div>
      <div class="reading">
        <span class="legend">At API prices</span><strong>{usd(t.cost_usd)}</strong>
        <span class="legend"
          >{usd(t.cost_without_cache_usd)} without caching{t.unpriced_requests ? ` · ${t.unpriced_requests} unpriced` : ""}</span
        >
      </div>
      <div class="reading">
        <span class="legend">Cache savings</span><strong>{usd(t.savings_usd)}</strong>
        <span class="legend">{cacheShare(t)}% of input read from cache</span>
      </div>
    </div>
    {#if allTokens}
      <div class="mix" role="img" aria-label={mix.map((m) => `${m.name} ${compact(m.n)}`).join(", ")}>
        {#each mix as m}<span class={m.cls} style:flex-grow={m.n}></span>{/each}
      </div>
      <p class="legend mix-key">
        {#each mix as m}<span><i class={m.cls}></i>{m.name} {compact(m.n)}</span>{/each}
      </p>
    {/if}
    <p class="legend">
      {t.requests.toLocaleString()} requests{t.failed ? `, ${t.failed} failed` : ""} · prices {history.data?.prices.source === "online"
        ? `from the online list, ${time(history.data.prices.updated_at)}`
        : "built in; the online list has not loaded yet"}
    </p>

    {#if history.data?.days.length}
      <h3>By day</h3>
      <ul class="list">
        <li class="item usage-spend legend" aria-hidden="true"><span>Day</span><span class="count">Tokens</span><span class="count">Spend</span><span class="count">Saved</span><span class="count">Requests</span></li>
        {#each [...history.data.days].reverse() as d (d.date)}
          <li class="item usage-spend">
            <span>{day(d.date)}</span><span class="num count">{compact(tokens(d))}</span><span class="num count">{spend(d)}</span><span class="num count">{usd(d.savings_usd)}</span><span class="num count">{d.requests.toLocaleString()}</span>
          </li>
        {/each}
      </ul>
    {/if}

    {#if history.data?.models.length}
      <h3>By model</h3>
      <ul class="list">
        <li class="item usage-spend legend" aria-hidden="true"><span>Model</span><span class="count">Tokens</span><span class="count">Spend</span><span class="count">Saved</span><span class="count">Requests</span></li>
        {#each history.data.models as m (m.model + m.provider)}
          <li class="item usage-spend">
            <code class="ellipsis" title={m.model}>{m.model}</code><span class="num count">{compact(tokens(m))}</span><span class="num count" title={m.priced ? "" : "No price known for this model"}>{spend(m)}</span><span class="num count">{usd(m.savings_usd)}</span><span class="num count">{m.requests.toLocaleString()}</span>
          </li>
        {/each}
      </ul>
    {/if}

    {#if history.data?.recent.length}
      <h3>Latest requests</h3>
      <ul class="list">
        <li class="item usage-request legend" aria-hidden="true"><span>Time</span><span>Model</span><span class="count">In</span><span class="count">Cached</span><span class="count">Out</span><span class="count">Cost</span></li>
        {#each history.data.recent.slice(0, 30) as r (r.request_id + r.at)}
          <li class="item usage-request">
            <span class="legend">{time(r.at)}</span>
            <code class="ellipsis" title={r.model}>{r.model}</code>
            <span class="num count">{compact(r.input_tokens)}</span>
            <span class="num count">{compact(r.cache_read_tokens)}</span>
            <span class="num count">{compact(r.output_tokens)}</span>
            <span class="num count">{r.failed ? "failed" : usd(r.cost_usd)}</span>
          </li>
        {/each}
      </ul>
    {/if}
  {/if}
</section>
