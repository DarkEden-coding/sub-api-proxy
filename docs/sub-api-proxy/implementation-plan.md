# Implementation plan

Recorded October 22, 2026. This plan builds on [discovery.md](discovery.md) and [validation.md](validation.md). Validation item 1, independent login sessions, was skipped by decision. The design below works with either outcome, and phase 0 includes a cheap check before credentials are deployed on both servers.

## Summary

Fork `vayungodara/cliproxy-rs` and add the project's behavior as new crates with small, named integration points in the existing server. The fork provides provider authentication, native Claude and Codex paths (including the Codex WebSocket), passive quota parsing, session extraction, and the v8 Management API. New work:

1. A thread registry that identifies threads, children, forks, and compaction generations from request metadata.
2. A routing policy that implements the per-thread rules in discovery, with automatically discovered plan capacity.
3. Account moves that use the clients' own full-history fallbacks.
4. Embedded storage and direct peer sync between power-server and the Mac.
5. A small local router on each client machine with sub-five-second server failover.
6. A CLI, a private web dashboard (the dark editorial flow view), and the routes the macOS visualizer reads.

## Repository and layout

- Create a public GitHub fork, renamed `sub-api-proxy`. Base it on upstream `81afb744` (v8.0.35 parity, one commit after the reviewed `85c7cf3`). Keep `upstream` as a remote and record the base revision in `docs/sub-api-proxy/UPSTREAM.md`. Merge upstream fixes by merging the tag, never by rebasing.
- This folder becomes the checkout. Move the current `docs/*.md` to `docs/sub-api-proxy/`. Keep `research/` and the raw `design/images/` out of version control: they contain private captures and transcripts.
- Keep upstream crates unchanged except at the integration points listed below. Each touched upstream file gets a short `// sub-api-proxy:` comment at the hook so merges stay readable.
- Leave unused providers (Gemini, xAI, Kimi, and others) compiled but unconfigured. Removing them would make upstream merges painful. Revisit only if binary size or memory measurements show a cost.

New crates:

| Crate | Responsibility |
| --- | --- |
| `sap-core` | Shared types: logical account, thread key, lineage, assignment, quota window, routing reason, hybrid logical clock (HLC). No I/O. |
| `sap-threads` | Thread identity extraction from Claude and Codex requests, the in-memory registry, lineage, and compaction-generation tracking. |
| `sap-routing` | Account eligibility, scoring, capacity learning, the decision table, and explanations for the UI. |
| `sap-store` | SQLite state database: schema, migrations, op log, bounded retention. |
| `sap-sync` | Peer protocol, change feed, merge rules, conflict records. |
| `sap-server` | Wiring into `cpa-server`: hooks, management and dashboard routes, server health endpoint, the `sap serve` entry point. |
| `sap-router` | Client-local router binary. Depends only on `sap-core`, tokio, hyper, and tokio-tungstenite, so its footprint stays small. |
| `sap-cli` | `sap` command: setup, accounts, status, threads, nodes, conflicts. Talks to the Management API through the local router. |
| `ui/sap` | Dashboard source (Svelte 5 and TypeScript with Vite, the same stack as upstream `ui/`), embedded in `sap-server` through `build.rs` like upstream's dashboard. |

The only new dependency is `rusqlite` (bundled SQLite, WAL mode) for embedded storage. HTTP, WebSocket, HMAC and the dashboard toolchain (axum, hyper, tokio-tungstenite, hmac, Svelte and Vite) are already in the workspace.

## Integration points in cliproxy-rs

| Point | Existing code | Change |
| --- | --- | --- |
| Session identity | `cpa-common/src/session.rs`, `cpa-server/src/session.rs` | After the existing extraction, call `sap_threads::identify(format, headers, body_prefix)`. Attach the result to the request context. |
| Credential selection | `cpa-server/src/scheduler.rs` (`Strategy`), `affinity.rs` | Add `Strategy::ThreadAffinity`. Its selector asks `sap_routing` for the account and records the reason. The existing strategies stay available. |
| Quota observations | `cpa-exec/src/codex_quota.rs`, `scheduler.rs` `Windows`, Claude `anthropic-ratelimit-unified-*` parsing | Forward each observation to `sap_routing` and `sap_store`. Parsing stays where it is. |
| Upstream errors | Claude and Codex executors, `cpa-server/src/errors.rs` | Classify rate-limit, overload, exhaustion, and revocation outcomes into `sap_core::Outcome` for the decision table. |
| Codex WebSocket | `cpa-server/src/websocket.rs`, `cpa-exec/src/codex_ws.rs` | Let the registry observe `response.create` frames. Allow the server to end a client socket deliberately when the thread's account must change. |
| Management routes | `cpa-server/src/management/` | Mount `/v8/management/routing/reset-aware/*`, `/sap/api/*`, `/sap/sync/*`, `/sap/health`, and the dashboard under `/sap/`. |

