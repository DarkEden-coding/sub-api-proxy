# Parallel work: contracts and briefs

Six workstreams run in parallel, each in its own git worktree and branch. They share one contract: the `sap-core` crate. Read [implementation-plan.md](implementation-plan.md) and [validation.md](validation.md) first.

## Rules for every workstream

1. **Stay in your area.** Edit only the paths listed in your brief. Do not edit `crates/sap-core`, upstream `cpa-*` crates, `Cargo.lock` beyond what your own crate's dependencies require, or another workstream's crate.
2. **The contract is `sap-core`.** Use its types for everything that crosses your crate's boundary. If the contract is missing something, keep a private type in your crate, make it work, and list the needed `sap-core` change under "Contract requests" in your final report. The integrator applies contract changes.
3. **Shared fixtures** live in `crates/sap-core/tests/fixtures/`:
   - `flow.json`: a full `api::FlowSnapshot` (dashboard sample data).
   - `identity.jsonl`: sanitized request heads captured from Claude Code 2.1.308 and Codex 0.163.0 against a mock, one JSON object per line: `scenario`, `provider`, `transport` (`http` or `ws`), `headers` (lowercased names), `body` (top-level fields only; conversation content replaced by `"<omitted>"`). For `ws` lines, `headers` are the WebSocket upgrade request headers and `body` is one `response.create` frame.
4. **Follow [AGENTS.md](../../AGENTS.md)**:
   - Code: no `str::to_lowercase` or `to_uppercase` (clippy disallows them; use ASCII variants where the input is ASCII by definition), request bytes forwarded unchanged, linear-time body handling, no secrets in logs.
   - Tests: tests never touch the network beyond localhost or a real `auth-dir`.
   - `ponytail:` comments mark deliberate simplifications.
5. **Coding preferences:**
   - Small modules with clear responsibilities.
   - Reuse what the workspace already has.
   - A few behavior and integration tests for the important workflows, and unit tests only for complex logic.
   - No speculative abstractions.
6. **Build only your crate.** Never build the whole workspace: BoringSSL and the release link are slow. Use `CARGO_BUILD_JOBS=4`, then:
   - `cargo fmt -p <crate>`
   - `cargo clippy -p <crate> --all-targets -- -D warnings`
   - `cargo test -p <crate>`
7. **No real provider traffic.** Never call real LLMs or provider endpoints.
8. **Finish cleanly.** Commit on your branch with clear messages, and do not push. Your final report covers:
   - what is done and what is not
   - the public API you expose, with signatures
   - how to run the tests and their results
   - contract requests
   - known gaps

## Contract summary (`crates/sap-core/src`)

| Module | Key items | Producer, consumer |
| --- | --- | --- |
| `ids` | `Provider`, `NodeId`, `AccountId`, `ThreadKey` (`"claude:<id>"`) | all |
| `thread` | `ThreadIdentity`, `RequestClass`, `LineageKind`, `Continuation`, `ThreadRecord`, `Assignment`, `ThreadUsage` | sap-threads → sap-server, sap-routing, sap-store |
| `account` | `Account`, `Capacity`, `AccountRuntime`, `NodeCredential` | sap-server → sap-routing; sap-store |
| `quota` | `QuotaWindow`, `WindowKind`, `QuotaObservation` | executors → sap-routing, sap-store |
| `routing` | `RoutingPolicy` trait, `RoutingInput`, `RoutingEvent`, `Outcome`, `Decision`, `Reason`, `AccountScore`, `RoutingConfig` | sap-routing implements |
| `hlc`, `sync` | `Hlc`, `Change`, `ChangeOp`, `Table`, `SyncCursor`, `ChangeBatch`, `Conflict`, `ChangeLog` trait | sap-store implements `ChangeLog`; sap-sync transports |
| `health` | `HealthReport`, `HEALTH_PATH`, `CLIENT_HOST_HEADER` | sap-server serves; sap-router reads |
| `api` | `FlowSnapshot`, `AccountView`, `ThreadView`, `NodeView`, `EventView`, request bodies, route table | sap-server serves; `ui/sap` reads |
| `reset_aware` | visualizer response types | sap-server serves |

## Workstream A: `sap-threads` (backend)

**Paths:** `crates/sap-threads/**`.

**Build:**

1. Identification functions:
   - `identify_claude(headers: &http::HeaderMap, body: &[u8]) -> Option<ThreadIdentity>`, for `POST /v1/messages`.
   - `identify_codex(headers: &http::HeaderMap, body: &[u8]) -> Option<ThreadIdentity>`. Here `headers` are the HTTP request or WebSocket upgrade headers, and `body` is the HTTP body or one `response.create` frame.
   - A dispatcher `identify(path: &str, headers, body)`.
