// Screenshots of every page in demo mode. Usage: node scripts/shots.mjs [outDir]
// Needs `npm run build` and `npm run preview` (port 4174) running; set CHROME to a browser binary.
import { chromium } from "playwright-core";

const out = process.argv[2] ?? "shots";
const base = process.env.SAP_URL ?? "http://localhost:4174/sap/";
const browser = await chromium.launch({ executablePath: process.env.CHROME, args: ["--no-sandbox"] });
const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, timezoneId: "UTC" });
page.on("pageerror", (e) => console.error("pageerror", e.message));
page.on("console", (m) => m.type() === "error" && console.error("console", m.text()));

for (const name of ["flow", "accounts", "threads", "nodes", "events", "settings"]) {
  await page.goto(`${base}?demo=1#/${name}`);
  await page.waitForTimeout(500);
  await page.screenshot({ path: `${out}/${name}.png`, fullPage: true });
}

await page.goto(`${base}?demo=1#/flow`);
await page.waitForTimeout(400);
await page.click('[data-unit="claude_code|scythe-desktop-1"]');
await page.click('[data-unit="codex_cli"]');
await page.waitForTimeout(300);
await page.screenshot({ path: `${out}/flow-collapsed.png`, fullPage: true });
await page.goto(`${base}?demo=1#/accounts`);
await page.click("tr.row >> nth=1");
await page.waitForTimeout(400);
await page.screenshot({ path: `${out}/accounts-detail.png`, fullPage: true });
await page.goto(`${base}?demo=1#/flow`);
await page.reload();
await page.click('[data-thread="codex:01a16530-4c4a-7de3-8b45-22937a4be0e8"]');
await page.waitForTimeout(300);
await page.screenshot({ path: `${out}/flow-codex-selected.png`, fullPage: true });
await browser.close();
