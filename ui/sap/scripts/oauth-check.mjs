// Browser regression using local mock servers and intercepted provider pages only.
// Build sap-router and ui/sap first. CHROME=/path/to/chromium node ui/sap/scripts/oauth-check.mjs
import assert from "node:assert/strict";
import { createServer as httpServer } from "node:http";
import { createServer as tcpServer } from "node:net";
import { mkdtemp, readFile, writeFile, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve, extname } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { chromium } from "playwright-core";

const root = fileURLToPath(new URL("../../../", import.meta.url));
const out = process.env.SAP_OAUTH_SHOTS ?? `${tmpdir()}/sap-oauth-review`;
const temporary = await mkdtemp(`${tmpdir()}/sap-oauth-check-`);
const servers = [];
let router, browser;
const listen = server => new Promise((accept, reject) => {
  server.once("error", reject);
  server.listen(0, "127.0.0.1", () => accept(server.address().port));
});
const close = server => new Promise(accept => server.close(accept));
try {
  // Never replace or reuse a real callback listener.
  for (const port of [54545, 1455]) {
    const check = tcpServer();
    await new Promise((accept, reject) => { check.once("error", reject); check.listen(port, "127.0.0.1", accept); });
    await close(check);
  }
  let count = 0;
  let lastLogin;
  const loginResponse = request => {
    assert.equal(request.callback_relay, true);
    const claude = request.provider === "claude";
    const auth = new URL(claude ? "https://claude.ai/oauth/authorize" : "https://auth.openai.com/oauth/authorize");
    auth.search = new URLSearchParams({ client_id: claude ? "9d1c250a-e61b-44d9-88ed-5944d1962f5e" : "app_EMoamEEZ73f0CkXaXp7hrann", redirect_uri: claude ? "http://localhost:54545/callback" : "http://localhost:1455/auth/callback", state: `fake-${++count}`, code_challenge: "fake-challenge", code_challenge_method: "S256", response_type: "code" }).toString();
    lastLogin = auth.href;
    return { url: auth.href, login_id: `fake-${count}`, callback_relay: true };
  };
  const callbacks = [];
  const health = { node: "mock", role: "primary", state: "serving", version: "test", now: "2026-11-07T00:00:00Z", started_at: "2026-11-07T00:00:00Z", upstream: { claude: true, codex: true }, open_streams: 0, sync: {} };
  for (const name of ["preferred", "issuer"]) {
    const server = httpServer(async (req, res) => {
      if (req.url === "/sap/health") { res.setHeader("Content-Type", "application/json"); res.end(JSON.stringify(health)); return; }
      if (req.url === "/sap/api/accounts/login") {
        let body = "";
        for await (const chunk of req) body += chunk;
        res.setHeader("Content-Type", "application/json");
        res.end(JSON.stringify(loginResponse(JSON.parse(body))));
        return;
      }
      assert.ok(req.url.startsWith("/anthropic/callback?") || req.url.startsWith("/codex/callback?"));
      assert.equal(req.headers.authorization, undefined);
      callbacks.push({ name, url: req.url });
      res.end(`Signed in on ${name}`);
    });
    servers.push({ server, port: await listen(server) });
  }
  const reserve = tcpServer();
  const routerPort = await listen(reserve);
  await close(reserve);
  const helper = `http://127.0.0.1:${routerPort}`;
  await writeFile(`${temporary}/router.yaml`, JSON.stringify({
    listen: `127.0.0.1:${routerPort}`, servers: [
      { name: "preferred", base_url: `http://127.0.0.1:${servers[0].port}` },
      { name: "issuer", base_url: `http://127.0.0.1:${servers[1].port}`, oauth_origins: ["https://dashboard.example.test"] },
    ],
  }));
  router = spawn(`${root}/target/debug/sap-router`, ["--config", `${temporary}/router.yaml`], { stdio: ["ignore", "ignore", "pipe"] });
  let routerLog = "";
  router.stderr.on("data", chunk => { routerLog += chunk; });
  await assert.doesNotReject(async () => {
    for (let attempt = 0; attempt < 100; attempt++) {
      if (router.exitCode !== null) throw new Error(routerLog);
      try { if ((await (await fetch(`${helper}/sap/router/status`)).json()).preferred === "preferred") return; } catch {}
      await new Promise(accept => setTimeout(accept, 25));
    }
    throw new Error(`Helper did not become ready: ${routerLog}`);
  });
  browser = await chromium.launch({ executablePath: process.env.CHROME, headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const sample = await readFile(`${root}/crates/sap-core/tests/fixtures/flow.json`, "utf8");
  await context.addInitScript(helper => {
    localStorage.setItem("sap-management-key", "fake-management-only");
    localStorage.setItem("sap-oauth-helper", helper);
  }, helper);
  await context.route("**/*", async route => {
    const url = new URL(route.request().url());
    const local = ["127.0.0.1", "localhost"].includes(url.hostname);
    if (local && (url.pathname.startsWith("/sap/oauth/") || !url.pathname.startsWith("/sap/") || url.pathname === "/sap/api/accounts/login")) {
      if (url.pathname.startsWith("/sap/oauth/")) assert.equal(route.request().headers().authorization, undefined);
      return route.continue();
    }
    if (["claude.ai", "auth.openai.com"].includes(url.hostname)) {
      assert.equal(url.href, lastLogin);
      const callback = new URL(url.searchParams.get("redirect_uri"));
      callback.search = `code=fake%2Bcode&state=${url.searchParams.get("state")}`;
      return route.fulfill({ status: 302, headers: { location: callback.href }, body: "" });
    }
    if (!local) assert.equal(url.origin, "https://dashboard.example.test", "unexpected external request");
    if (url.pathname === "/sap/api/accounts/login") return route.fulfill({ json: loginResponse(route.request().postDataJSON()) });
    if (url.pathname === "/sap/api/flow") return route.fulfill({ contentType: "application/json", body: sample });
    if (url.pathname === "/sap/api/stream") return route.fulfill({ contentType: "text/event-stream", body: `event: snapshot\ndata: ${JSON.stringify(JSON.parse(sample))}\n\n` });
    if (url.pathname.endsWith("/history")) return route.fulfill({ json: { samples: [] } });
    const path = url.pathname === "/sap/" ? "index.html" : url.pathname.replace(/^\/sap\//, "");
    assert.ok(!path.includes(".."));
    const contentType = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml" }[extname(path)] ?? "application/octet-stream";
    return route.fulfill({ contentType, body: await readFile(`${root}/ui/sap/dist/${path}`) });
  });
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.goto("https://dashboard.example.test/sap/#/accounts");
  await page.getByRole("button", { name: "Add account", exact: true }).waitFor();
  await mkdir(out, { recursive: true });
  await page.screenshot({ path: `${out}/accounts-wide.png`, fullPage: true });
  await page.setViewportSize({ width: 640, height: 900 });
  await page.evaluate(() => new Promise(done => requestAnimationFrame(() => requestAnimationFrame(done))));
  const actionBox = await page.getByRole("button", { name: "Add account", exact: true }).boundingBox();
  assert.ok(actionBox && actionBox.x >= 0 && actionBox.x + actionBox.width <= 640, "sign-in button must fit the narrow viewport");
  await page.screenshot({ path: `${out}/accounts-narrow.png`, fullPage: false });
  await page.setViewportSize({ width: 1440, height: 900 });
  for (const provider of ["claude", "codex"]) {
    await page.locator("select").selectOption(provider);
    const opened = context.waitForEvent("page");
    await page.getByRole("button", { name: "Add account", exact: true }).click();
    const popup = await opened;
    await popup.getByText("Signed in on issuer", { exact: true }).waitFor();
    const callback = callbacks.at(-1);
    assert.equal(callback.name, "issuer");
    assert.equal(callback.url, `${provider === "claude" ? "/anthropic" : "/codex"}/callback?code=fake%2Bcode&state=fake-${count}`);
    await popup.close();
  }
  await page.locator("tr.row").first().click();
  const opened = context.waitForEvent("page");
  await page.getByRole("button", { name: "Sign in again", exact: true }).click();
  const popup = await opened;
  await popup.getByText("Signed in on issuer", { exact: true }).waitFor();
  await popup.close();
  assert.equal(callbacks.length, 3);
  for (const [base, expected] of [[helper, "preferred"], [`http://127.0.0.1:${servers[1].port}`, "issuer"]]) {
    await page.goto(`${base}/sap/#/accounts`);
    const opened = context.waitForEvent("page");
    await page.getByRole("button", { name: "Add account", exact: true }).click();
    const popup = await opened;
    await popup.getByText(`Signed in on ${expected}`, { exact: true }).waitFor();
    assert.equal(callbacks.at(-1).name, expected);
    await popup.close();
  }
  assert.equal(callbacks.length, 5);
  assert.deepEqual(errors, []);
  // A non-loopback helper address fails before starting another login.
  await page.getByLabel("Local sign-in helper").fill("http://100.64.0.1:8317");
  await page.getByRole("button", { name: "Add account", exact: true }).click();
  await page.getByText("The sign-in helper must use an http:// loopback address on this device.", { exact: true }).waitFor();
  assert.equal(count, 5);
  console.log(`PASS: HTTPS dashboard -> local helper -> Claude/Codex callback -> issuing server; sign-in again; origin validation. Screenshots: ${resolve(out)}`);
} finally {
  await browser?.close();
  if (router && router.exitCode === null) { const exited = new Promise(accept => router.once("exit", accept)); router.kill(); await exited; }
  for (const { server } of servers) await close(server);
  await rm(temporary, { recursive: true, force: true });
}
