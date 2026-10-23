<script lang="ts">
  import { fetchUsage, isDemo } from "../api/client.ts";
  import type { UsageHistory, UsageSum } from "../api/types.ts";
  import { cacheShare, compact, usd } from "../model/format.ts";

  const RANGES = [7, 30, 90];
  let days = $state(30);
  let data = $state<UsageHistory | null>(null);
  let message = $state("");

  // Loads on mount and when the period changes, then every 30 s while the tab is open.
  $effect(() => {
    const period = days;
    const load = () =>
      fetchUsage(period).then(
        (d) => ((data = d), (message = "")),
        (e: Error & { status?: number }) => (message = e.status === 404 || e.status === 501 ? "This server does not keep usage history." : e.message),
      );
    void load();
    const timer = setInterval(load, 30_000);
    return () => clearInterval(timer);
  });

  const tokens = (s: UsageSum) => s.input_tokens + s.output_tokens + s.cache_read_tokens + s.cache_write_tokens;
  const spend = (s: UsageSum) => (s.unpriced_requests && s.unpriced_requests === s.requests ? "—" : usd(s.cost_usd));
  const mix = $derived(
    data
      ? [
          { name: "Input", n: data.totals.input_tokens, color: "var(--muted)" },
          { name: "Cache read", n: data.totals.cache_read_tokens, color: "var(--ok)" },
          { name: "Cache write", n: data.totals.cache_write_tokens, color: "var(--warn)" },
          { name: "Output", n: data.totals.output_tokens, color: "var(--claude)" },
        ]
      : [],
  );
  const day = (d: string) => new Date(`${d}T00:00:00Z`).toLocaleDateString("en-GB", { timeZone: "UTC", weekday: "short", day: "numeric", month: "short" });
  const time = (at: number) => new Date(at).toLocaleString("en-GB", { hourCycle: "h23", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
</script>

<div class="page">
  <div class="head">
    <div>
      <h2>Usage</h2>
      <div class="lede">tokens, and what the same traffic would cost at pay-per-token API prices</div>
    </div>
    <div class="range">
      {#each RANGES as d}<button class="btn" class:primary={days === d} onclick={() => (days = d)}>{d} days</button>{/each}
    </div>
  </div>

  {#if message}<p class="err">{message}</p>{/if}
  {#if data}
    {@const t = data.totals}
    {#if !data.statistics_enabled}
      <p class="warnline"><span class="dot warn"></span>Usage statistics are off, so nothing new is recorded. Set <span class="mono">observability.usage.usage-statistics-enabled</span> to <span class="mono">true</span>.</p>
    {/if}
    <div class="cards">
      <div class="card">
        <div class="k">Tokens</div>
        <div class="v">{compact(tokens(t))}</div>
        <div class="s">{compact(t.input_tokens)} in · {compact(t.output_tokens)} out · {compact(t.cache_read_tokens)} cached</div>
      </div>
      <div class="card">
        <div class="k">At API prices</div>
        <div class="v">{usd(t.cost_usd)}</div>
        <div class="s">{usd(t.cost_without_cache_usd)} without caching{t.unpriced_requests ? ` · ${t.unpriced_requests} unpriced` : ""}</div>
      </div>
      <div class="card">
        <div class="k">Cache savings</div>
        <div class="v">{usd(t.savings_usd)}</div>
        <div class="s">{cacheShare(t)}% of input read from cache</div>
      </div>
      <div class="card">
        <div class="k">Requests</div>
        <div class="v">{t.requests.toLocaleString()}</div>
        <div class="s">{t.failed ? `${t.failed} failed` : "none failed"}</div>
      </div>
    </div>

    {#if tokens(t)}
      <div class="mix" role="img" aria-label={mix.map((m) => `${m.name} ${compact(m.n)}`).join(", ")}>
        {#each mix as m}<span style:flex-grow={m.n} style:background={m.color}></span>{/each}
      </div>
      <div class="key faint">
        {#each mix as m}<span><i style:background={m.color}></i>{m.name} {compact(m.n)}</span>{/each}
      </div>
    {/if}
    <div class="lede src">
      prices {data.prices.source === "online" && data.prices.updated_at ? `from the online list, ${time(data.prices.updated_at)}` : "built in; the online list has not loaded yet"}
    </div>

    {#if data.days.length}
      <h2 class="sec">By day</h2>
      <table class="table">
        <thead><tr><th>Day</th><th class="n">Tokens</th><th class="n">Spend</th><th class="n">Saved</th><th class="n">Requests</th></tr></thead>
        <tbody>
          {#each [...data.days].reverse() as d (d.date)}
            <tr><td>{day(d.date)}</td><td class="n">{compact(tokens(d))}</td><td class="n">{spend(d)}</td><td class="n">{usd(d.savings_usd)}</td><td class="n">{d.requests.toLocaleString()}</td></tr>
          {/each}
        </tbody>
      </table>
    {/if}

    {#if data.models.length}
      <h2 class="sec">By model</h2>
      <table class="table">
        <thead><tr><th>Model</th><th class="n">Tokens</th><th class="n">Spend</th><th class="n">Saved</th><th class="n">Requests</th></tr></thead>
        <tbody>
          {#each data.models as m (m.model + m.provider)}
            <tr>
              <td class="mono">{m.model}{#if !m.priced}<span class="faint"> · no price known</span>{/if}</td>
              <td class="n">{compact(tokens(m))}</td><td class="n">{spend(m)}</td><td class="n">{usd(m.savings_usd)}</td><td class="n">{m.requests.toLocaleString()}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}

    {#if data.recent.length}
      <h2 class="sec">Latest requests</h2>
      <table class="table">
        <thead><tr><th>Time</th><th>Model</th><th class="n">In</th><th class="n">Cached</th><th class="n">Out</th><th class="n">Cost</th></tr></thead>
        <tbody>
          {#each data.recent.slice(0, 30) as r (r.request_id + r.at)}
            <tr>
              <td class="faint">{time(r.at)}</td><td class="mono">{r.model}</td>
              <td class="n">{compact(r.input_tokens)}</td><td class="n">{compact(r.cache_read_tokens)}</td><td class="n">{compact(r.output_tokens)}</td>
              <td class="n">{r.failed ? "failed" : usd(r.cost_usd)}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
    {#if !data.recent.length}<div class="empty">No requests recorded in this period{isDemo() ? "" : " yet"}.</div>{/if}
  {:else if !message}
    <div class="empty">Loading…</div>
  {/if}
</div>

<style>
  .head { display: flex; justify-content: space-between; align-items: flex-end; }
  .range { display: flex; gap: 8px; margin-bottom: 14px; }
  .cards { display: grid; grid-template-columns: repeat(4, 1fr); gap: 14px; margin-bottom: 22px; }
  .card { background: var(--panel); border: 1px solid var(--rule-soft); border-radius: 8px; padding: 16px 18px; }
  .k { color: var(--faint); font-size: 11px; letter-spacing: 1px; text-transform: uppercase; }
  .v { font-family: var(--serif); font-size: 34px; letter-spacing: -0.5px; margin: 6px 0 4px; font-variant-numeric: tabular-nums; }
  .s { color: var(--muted); font-size: 12px; }
  .mix { display: flex; height: 10px; border-radius: 5px; overflow: hidden; background: var(--panel-2); }
  .mix span { flex-basis: 0; min-width: 2px; }
  .key { display: flex; gap: 20px; margin-top: 8px; font-size: 12px; }
  .key i { display: inline-block; width: 8px; height: 8px; border-radius: 50%; margin-right: 6px; }
  .src { margin-top: 10px; }
  .sec { font-size: 19px; margin: 26px 0 10px; }
  .n { text-align: right; font-variant-numeric: tabular-nums; }
  .warnline { color: var(--warn); font-size: 12px; }
</style>