2. Parse only the top-level fields you need in one linear pass. For example, deserialize into a struct of `Option<&RawValue>` and borrowed fields, with skipped fields ignored. Never re-serialize the body.
3. Claude rules:
   - The thread is `x-claude-code-session-id`, falling back to `metadata.user_id.session_id`.
   - A subagent key is `<session>/<x-claude-code-agent-id>`. Its parent is the session thread, and `agent_label` is `x-claude-code-agent-type`.
   - Classes come from `x-claude-code-request-class`: `main` → `Turn`, `subagent` → `SubagentTurn`, `compaction` → `Compaction` (trigger from `x-claude-code-compaction`), `auxiliary` → `Auxiliary`.
   - `thread.type == "continue"` → `Continuation::Continue { previous_id: thread.previous_message_id }`. Otherwise `FullHistory`.
   - `previous_message_id` comes from `diagnostics.previous_message_id`, and `turn_id` from `x-claude-code-prompt-id`.
4. Codex rules:
   - The thread is the `thread-id` header and `root_session` is `session-id`. Read `x-codex-turn-metadata` (a JSON header) or the frame's `client_metadata["x-codex-turn-metadata"]`.
   - Lineage: `parent_thread_id` and `thread_source == "subagent"` → `Subagent`. `forked_from_thread_id` → `Fork`.
   - Classes from `request_kind`: `prewarm` → `Prewarm`, `turn` → `Turn` (or `SubagentTurn` on a subagent thread), `compaction` → `Compaction` (trigger from `compaction.trigger`).
   - `window_number` comes from the `x-codex-window-id` suffix, and `agent_label` from `agent_name`.
   - `previous_response_id` non-null → `Continue`.
5. Both: `client_host` comes from `sap_core::health::CLIENT_HOST_HEADER`. The model comes from the body `model`. `ClientKind` comes from user-agent and the markers above.
6. `Registry`, in memory and bounded by a configurable maximum number of records (evict the least recently seen inactive threads first):
   - `observe(&ThreadIdentity, now, node) -> Observation`, where the result has the record, whether it is new, and the resolved lineage.
   - Claude fork resolution: a new root session whose `previous_message_id` equals another thread's `last_response_id` becomes a `Fork` of that thread.
   - `record_response(key, response_id)`, `record_usage(key, ThreadUsage delta)`, `stream_opened` / `stream_closed`.
   - `compaction_completed(key)`, which increments the generation and returns it (Codex can also infer it from `window_number` increasing).
   - `assign(key, Assignment)`, `get`, `active(now, idle_window) -> Vec<ThreadRecord>` (in flight or seen within the window), `children(key)`.
   - `export() -> Vec<ThreadRecord>` and `import(Vec<ThreadRecord>)` for the store.
7. Tests: a fixture test that runs every line of `identity.jsonl` and asserts per scenario:
   - `claude:new`: one auxiliary request, a main `FullHistory` turn and then `Continue` turns, and a subagent request whose key ends in `/<agent-id>` with the parent set.
   - `claude:fork`: a new session whose `previous_message_id` matches the earlier thread's last message.
   - `claude:compact`: `Compaction`, trigger `Manual`.
   - `codex:new`: a root thread with prewarm and then turns, and a subagent thread with `parent_thread_id`.
   - `codex:fork`: `forked_from_thread_id`.
   - `codex:compact`: `Compaction`, trigger `Auto`.
   - Resume scenarios keep the same key.
   - Plus registry behavior tests (fork resolution, active window, eviction bound). Fork resolution needs response IDs: simulate them in the test.

## Workstream B: `sap-routing` (backend)

**Paths:** `crates/sap-routing/**`.

**Build** `ThreadAffinityPolicy::new(RoutingConfig)`, implementing `sap_core::routing::RoutingPolicy`:

- **Eligibility:** the account is enabled and not `revoked`, `credential_usable`, `serves_model`, not `exhausted_until` in the future, and its provider matches. `cooling_until` in the future makes an account ineligible for new threads, but the decision table governs current threads.
- **Score per window `w`:**
  - `remaining_w = capacity × (1 − used_w/100)`
  - `urgency_w = remaining_w / max(seconds_to_reset_w, reset_floor_secs)`
  - `headroom = min_w remaining_w`
  - `score = urgency_weight·norm(Σ urgency) + headroom_weight·norm(headroom) − load_weight·share(active_threads)`, normalized across eligible accounts. Missing or stale windows (older than `stale_after_secs`) multiply by `stale_penalty` and set `stale`.
  - Ties are broken by `priority`, then by account ID.
  - `weight_percent` is each eligible account's share of the positive scores. Ineligible accounts get 0 and a reason.