## Data model

The thread key is the pair (provider, thread id):

- **Claude Code:** `X-Claude-Code-Session-Id`, or `<session>/<agent-id>` when `x-claude-code-agent-id` is present.
- **Codex:** the `thread-id` header.

Records:

- **Logical account** (replicated): id, provider, label, email, plan type, capacity estimate and its source (discovered, learned, or overridden), priority, disabled flag, model entitlements, created by node, HLC version per field.
- **Node credential** (local only, never replicated): logical account id, node id, credential file reference, refresh state, last refresh error.
- **Thread** (replicated): key, client, client machine, parent key, lineage kind (`root`, `subagent`, `fork`), assigned account, assignment reason and time, compaction generation, title when known, first seen and last seen, serving node, HLC version.
- **Quota observation** (replicated, append-only): account, window (`5h`, `weekly`, others reported by the provider), used percent, reset time, source (`passive` or `probe`), observing node, node sequence number.
- **Usage bucket** (replicated per node): account, node, bucket start, success, failed, input, cache read, cache write, and output tokens. Totals sum across nodes, so mirrored records are never counted twice.
- **Event** (replicated, bounded): routing moves, compactions, failovers, sync conflicts, account state changes.
- **Conflict** (replicated): record, field, both values with node and HLC, and resolution state.

Live sockets, in-flight requests, and provider prompt caches are not state. Full transcripts are not stored.

## Thread identity and lineage

Implementation of the findings in [validation.md](validation.md):

- **Request class.** Claude `x-claude-code-request-class` values `main`, `subagent`, and `compaction` belong to the thread. `auxiliary` requests (titles, probes) use the thread's account without creating a thread. Codex `request_kind` values `prewarm`, `turn`, and `compaction` belong to the thread.
- **Subagents.**
  - Claude: the parent is the session thread, and the child key adds `agent-id`.
  - Codex: `x-codex-turn-metadata.parent_thread_id`, with `thread_source: subagent`.
- **Forks.**
  - Codex: `forked_from_thread_id`.
  - Claude: a new session whose first `diagnostics.previous_message_id` matches a message ID that the registry recorded for another thread. Record the last response message ID per thread for this lookup only.
- **Compaction generation.**
  - Codex: the window number in `x-codex-window-id`.
  - Claude: increment on each `request-class: compaction` request that completes successfully.
- **Active threads** have a request in flight or a request within a configurable idle window (default 10 minutes). Only active threads appear in the flow view and count toward load.
- **Machine.** The local router adds `X-Sap-Client-Host` and strips it before upstream forwarding. Direct requests without a router show `unknown`.

Avoid parsing whole request bodies. Read headers first. For Claude bodies, scan only the top-level `thread`, `diagnostics`, and `metadata` fields with a borrowing parser over the raw bytes (`serde_json::value::RawValue` for skipped fields), then pass the original bytes through unchanged.

## Routing policy

The decision table from discovery is the specification. Implementation details:

**Eligibility.** An account is eligible when it is enabled, its provider matches the request, it is entitled to the requested model, it has a usable credential on the serving node, and no confirmed exhaustion covers the current time.

**Scoring a new thread.** For each eligible account and each reported window `w`:

- `remaining_w = capacity × (1 − used_w)`
- `urgency_w = remaining_w / max(time_to_reset_w, floor)`, which favors allowance that will otherwise expire unused
- `headroom = min over windows of remaining_w`, so the tightest window limits the account

`score = urgency_weight × normalized(sum of urgency) + headroom_weight × normalized(headroom) − load_weight × active_streams_share`

Stale or missing windows reduce the score by a configurable penalty and are shown as unknown. They are never treated as zero usage. Coefficients live in config. The defaults come from replaying recorded usage history in tests.

**Capacity discovery.** When an account is added:

1. Read the plan from the provider. Codex sends `plan_type` in the `codex.rate_limits` event and in `wham/usage`. For Claude, read the profile endpoint's organization and rate tier.
2. Map the plan to an initial multiplier from a config table, for example Pro = 1, Max 5× = 5, Max 20× = 20, Plus = 1, and ChatGPT Pro from the table.
3. Refine with an exponentially weighted moving average (EWMA) of tokens consumed per percentage point of each window, from passive observations. Show the source of the estimate. Accept a manual override.

