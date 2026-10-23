<script lang="ts">
  import { fetchUsage } from "../api/client.ts";
  import type { UsageHistory, UsageSum } from "../api/types.ts";
  import Donut from "../components/Donut.svelte";
  import StackedColumns from "../components/StackedColumns.svelte";
  import { cacheShare, compact, usd } from "../model/format.ts";
  import { fillDays, percent, SAVED_COLOR, SPENT_COLOR, TOKEN_TYPES, tokenCounts, trimLeading } from "../model/usage.ts";

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
        (e: Error & { status?: number }) =>
          (message = e.status === 404 || e.status === 501 ? "This server does not keep usage history." : e.message),
      );
    void load();
    const timer = setInterval(load, 30_000);
    return () => clearInterval(timer);
  });

  const tokens = (s: UsageSum) => tokenCounts(s).reduce((a, b) => a + b, 0);
  const dayLabel = (date: string) =>
    new Date(`${date}T00:00:00Z`).toLocaleDateString("en-GB", { timeZone: "UTC", day: "numeric", month: "short" });
  const time = (at: number) =>
    new Date(at).toLocaleString("en-GB", { hourCycle: "h23", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
  const money = (n: number) => (n === 0 ? "$0" : usd(n));

  const filled = $derived(data ? trimLeading(fillDays(data.days, days, Date.now())) : []);
  const peakSpend = $derived(data?.days.reduce<UsageSum & { date: string } | null>((a, d) => (!a || d.cost_usd > a.cost_usd ? d : a), null) ?? null);
  const peakTokens = $derived(data?.days.reduce<UsageSum & { date: string } | null>((a, d) => (!a || tokens(d) > tokens(a) ? d : a), null) ?? null);
  const spendColumns = $derived(
    filled.map((d) => ({ label: dayLabel(d.date), values: "empty" in d ? [0, 0] : [d.cost_usd, Math.max(0, d.savings_usd)] })),
  );
  const tokenColumns = $derived(
    filled.map((d) => ({ label: dayLabel(d.date), values: "empty" in d ? [0, 0, 0, 0] : tokenCounts(d) })),
  );
  const tokenParts = $derived(
    data ? TOKEN_TYPES.map((t, i) => ({ label: t.label, color: t.color, value: tokenCounts(data!.totals)[i] })) : [],
  );
  const spendParts = $derived(
    data ? TOKEN_TYPES.map((t) => ({ label: t.label, color: t.color, value: data!.totals.cost_by_type_usd[t.key] })) : [],
  );
  const activeDays = $derived(data?.days.filter((d) => d.requests > 0).length ?? 0);
  const topSpend = $derived(Math.max(0, ...(data?.models.map((m) => m.cost_usd) ?? [0])));
  const savedShare = $derived(data && data.totals.cost_without_cache_usd > 0 ? data.totals.savings_usd / data.totals.cost_without_cache_usd : 0);
</script>

<div class="page">
  <div class="head">
    <div>
      <h2>Usage</h2>
      <div class="lede">tokens, and what the same traffic would cost at pay-per-token API prices</div>
    </div>
    <div class="range" role="group" aria-label="Period">
      {#each RANGES as d}<button class="btn" class:primary={days === d} aria-pressed={days === d} onclick={() => (days = d)}>{d} days</button>{/each}
    </div>
  </div>

  {#if message}<p class="err">{message}</p>{/if}
  {#if data}
    {@const t = data.totals}
    {#if !data.statistics_enabled}
      <p class="warnline"><span class="dot warn"></span>Usage statistics are off, so nothing new is recorded. Set <span class="mono">observability.usage.usage-statistics-enabled</span> to <span class="mono">true</span>.</p>
    {/if}

    <section class="tiles">
      <div class="tile hero">
        <div class="k">At API prices</div>
        <div class="v">{usd(t.cost_usd)}</div>
        <div class="s">
          {usd(t.cost_without_cache_usd)} without caching{#if activeDays} · {usd(t.cost_usd / activeDays)} per active day{/if}
        </div>
      </div>
      <div class="tile">
        <div class="k">Saved by caching</div>
        <div class="v">{usd(t.savings_usd)}</div>
        <div class="s">{percent(savedShare)} off the uncached price</div>
      </div>
      <div class="tile">
        <div class="k">Tokens</div>
        <div class="v">{compact(tokens(t))}</div>
        <div class="s">{cacheShare(t)}% of input from cache</div>
      </div>
      <div class="tile">
        <div class="k">Requests</div>
        <div class="v">{t.requests.toLocaleString("en-US")}</div>
        <div class="s">{t.failed ? `${t.failed.toLocaleString("en-US")} failed` : "none failed"}{t.unpriced_requests ? ` · ${t.unpriced_requests} unpriced` : ""}</div>
      </div>
    </section>

    <div class="layout">
      <div class="main">
        <section class="card">
          <div class="card-head">
            <div>
              <h3>Daily spend</h3>
              <div class="lede">API-equivalent cost, and what caching took off it</div>
              {#if activeDays}<div class="figs"><span><b>{usd(t.cost_usd / activeDays)}</b> per active day</span>{#if peakSpend}<span><b>{usd(peakSpend.cost_usd)}</b> peak, {dayLabel(peakSpend.date)}</span>{/if}</div>{/if}
            </div>
            <div class="legend">
              <span><i style:background={SPENT_COLOR}></i>Spent</span>
              <span><i style:background={SAVED_COLOR}></i>Saved by caching</span>
            </div>
          </div>
          <StackedColumns
            series={[{ label: "Spent", color: SPENT_COLOR }, { label: "Saved by caching", color: SAVED_COLOR }]}
            columns={spendColumns}
            format={money}
            axisFormat={(n) => `$${compact(n)}`}
            totalLabel="Without caching"
          />
        </section>
        <section class="card">
          <div class="card-head">
            <div>
              <h3>Daily tokens</h3>
              <div class="lede">by type; cache reads are billed at a fraction of the input price</div>
              {#if activeDays}<div class="figs"><span><b>{compact(tokens(t) / activeDays)}</b> per active day</span>{#if peakTokens}<span><b>{compact(tokens(peakTokens))}</b> peak, {dayLabel(peakTokens.date)}</span>{/if}</div>{/if}
            </div>
            <div class="legend">
              {#each TOKEN_TYPES as tt}<span><i style:background={tt.color}></i>{tt.label}</span>{/each}
            </div>
          </div>
          <StackedColumns series={TOKEN_TYPES.map((tt) => ({ label: tt.label, color: tt.color }))} columns={tokenColumns} format={compact} />
        </section>
      </div>
      <div class="side">
        <section class="card fill">
          <h3>Token mix</h3>
          <div class="lede">share of all tokens in the period</div>
          <Donut parts={tokenParts} format={compact} caption="tokens" />
        </section>
        <section class="card fill">
          <h3>Where the money goes</h3>
          <div class="lede">API-equivalent spend by token type</div>
          <Donut parts={spendParts} format={money} caption="spent" />
        </section>
      </div>
    </div>

    {#if data.models.length}
      <section class="card">
        <h3>By model</h3>
        <div class="lede">ordered by spend</div>
        <table class="table models">
          <thead><tr><th>Model</th><th class="barcol">Spend</th><th class="n">Saved</th><th class="n">Tokens</th><th class="n">Cached</th><th class="n">Requests</th></tr></thead>
          <tbody>
            {#each data.models as m (m.model + m.provider)}
              <tr>
                <td><span class="mono">{m.model}</span><span class="faint prov">{m.provider}</span></td>
                <td class="barcol">
                  {#if m.priced}
                    <div class="hbar"><span style:width="{topSpend ? (m.cost_usd / topSpend) * 100 : 0}%"></span></div>
                    <b>{usd(m.cost_usd)}</b>
                  {:else}<span class="faint">no price known</span>{/if}
                </td>
                <td class="n">{m.priced ? usd(m.savings_usd) : "—"}</td>
                <td class="n">{compact(tokens(m))}</td>
                <td class="n">{cacheShare(m)}%</td>
                <td class="n">{m.requests.toLocaleString("en-US")}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </section>
    {:else}
      <div class="empty">No requests recorded in the last {days} days.</div>
    {/if}

    <div class="foot faint">
      Prices {data.prices.source === "online" && data.prices.updated_at ? `from the public price list, updated ${time(data.prices.updated_at)}` : "from the built-in table; the public list has not loaded yet"}.
      Days are UTC.
    </div>
  {:else if !message}
    <div class="empty">Loading…</div>
  {/if}
</div>

<style>
  .head { display: flex; justify-content: space-between; align-items: flex-end; margin-bottom: 4px; }
  .range { display: flex; gap: 8px; margin-bottom: 14px; }
  .warnline { color: var(--warn); font-size: 12px; }

  .tiles { display: grid; grid-template-columns: 1.4fr 1fr 1fr 1fr; gap: 14px; margin-bottom: 14px; }
  .tile, .card { background: var(--panel); border: 1px solid var(--rule-soft); border-radius: 10px; }
  .tile { padding: 18px 20px; }
  .k { color: var(--faint); font-size: 11px; letter-spacing: 1px; text-transform: uppercase; }
  .v { font-family: var(--serif); font-size: 34px; letter-spacing: -0.5px; margin: 8px 0 4px; }
  .hero .v { font-size: 46px; color: var(--ink); }
  .s { color: var(--muted); font-size: 12px; }

  .card { padding: 18px 20px 16px; margin-bottom: 14px; }
  .card h3 { font-size: 18px; }
  .card .lede { margin-bottom: 12px; }
  .card-head { display: flex; justify-content: space-between; align-items: flex-start; gap: 20px; }
  .legend { display: flex; gap: 16px; font-size: 12px; color: var(--muted); padding-top: 4px; }
  .legend i { display: inline-block; width: 9px; height: 9px; border-radius: 2px; margin-right: 7px; vertical-align: -0.5px; }
  .layout { display: grid; grid-template-columns: minmax(0, 1.75fr) minmax(400px, 1fr); gap: 14px; margin-bottom: 14px; }
  .main, .side { display: flex; flex-direction: column; gap: 14px; }
  .main .card, .side .card { margin-bottom: 0; }
  .side .card { flex: 1; display: flex; flex-direction: column; }
  .side .card :global(.donut) { flex: 1; }
  .figs { display: flex; gap: 18px; color: var(--muted); font-size: 12px; margin: -6px 0 10px; }
  .figs b { color: var(--ink); font-weight: 500; font-variant-numeric: tabular-nums; }

  .models td { padding-top: 11px; padding-bottom: 11px; }
  .models .n, .models th.n { text-align: right; font-variant-numeric: tabular-nums; }
  .prov { margin-left: 8px; font-size: 11.5px; }
  .barcol { width: 34%; }
  .barcol .hbar { display: inline-block; width: calc(100% - 80px); height: 8px; border-radius: 0 4px 4px 0; background: var(--panel-2); vertical-align: middle; overflow: hidden; }
  .hbar span { display: block; height: 100%; min-width: 2px; background: var(--claude); border-radius: 0 4px 4px 0; }
  .barcol b { display: inline-block; width: 72px; text-align: right; font-weight: 500; color: var(--ink); font-variant-numeric: tabular-nums; }
  .foot { font-size: 11.5px; margin-top: 4px; }
</style>