- **Decision table:**

  | Event | Decision |
  | --- | --- |
  | `NewThread` | best score, `Reason::BestScore` with the deciding `ScoreDetail` |
  | `ChildStart` | parent account if eligible (`ParentAffinity`), else best score (`ParentUnavailable`) |
  | `Continue` | keep (`Kept`) while eligible; if exhausted, revoked, or unable to serve the model, move (`MovedExhausted` or `MovedIneligible`) |
  | `AfterOutcome` `RateLimited`/`Overloaded` | `RetrySame` until `first_failure_at + retry_deadline_secs`, then `Reject { retryable: true }` and keep the assignment |
  | `AfterOutcome` `Exhausted` | move to the best other eligible account (`MovedExhausted`) |
  | `AfterOutcome` `Revoked`/`ModelIneligible` | move (`MovedIneligible`) |
  | `CompactionCompleted` | move only if the best other score exceeds the current one by `compaction_move_margin` (`MovedAfterCompaction`), else `KeptAfterCompaction` |
  | no eligible account | `Reject { retryable: true, earliest_usable_at }`: the earliest known reset among accounts that serve the model, or `None` if none is known. Never invent one. |

- **Capacity:**
  - `plan_capacity(plan: &str, cfg) -> Option<f64>`, an ASCII case-insensitive substring match in table order.
  - `CapacityLearner`, an EWMA of tokens per percentage point from successive observations of the same window when its reset did not change. It returns a `Capacity { source: Learned }` once it has enough samples, relative to a configurable baseline of tokens per percent for multiplier 1.
- **Tests:** one test per table row, plus unit tests for scoring edge cases (stale data, different plan sizes with the same percentage, a reset that is soon versus later, load) and for the learner.

## Workstream C: `sap-store` (backend)

**Paths:** `crates/sap-store/**`.

**Build** `Store::open(path, node: NodeId, limits: Limits)` and `Store::open_in_memory(node)` on rusqlite (WAL mode, file mode 0600):

- **Schema and migrations** for accounts, settings, client_access, threads, quota_observations, usage_buckets, events, conflicts, node_credentials (local only, never in the op log), the op log, and the cursor.
- **Domain API.** Each write appends `Change`s with a fresh HLC and per-node `seq`:
  - `upsert_account` / `patch_account(id, field changes)` / `accounts()`
  - `put_thread(ThreadRecord)` / `threads()` / `delete_thread`
  - `append_quota(QuotaObservation)`, `latest_windows(account)`, `quota_history(account, since)`
  - `add_usage(account, bucket_start, deltas)`. The bucket is per node; totals sum across nodes.
  - `append_event(EventView)` / `events(limit)`
  - `conflicts()`, `resolve_conflict(id, keep: NodeId)`
  - `get_setting` / `set_setting`
  - `put_credential` / `credentials()`
- **`ChangeLog` implementation** with the merge rules in `sap_core::sync` docs:
  - Field-level last writer wins by HLC.
  - A `Conflict` when two different origins changed the same field and neither change had seen the other. Track for each field the HLC and origin of its last write, and the sender's cursor at write time. An apply from a peer whose change did not observe the local write is concurrent.
  - Thread last writer wins with no conflict.
  - Append tables deduplicated by `(origin, seq)`.
  - Tombstones.
  - `apply_remote` is idempotent and observes remote HLCs.
- **Retention:** row-age limits per table and a total byte budget, enforced by `compact()`, which deletes the oldest append rows and op-log entries already acknowledged by all known peers (track peer cursors with `set_peer_cursor`).
- **Tests:** two in-memory stores exchanging `changes_since` and `apply_remote`:
  - convergence
  - a concurrent edit making a conflict
  - a non-concurrent edit making none
  - append deduplication
  - usage summed without double counting
  - tombstones
  - retention

## Workstream D: `sap-sync` (backend)

**Paths:** `crates/sap-sync/**`.

**Build** the transport, generic over `C: sap_core::sync::ChangeLog`:

- **Server side:** `router(state) -> axum::Router` serving:
  - `GET /sap/sync/changes?since=<json cursor>&limit=N&wait_ms=M`. It long-polls until changes exist or `wait_ms` elapses, then returns a `ChangeBatch`.
  - `POST /sap/sync/ack` with the peer's cursor.
