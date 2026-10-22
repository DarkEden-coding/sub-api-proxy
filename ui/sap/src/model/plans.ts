/** Display names for the plan strings providers report. Keys are normalized plan names. */
const PLAN_NAMES: Record<string, string> = {
  // Claude
  pro: "Pro",
  max_5x: "Max 5×",
  max_20x: "Max 20×",
  team: "Team",
  enterprise: "Enterprise",
  // ChatGPT
  free: "Free",
  plus: "Plus",
  business: "Business",
};

/** Lowercased, with the `default_` and provider prefixes dropped: "default_claude_max_20x" → "max_20x". */
function normalize(plan: string): string {
  return plan
    .trim()
    .toLowerCase()
    .replace(/[\s-]+/g, "_")
    .replace(/^default_/, "")
    .replace(/^(claude|chatgpt|codex)_/, "");
}

const titleCase = (raw: string): string =>
  raw
    .trim()
    .split(/[\s_-]+/)
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1).toLowerCase())
    .join(" ");

/** "Max 20×" from "default_claude_max_20x", "Plus" from "plus"; an unknown plan is its raw value in title case. */
export function planName(plan: string | null): string {
  if (!plan?.trim()) return "";
  return PLAN_NAMES[normalize(plan)] ?? titleCase(plan);
}