**Existing threads.** Follow the table:

- Continuations keep their account.
- Temporary limits retry on the same account until a configurable deadline, then return a retryable error.
- Confirmed exhaustion, revocation, or ineligibility moves the thread.
- After a successful compaction, the thread is re-scored and moves only if another account beats the current one by a configurable margin. Children start on the parent's account when it is eligible.

**Explanations.** Every decision stores a short reason, such as `parent affinity`, `most headroom`, `moved: weekly limit`, or `kept after compaction`. The dashboard and visualizer show these instead of recomputing ranking locally.

## Moving a thread between accounts or servers

Provider-side thread state means a continuation can only be served by the account, and possibly the login, that created it. Rules:

- **Claude.** If a `thread: continue` request arrives for a thread whose account changed, or that this node cannot serve, answer HTTP 400 `invalid_request_error`. Validation showed that Claude Code then resends the full history as `thread: create`, which is then routed normally.
- **Codex.** If the account must change while a client WebSocket is open, finish the current response if one is streaming, then end the client socket with a normal close. If phase 0 shows that Codex does not reconnect with full input after a close, send a `response.failed` error event first, or fall back to failing the turn with a clear error. For HTTP requests that carry an unknown `previous_response_id`, return the error the provider uses for that case.
- **Server failover.** Both rules make the backup server safe. A continuation that arrives at a node without its upstream state is forced into a full resend. Phase 0 checks whether one logical account's server-side thread works across both nodes' logins. If it does, no forced resend is needed when only the node changes.

## Storage and synchronization

**Storage.** One SQLite database per node (`~/.local/share/sub-api-proxy/state.db`, mode 0600, WAL), plus the existing cliproxy credential directory for node credentials.

- **Memory.** The thread registry, the latest quota windows per account, and active streams. Bounded by entry count and bytes. The oldest inactive threads are evicted first.
- **Disk.** The op log, quota history, usage buckets, and events, with configurable retention (defaults: 14 days of quota history, 90 days of hourly usage, 7 days of events) and a total byte budget enforced by compaction.

Because both clients resend full context when asked, recovery data does not need conversation content. When budgets are exhausted, the oldest inactive thread records are dropped. A dropped thread that returns is forced into a full resend and re-assigned, which matches the explicit-recovery behavior in discovery.

**Sync protocol.** Peer HTTP over Tailscale between configured peers. No fixed limit on peer count.

- Each node keeps an op log with a per-node sequence number. A change is (record key, field, value, HLC, node).
- Peers pull with `GET /sap/sync/changes?since=<seq per node>` and hold a long-poll open for push notification, so normal lag stays under a second.
- Authenticate with a per-peer secret (HMAC over method, path, timestamp, and body hash). Allow peers only on Tailscale addresses.

**Merge rules:**

| Data | Rule |
| --- | --- |
| Account fields, settings, client access config | Last writer wins per field by HLC. Concurrent edits to the same field from different nodes during a split create a conflict record. The newer value applies provisionally, and the dashboard asks for a choice. |
| Thread assignment | Last writer wins by HLC. A divergent assignment after a split is recorded as an event, not a conflict, because the next request settles it. |
| Quota observations, usage buckets, events | Union by (node, sequence). Usage totals add per-node counters. |
| Node credentials | Never synced. Adding an account on one node shows `login needed on <node>` on the others. |

**If independent logins fail.** If phase 0 shows that two logins for one subscription interfere, switch that provider to a single refresh owner: one node owns refreshes and sends fresh access tokens to its peer over the authenticated sync channel, kept in memory only and never in the op log. During a split, the node without the owner uses its last access token until it expires. Ship both modes behind a per-provider setting.

## Local router and failover

`sap-router` runs on every client machine through launchd or systemd and listens on `127.0.0.1:8317`. Clients use only that address.

- **Server selection.** Use an ordered server list from config (power-server, then the Mac) that peers can update through sync. Probe `GET /sap/health` on every server every second with a 1.5 s timeout. The health response includes node state, upstream reachability, and the sync position.
- **State machine.** Each server is `healthy`, `suspect` (one failed probe or a connection error), or `down` (two consecutive failures, within about 3 s). Recovery requires a stable health period (default 30 s) before new requests return to the preferred server. Existing streams finish where they are.
- **Silent failures.** Server health comes from probes, not from stream silence, so a model that is reasoning without output is never mistaken for a dead server. When a server goes `down`, the router ends its open streams to that server so the clients retry.
- **Request classification:**
  - Connection refused, or no TCP or TLS session: not sent. Retry immediately on the next server.
  - Request sent but no response headers: ambiguous. Return the provider's retryable overload error so the client retries through the router, never a silent replay.
  - Output already delivered: end the stream, and the client retries the turn.