- **Authentication:** HMAC-SHA256 with a per-peer secret over `method\npath_and_query\ntimestamp\nsha256(body)`, in headers `x-sap-peer`, `x-sap-timestamp` and `x-sap-signature`. Reject skew over 60 s.
- **Client side:** `SyncAgent::spawn(log, peers, notify)`. For each peer it runs a pull loop (with long-poll, backoff on errors, and a cap) and applies batches. `notify` is a `tokio::sync::Notify` that local writers trigger to wake waiting long-polls.
- **Status:** `PeerStatus { last_success, last_error, lag_secs, cursor }` per peer, for the Nodes page.
- **Tests:** use a simple in-memory `ChangeLog` test double inside your tests (sap-store is built in parallel):
  - two nodes on localhost converge in under 1 s
  - a partition (stopping one server) and reconnect converge
  - a bad signature is rejected
  - long-poll wakes on notify

## Workstream E: `sap-router` (backend)

**Paths:** `crates/sap-router/**`. This is a binary crate; split it into modules and keep `main.rs` thin.

**Build:**

- **Config:** YAML (`--config`, default `~/.config/sub-api-proxy/router.yaml`) with:
  - `listen` (default `127.0.0.1:8317`)
  - `client_host` (default: the hostname)
  - `servers` (an ordered list of `{ name, base_url }`)
  - `probe_interval_ms` (1000), `probe_timeout_ms` (1500), `down_after_failures` (2)
  - `stable_secs` (30)
  - `connect_timeout_ms` (1000)
- **Health:**
  - Probe `GET {base_url}/sap/health` (`sap_core::health::HealthReport`).
  - Each server moves between `Healthy`, `Suspect` and `Down`. Only a `Serving` report counts as healthy.
  - The preferred server is the first `Healthy` server in list order. A recovered earlier server becomes preferred again only after `stable_secs` of continuous health.
- **Proxying:**
  - Stream request and response bodies without buffering, keep bytes unchanged, add the `x-sap-client-host` header, and pass `Authorization` through.
  - Support WebSocket upgrade passthrough, for example for `GET /backend-api/codex/responses`, relaying frames in both directions.
- **Failover classification:**
  - Connection refused, or no connection within `connect_timeout_ms`: retry the next healthy server. This is safe because the request was never sent.
  - Request sent but no response headers when the server goes `Down`: answer `503` with a JSON error body `{"type":"error","error":{"type":"overloaded_error","message":"..."}}` so clients retry.
  - Streaming when the server goes `Down`: end the response stream or close the WebSocket so the client retries.
- **Local status:** `GET /sap/router/status` (served locally, never proxied) with each server's state and the current preferred server.
- **Tests:** fake servers in-process on localhost:
  - normal proxy, including streaming
  - kill: a new request reaches the backup in under 5 s
  - refused port
  - black hole: the server accepts but never answers health or requests; it is detected within about 3 s, and open streams to it end
  - recovery with the stable period
  - WebSocket passthrough, and close on down

## Workstream F: dashboard `ui/sap` (frontend and visual)

**Paths:** `ui/sap/**` only. Do not touch upstream `ui/src` or `ui/package.json`.

**Build** a separate Vite app with Svelte 5 and TypeScript, the same stack as upstream `ui/` (`ui/package.json`), with its own `package.json`:

- **Types:** `src/api/types.ts` mirrors `crates/sap-core/src/api.rs` exactly (snake_case keys). Also `WindowKind`, `ClientKind`, `LineageKind`, `NodeRole`, `NodeState` and `CapacitySource`, as they serialize from `sap-core`. Read the Rust source to get the JSON shapes right.
- **Data client:** `src/api/client.ts` with `fetchFlow()`, `subscribe(onSnapshot)` over SSE `GET /sap/api/stream`, and the account, thread and conflict actions from the route table in `api.rs`, with a bearer management key kept in `localStorage`. A dev mode (`?demo=1`, or the API unreachable during `vite dev`) uses `crates/sap-core/tests/fixtures/flow.json`.
- **Design:** follow `docs/sub-api-proxy/design/mock-flow-editorial.html` closely. Dark only, desktop only, warm editorial: charcoal background, serif headings, terracotta for Claude, sage for OpenAI, thin rules. Port its CSS variables.
- **Flow page (home):**
  - Threads on the left, grouped by client and then by `machine`, with collapsible groups.
  - Children (subagents, forks) nested under their parent via `parent`. Only active threads appear (the server sends only active ones).
  - Servers in the middle and accounts on the right, with SVG connection lines whose thickness follows thread count. The line drawing in the mock is the reference.
  - **Idle, exhausted, cooling, disabled and login-needed accounts stay in the view, greyed out, with their reset or available-at time.**
  - Selecting a thread highlights its path and fills a detail panel. Below: the requests and cache chart from `stats.series`, and recent events.
