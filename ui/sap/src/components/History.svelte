<script lang="ts">
  import type { HistoryPoint } from "../api/types.ts";

  let { samples }: { samples: HistoryPoint[] } = $props();

  const W = 300, H = 60;
  const line = (pick: (p: HistoryPoint) => number | null) => {
    let d = "";
    samples.forEach((p, i) => {
      const v = pick(p);
      if (v === null) return;
      const x = samples.length > 1 ? (i / (samples.length - 1)) * W : 0;
      d += `${d ? "L" : "M"}${x.toFixed(1)},${(H - 4 - (Math.min(v, 100) / 100) * (H - 8)).toFixed(1)} `;
    });
    return d;
  };
</script>

<svg width="100%" height={H} viewBox="0 0 {W} {H}" preserveAspectRatio="none">
  <line x1="0" x2={W} y1="4" y2="4" stroke="#2a2520" vector-effect="non-scaling-stroke" />
  <line x1="0" x2={W} y1={H - 4} y2={H - 4} stroke="#2a2520" vector-effect="non-scaling-stroke" />
  <path d={line((p) => p.weekly_used_percent)} stroke="#9a9083" fill="none" stroke-width="1.5" vector-effect="non-scaling-stroke" />
  <path d={line((p) => p.five_hour_used_percent)} stroke="#cf8a66" fill="none" stroke-width="1.5" vector-effect="non-scaling-stroke" />
</svg>
<div class="legend"><span style:color="var(--claude)">5 h used</span><span style:color="var(--muted)">week used</span></div>

<style>
  svg { display: block; }
  .legend { display: flex; gap: 16px; color: var(--faint); font-size: 11px; margin-top: 4px; }
  .legend span::before { content: ""; display: inline-block; width: 12px; height: 2px; margin-right: 5px; vertical-align: 3px; background: currentColor; }
</style>