- **WebSocket.** Proxy each client WebSocket to one server connection. If that server goes down, close the client socket so Codex reconnects through the router to the backup.
- **Management and visualizer traffic** also goes through the router, which gives the visualizer the same failover path as inference.

Target: a new request reaches the backup under five seconds after a failure. Test both immediate failures and black holes.

## Client integration

`sap setup` writes supported client configuration and backs up each file first:

- **Claude Code:** `ANTHROPIC_BASE_URL=http://127.0.0.1:8317` and a client key in `~/.claude/settings.json`.
- **Codex:** a configuration that keeps ChatGPT-mode semantics (`chatgpt_base_url`, or a model provider with WebSocket support), chosen by the phase 0 check.
- **Hooks:** optional. They are installed only as a diagnostic cross-check (`sap setup --hooks`) and post to the router. Routing does not depend on them.

Phase 0 must confirm that each client keeps the same request features through a custom base URL: thread beta, session and agent headers, remote compaction, rate-limit events. If a client drops features through its supported settings, the fallback is a launch wrapper that sets `HTTPS_PROXY` to the router, with a locally trusted certificate that is limited to the provider hosts.

## Management, CLI, and visualizer

- **Management API** (`/sap/api`, management key, Tailscale only): accounts (add with a login URL, disable, priority, plan override), threads (active list, history, manual reassignment), nodes (health, failovers, sync lag), conflicts (list, resolve), events (stream over SSE), and settings.
- **CLI:** `sap login claude|codex`, `sap accounts`, `sap status`, `sap threads`, `sap nodes`, `sap conflicts resolve`, `sap setup`, `sap doctor` (checks client config, router health, and both servers).
- **Visualizer compatibility:** implement the four `reset-aware` routes with the contracts in `UsageCore/Models.swift`: snake_case keys, RFC 3339 dates, providers `claude` and `codex`, `weight` and `share_percent` from the real selector, windows with `source` and `observed_at`, history samples, and request buckets. App changes:
  1. Replace the hardcoded `power-server` URL with the local router address from configuration.
  2. Show server-provided routing reasons and weights instead of the local ranking in `Routing.swift`, and update `RoutingTests`.
- **Contract tests:** decode the server's JSON with the app's `UsageCore` decoder in a Swift test fed by fixtures that the Rust tests generate.

## Dashboard

Served by `sap-server` at `/sap/` and available only through the private deployment. Built from `ui/sap` and embedded in the binary. It gets live updates over SSE. Agreed design:

- **Look.** Dark only, desktop only, editorial style (`design/mock-flow-editorial.html`): warm charcoal background, serif headings, muted terracotta for Claude and sage for OpenAI, thin rules.
- **Flow view (home page).**
  - Threads on the left, grouped by client and then by machine. Groups expand and collapse, and children (subagents, forks) are nested under their parent. Only active threads are shown.
  - Servers in the middle and accounts on the right, connected by lines whose thickness follows the number of threads.
  - Idle and exhausted accounts stay in the view, greyed out, with their reset times.
  - Selecting a thread highlights its path and shows its detail panel: parent, account, reason, serving node, cache hit, compactions.
- **Other pages:**
  - Accounts: add and login, controls, quota history charts.
  - Threads: search and history, reassignment.
  - Nodes: health, failovers, sync lag, conflicts.
  - Events and Settings.

## Testing and measurement

Keep the test suite small and behavior-focused, following the coding preferences:

- **Routing scenarios:** one integration test per row of the decision table, with mock upstreams from `codex_testkit` and the Claude fixtures. Unit tests only for scoring and capacity learning.
- **Client fixtures:** the identity cases captured in `research/capture` (new, resume, fork, subagent, manual and automatic compaction), stored as sanitized request heads without content, and asserted against `sap_threads::identify`.
- **Replay corpus:** a local-only generator that rebuilds realistic large request bodies from `~/.claude/projects` (`api-request`, `api-request-blob`, `api-request-shape` records) and `~/.codex/sessions`, for memory and latency benchmarks. The output stays private and gitignored.
- **Failover:** process kill, refused port, black hole (pfctl on macOS, nftables on Linux), upstream-only failure, an active stream interrupted, recovery, and failback. Measure time to the first successful request on the backup.
- **Sync:** two in-process nodes with an injectable partition. Concurrent account edits, routing changes, stale quota, account disablement during a split, and reconnect convergence, with usage counts checked for double counting.
- **Performance:** extend `bench/` with the replay corpus. Record idle, peak, and retained RSS, CPU, and proxy-added latency at increasing account counts and concurrency on macOS and Linux. Set budgets from the first baseline, then gate regressions in CI.