- **Other pages:**
  - Accounts: list, details, disable or enable, priority, capacity override, and add-account login, which opens `LoginStart.url`.
  - Threads: an active list with search, and a reassign action.
  - Nodes: health, failovers, sync lag, and the conflict list with resolve.
  - Events.
  - Settings: the management key field.
- **Delivery:**
  - `npm run build` outputs `ui/sap/dist` (ignored by git). Base path `/sap/`.
  - Tests use `node --test`, as upstream does, for data shaping: grouping by client and machine, the parent and child tree, and the account greying logic.
  - Check visually with the T3 `html_preview` tool or a headless browser screenshot of `vite preview` with `?demo=1`. Include screenshots or their paths in the final report.

## Workstream G: hook points in `cpa-server` (backend)

**Paths:** a new `crates/cpa-server/src/sap_hooks.rs`, minimal edits at the call sites below, and `crates/cpa-server/Cargo.toml` (add `sap-core`). No other sap crate may be a dependency of `cpa-server`: the real hook implementation lives in a later `sap-server` crate and is installed at startup.

**Goal:** a trait object on `Runtime` that Sub-Api-Proxy uses to identify threads, choose credentials, and observe results. With no hooks installed, behavior and bytes are identical to upstream, so the existing tests of touched modules must keep passing.

**Findings to build on:**
- `Runtime::acquire` (`runtime.rs`) already supports an external choice: `plugins::scheduler_request` / `scheduler_pick` produce `Pick::Auth(credential_id)`, `Pick::Configured`, or `AcquireError::Rejected`, and `select_picked` honors it. Reuse that path: the hook's answer becomes a `Pick`.
- `dispatch.rs` builds the `Selection` (around the `scheduler_context` call) where the request's headers and body are available.
- `websocket.rs` handles Codex `response.create` frames on `/backend-api/codex/responses`.
- `usage_record.rs` parses usage from Claude (`message_start`, `message_delta`) and Codex (`response.completed`) streams.
- The executors' quota stores (`executors.claude.quota()`, `executors.codex.quota()`) hold the passive quota snapshots.

**Trait** (adjust names if the code suggests better ones, and keep it small):

```rust
pub trait SapHooks: Send + Sync {
    /// Once per inference request: an HTTP body, or each Codex `response.create` frame.
    /// The bytes are the original, unmodified request.
    fn identify(&self, path: &str, headers: &http::HeaderMap, body: &[u8]) -> Option<Arc<SapRequest>>;
    /// Choose among the ready candidates (credential id, provider, priority).
    fn select(&self, request: &SapRequest, candidates: &[SapCandidate]) -> SapPick;
    /// The attempt on `credential` finished; `status` and the classified error, if any.
    fn on_outcome(&self, request: &SapRequest, credential: &str, result: &AttemptResult);
    /// Stream observations: response or message IDs, usage, and quota signals.
    fn on_observation(&self, request: &SapRequest, credential: &str, observation: Observation);
    fn on_stream(&self, request: &SapRequest, open: bool);
}
pub enum SapPick {
    Configured,
    Credential(String),
    /// Reply without calling upstream: exact status, content type and body bytes
    /// (for example the Claude 400 that forces a full-history resend).
    Reply { status: u16, content_type: &'static str, body: bytes::Bytes },
    /// Codex WebSocket only: finish the current response, then close the client socket normally.
    CloseSocket,
}
```

`SapRequest` wraps `sap_core::thread::ThreadIdentity` plus a `Box<dyn Any + Send + Sync>` slot for the implementation.

**Also:**
- `Runtime::set_sap_hooks(Arc<dyn SapHooks>)`.
- Strip `sap_core::health::CLIENT_HOST_HEADER` from upstream requests after `identify` has seen it.
- Serve `GET /sap/health` only when hooks are installed, through a hook method returning `HealthReport`.

**Tests:** a fake hooks implementation in `cpa-server` tests, using existing test helpers (`cpa_server::testing::runtime`, mock upstreams, private `auth-dir`). Cover:
- identify sees the original bytes
- `Credential` pins the choice
- `Reply` sends exact bytes and makes no upstream call
- outcome and observation callbacks fire with usage and the response ID for Claude and Codex
- the client host header never reaches upstream
- WebSocket frames are identified per frame
- `CloseSocket` closes after the current response
- no hooks means no behavior change

**Build:** `cpa-server` is large, so use `CARGO_BUILD_JOBS=4 nice -n 19` and run only filtered tests (`cargo test -p cpa-server sap_hooks`), plus the existing tests of the modules you touch. Mark every upstream edit with a `// sub-api-proxy:` comment.