No test calls a real model. Live checks that need a provider (phase 0) run only with explicit approval and use account and usage endpoints where possible.

## Phases

Each phase ends with working, tested software.

### Phase 0: Fork, build, and cheap checks

- Create the fork, CI on macOS and Linux (aarch64 and x86_64; confirm power-server's architecture when it is online), the `UPSTREAM.md` record, and workspace placeholders for the new crates.
- Using the mock, check each client's behavior through a custom base URL and pick the Codex configuration.
- Using the mock, check Codex's reaction to a clean WebSocket close and to a `response.failed` event.
- With approval, check the remaining provider questions:
  - Do two logins for one account stay valid when each refreshes on its own? This is the skipped test 1, and it uses only refresh and usage endpoints.
  - Can a Claude server-side thread be continued from the second login of the same account? This needs one small inference request.
- **Exit:** the fork builds and passes upstream tests in CI, and the client configuration choice and the refresh mode are recorded in `validation.md`.

### Phase 1: Thread registry

- `sap-core` types and `sap-threads` extraction and registry, wired into session identity, including the `X-Sap-Client-Host` handling.
- **Exit:** identity fixtures pass, and the dashboard data endpoint lists live threads with lineage against the mock.

### Phase 2: Quota discovery and routing

- Plan discovery and capacity learning, the scoring, the decision table, `Strategy::ThreadAffinity`, explanations, and outcome classification.
- **Exit:** every decision-table scenario passes against mock upstreams, and `reset-aware/status` reports the real selector's weights.

### Phase 3: Account moves

- The forced full resend for Claude, socket handling for Codex, and the retry deadline for temporary limits.
- **Exit:** move scenarios (exhaustion, revocation, compaction re-score) succeed with the real clients against the mock, without losing a turn.

### Phase 4: Storage and peer sync

- `sap-store` schema and retention, `sap-sync` protocol and merge rules, conflict records, the credential-mode switch, and the node credential login flow.
- **Exit:** the partition test suite passes, and two local nodes converge within a second when connected.

### Phase 5: Local router and failover

- `sap-router`, health probes, the state machine, request classification, WebSocket handling, and the launchd and systemd units.
- **Exit:** the failover suite meets the five-second target for new requests on macOS and Linux.

### Phase 6: Management API, CLI, and visualizer

- `/sap/api`, the `sap` CLI including `setup` and `doctor`, the reset-aware routes, the visualizer app changes, and contract tests.
- **Exit:** the visualizer runs against the new server through the router and shows real routing reasons.

### Phase 7: Dashboard

- Flow view as designed, then the Accounts, Threads, Nodes, Events, and Settings pages, with SSE updates.
- **Exit:** every management action in the CLI is also available in the dashboard, and the flow view stays correct with many active threads.

### Phase 8: Hardening and cutover

- Benchmarks, a 24-hour soak with the replay corpus, the failover and sync suites on the real machines, and the install scripts.
- Install on power-server (replacing the old server) and the Mac, run `sap setup` on each client, and remove the old configuration after a successful day of use.
- **Exit:** a day of normal use with no lost turns, recorded resource baselines, and a working failover drill.

## Risks

| Risk | Mitigation |
| --- | --- |
| Logins for one subscription interfere (test 1 skipped) | Single refresh owner mode (phase 4), chosen by the phase 0 check. |
| A client drops features behind a custom base URL | Phase 0 check, with the `HTTPS_PROXY` launch wrapper as the fallback. |
| Codex does not reconnect cleanly after a forced close | Phase 0 check, with an error event or a failed turn as the fallback. |
| A server-side thread cannot continue from another login | Forced full resend on node change. This costs one full-context request and its cache. |
| Client protocols change with updates | The identity fixtures run in CI, and `sap doctor` warns when an untested client version is seen. |
| Upstream merges conflict with the integration points | Changes stay in new crates, and the hooks are small and marked. |
| Provider terms | You have accepted this risk for single-person use. Recorded here for completeness. |
